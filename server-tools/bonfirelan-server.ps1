<#
.SYNOPSIS
  bonfireLAN host script: publishes the server's manifest (version, loader, mods)
  so bonfireLAN players on the LAN can detect and download the required mods.

.DESCRIPTION
  Run it on the PC that hosts the Minecraft server, next to the server folder.
  Serves (read-only):
    GET /bonfirelan/manifest          -> ServerManifest JSON (see src/types.ts)
    GET /bonfirelan/mods/<fileName>   -> the mod .jar itself (only files listed in the manifest)

  The manifest is recomputed whenever the mods/ folder changes.

.EXAMPLE
  .\bonfirelan-server.ps1 -ServerDir "C:\mc-server"
  .\bonfirelan-server.ps1 -ServerDir . -RecommendedRam 6144 -McVersion 1.21.1
#>
param(
    [string]$ServerDir = ".",
    [int]$Port = 25580,
    [string]$Bind = "0.0.0.0",
    [int]$RecommendedRam = 4096,
    [string]$McVersion = "",
    [string]$FabricVersion = ""
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.IO.Compression.FileSystem

$ServerDir = (Resolve-Path $ServerDir).Path
$ModsDir = Join-Path $ServerDir "mods"


function Get-ServerName {
    $props = Join-Path $ServerDir "server.properties"
    if (Test-Path $props) {
        $line = Get-Content $props | Where-Object { $_ -match '^motd=' } | Select-Object -First 1
        if ($line) { return ($line -replace '^motd=', '').Trim() }
    }
    return "Minecraft Server"
}

function Get-HighestVersionDir([string]$dir) {
    if (-not (Test-Path $dir)) { return "" }
    $names = Get-ChildItem $dir -Directory | ForEach-Object { $_.Name } | Where-Object { $_ -match '^\d+\.\d+' }
    if (-not $names) { return "" }
    return ($names | Sort-Object { [version](($_ -replace '[^\d.].*$', '') -replace '\.$', '') } | Select-Object -Last 1)
}

function Get-McVersion {
    if ($McVersion) { return $McVersion }
    # 1.18+ server bundler extracts to versions/<ver>/; older layouts keep it in libraries/.
    $v = Get-HighestVersionDir (Join-Path $ServerDir "versions")
    if (-not $v) { $v = Get-HighestVersionDir (Join-Path $ServerDir "libraries\net\minecraft\server") }
    return $v
}

function Get-FabricVersion {
    if ($FabricVersion) { return $FabricVersion }
    return Get-HighestVersionDir (Join-Path $ServerDir "libraries\net\fabricmc\fabric-loader")
}

function Read-FabricModJson([string]$jarPath) {
    $zip = $null
    try {
        $zip = [System.IO.Compression.ZipFile]::OpenRead($jarPath)
        $entry = $zip.GetEntry("fabric.mod.json")
        if (-not $entry) { return $null }
        $reader = New-Object System.IO.StreamReader($entry.Open())
        try { return ($reader.ReadToEnd() | ConvertFrom-Json) } finally { $reader.Dispose() }
    } catch {
        return $null
    } finally {
        if ($zip) { $zip.Dispose() }
    }
}

function Get-ModEntries {
    if (-not (Test-Path $ModsDir)) { return @() }
    $entries = @()
    foreach ($jar in Get-ChildItem $ModsDir -Filter *.jar -File) {
        $meta = Read-FabricModJson $jar.FullName
        $name = $jar.BaseName; $version = ""; $modId = ""; $environment = "*"
        if ($meta) {
            if ($meta.name) { $name = [string]$meta.name }
            if ($meta.version) { $version = [string]$meta.version }
            if ($meta.id) { $modId = [string]$meta.id }
            if ($meta.environment) { $environment = [string]$meta.environment }
        }
        $entries += [ordered]@{
            name        = $name
            version     = $version
            hash        = (Get-FileHash $jar.FullName -Algorithm SHA1).Hash.ToLower()
            source      = "lan"
            url         = "/bonfirelan/mods/" + [uri]::EscapeDataString($jar.Name)
            fileName    = $jar.Name
            modId       = $modId
            environment = $environment
            size        = $jar.Length
        }
    }
    return $entries
}


$script:cacheKey = $null
$script:manifestJson = $null
$script:modFiles = @{}

function Get-ModsSignature {
    if (-not (Test-Path $ModsDir)) { return "none" }
    return (Get-ChildItem $ModsDir -Filter *.jar -File |
        ForEach-Object { "$($_.Name)|$($_.Length)|$($_.LastWriteTimeUtc.Ticks)" }) -join ";"
}

function Update-Manifest {
    $sig = Get-ModsSignature
    if ($sig -eq $script:cacheKey) { return }
    $mods = @(Get-ModEntries)
    $fabric = Get-FabricVersion
    $manifest = [ordered]@{
        serverName     = Get-ServerName
        minecraft      = Get-McVersion
        loader         = $(if ($fabric) { "fabric" } else { "vanilla" })
        fabricVersion  = $fabric
        recommendedRam = $RecommendedRam
        mods           = $mods
    }
    $script:manifestJson = ConvertTo-Json -InputObject $manifest -Depth 5 -Compress
    $script:modFiles = @{}
    foreach ($m in $mods) { $script:modFiles[$m.fileName] = Join-Path $ModsDir $m.fileName }
    $script:cacheKey = $sig
    Write-Host ("[{0:HH:mm:ss}] Manifest: MC {1}, loader {2} {3}, {4} mods" -f (Get-Date), $manifest.minecraft, $manifest.loader, $fabric, $mods.Count)
}

# TcpListener instead of HttpListener: no admin or URL ACL needed.

function Send-Response($stream, [int]$status, [string]$reason, [string]$contentType, [byte[]]$body, [string]$filePath) {
    $length = if ($filePath) { (Get-Item $filePath).Length } else { $body.Length }
    $head = "HTTP/1.1 $status $reason`r`nContent-Type: $contentType`r`nContent-Length: $length`r`nConnection: close`r`n`r`n"
    $headBytes = [Text.Encoding]::ASCII.GetBytes($head)
    $stream.Write($headBytes, 0, $headBytes.Length)
    if ($filePath) {
        $fs = [IO.File]::OpenRead($filePath)
        try { $fs.CopyTo($stream) } finally { $fs.Dispose() }
    } elseif ($body.Length -gt 0) {
        $stream.Write($body, 0, $body.Length)
    }
}

function Send-Text($stream, [int]$status, [string]$reason, [string]$text, [string]$contentType = "text/plain; charset=utf-8") {
    Send-Response $stream $status $reason $contentType ([Text.Encoding]::UTF8.GetBytes($text)) $null
}

function Handle-Client($client) {
    $client.ReceiveTimeout = 5000
    $stream = $client.GetStream()
    try {
        $reader = New-Object IO.StreamReader($stream, [Text.Encoding]::ASCII, $false, 1024, $true)
        $requestLine = $reader.ReadLine()
        while ($true) { $h = $reader.ReadLine(); if ([string]::IsNullOrEmpty($h)) { break } }
        if (-not $requestLine) { return }
        $parts = $requestLine.Split(' ')
        $method = $parts[0]; $path = $parts[1].Split('?')[0]

        if ($method -ne "GET") {
            Send-Text $stream 405 "Method Not Allowed" "method not allowed"
        } elseif ($path -eq "/bonfirelan/manifest") {
            Update-Manifest
            Send-Text $stream 200 "OK" $script:manifestJson "application/json; charset=utf-8"
        } elseif ($path.StartsWith("/bonfirelan/mods/")) {
            Update-Manifest
            # Only files listed in the manifest are served (no path traversal).
            $file = [uri]::UnescapeDataString($path.Substring("/bonfirelan/mods/".Length))
            if ($script:modFiles.ContainsKey($file)) {
                Send-Response $stream 200 "OK" "application/java-archive" $null $script:modFiles[$file]
                Write-Host ("[{0:HH:mm:ss}] {1} downloaded {2}" -f (Get-Date), $client.Client.RemoteEndPoint, $file)
            } else {
                Send-Text $stream 404 "Not Found" "mod not found"
            }
        } else {
            Send-Text $stream 404 "Not Found" "not found"
        }
    } catch {
        Write-Warning "Request failed: $_"
    } finally {
        $stream.Dispose(); $client.Close()
    }
}


Write-Host "bonfireLAN host - server: $ServerDir"
Update-Manifest
$listener = New-Object Net.Sockets.TcpListener([Net.IPAddress]::Parse($Bind), $Port)
$listener.Start()
$ips = [Net.Dns]::GetHostAddresses([Net.Dns]::GetHostName()) | Where-Object { $_.AddressFamily -eq 'InterNetwork' } | ForEach-Object { $_.IPAddressToString }
Write-Host "Listening on port $Port. Manifest: http://<ip>:$Port/bonfirelan/manifest  (IPs: $($ips -join ', '))"
Write-Host "Keep this window open while you play. Ctrl+C to stop."
try {
    while ($true) {
        if ($listener.Pending()) { Handle-Client $listener.AcceptTcpClient() }
        else { Start-Sleep -Milliseconds 100 }
    }
} finally {
    $listener.Stop()
}
