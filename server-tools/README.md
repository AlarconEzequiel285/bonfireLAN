# bonfireLAN host script

Run it on the PC hosting the Minecraft server. It tells everyone on the LAN which Minecraft version, Fabric version and mods the server has, and serves the `.jar`s so bonfireLAN can download them for the players.

## Usage

```powershell
powershell -ExecutionPolicy Bypass -File "$HOME\Downloads\bonfirelan-server.ps1" -ServerDir "C:\path\to\your\server"
```

| Parameter | Default | What for |
|---|---|---|
| `-ServerDir` | `.` | Server folder (the one with `mods/` and `server.properties`) |
| `-Port` | `25580` | HTTP port (bonfireLAN looks for it on 25580) |
| `-RecommendedRam` | `4096` | Recommended RAM in MB sent to players |
| `-McVersion` / `-FabricVersion` | auto | Force them if detection gets it wrong |
| `-Bind` | `0.0.0.0` | Address to listen on (`127.0.0.1` = this PC only, for testing) |

The first time, Windows asks about the firewall: allow it **only on private networks**.

Leave it open while the server is running. If you add or remove mods it updates by itself.

## Endpoints (read-only)

- `GET /bonfirelan/manifest`: JSON with `serverName`, `minecraft`, `loader`, `fabricVersion`, `recommendedRam` and `mods[]` (`name`, `version`, `hash` SHA-1, `url`, `fileName`, `modId`, `environment`, `size`). Contract in `src/types.ts`.
- `GET /bonfirelan/mods/<fileName>`: the `.jar`. Only serves files listed in the manifest.
