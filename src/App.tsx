import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  DiscoveredServer,
  HealthReport,
  LaunchConfig,
  LaunchExit,
  LaunchPhase,
  LaunchResult,
  ModSyncResult,
  PrepareProgress,
  PrepareResult,
  ServerManifest,
} from "./types";
import {
  loadFavorites,
  loadUsername,
  sanitizeUsername,
  saveFavorite,
  saveUsername,
  stripMinecraftColors,
} from "./utils";

const REFRESH_MS = 15_000;

function serverName(s: DiscoveredServer) {
  return stripMinecraftColors(s.name) || `${s.ip}:${s.port}`;
}

function RefreshIcon({ spinning }: { spinning: boolean }) {
  return (
    <svg
      viewBox="0 0 24 24"
      className={`h-4 w-4 ${spinning ? "animate-spin" : ""}`}
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d="M21 12a9 9 0 1 1-2.64-6.36L21 8" />
      <path d="M21 3v5h-5" />
    </svg>
  );
}

function UserIcon() {
  return (
    <svg viewBox="0 0 24 24" className="h-3.5 w-3.5" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
      <circle cx="12" cy="8" r="4" />
      <path d="M4 21c0-4 4-6 8-6s8 2 8 6" />
    </svg>
  );
}

function NameField({ value, fallback, onChange }: { value: string | null; fallback: string; onChange: (name: string | null) => void }) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");

  function commit() {
    setEditing(false);
    if (draft === "" || draft === fallback) onChange(null);
    else if (draft.length >= 3) onChange(draft);
  }

  if (editing) {
    return (
      <input
        autoFocus
        value={draft}
        maxLength={16}
        spellCheck={false}
        placeholder={fallback}
        onChange={(e) => setDraft(sanitizeUsername(e.currentTarget.value))}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") e.currentTarget.blur();
          if (e.key === "Escape") setEditing(false);
        }}
        className="w-36 rounded-md border border-stone-700 bg-stone-900 px-2.5 py-1 text-xs focus:outline-none"
      />
    );
  }

  return (
    <button
      onClick={() => {
        setDraft(value ?? fallback);
        setEditing(true);
      }}
      className="flex items-center gap-1.5 rounded-md bg-stone-900/60 px-2.5 py-1 text-xs text-stone-300 transition hover:bg-stone-800 hover:text-stone-100"
    >
      <UserIcon />
      {value ?? fallback}
    </button>
  );
}

function Favicon({ src, name }: { src?: string; name: string }) {
  if (src) {
    return <img src={src} alt="" className="h-11 w-11 shrink-0 rounded-md [image-rendering:pixelated]" />;
  }
  return (
    <div className="grid h-11 w-11 shrink-0 place-items-center rounded-md bg-stone-800 text-sm font-semibold text-stone-400">
      {name.charAt(0).toUpperCase()}
    </div>
  );
}

function ModList({ manifestUrl }: { manifestUrl: string }) {
  const [manifest, setManifest] = useState<ServerManifest | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    invoke<ServerManifest>("get_manifest", { url: manifestUrl })
      .then(setManifest)
      .catch((err) => setError(String(err)));
  }, [manifestUrl]);

  if (error) return <p className="text-red-400">Couldn't load the mod list.</p>;
  if (!manifest) return <p className="text-stone-500">Loading…</p>;
  if (manifest.mods.length === 0) return <p className="text-stone-500">No mods.</p>;

  return (
    <ul className="space-y-1">
      {manifest.mods.map((m) => (
        <li key={m.hash} className="flex justify-between gap-3">
          <span className="truncate text-stone-300">{m.name}</span>
          <span className="shrink-0 text-stone-600">{m.environment === "server" ? "server" : m.version}</span>
        </li>
      ))}
    </ul>
  );
}

function ServerRow({
  server,
  disabled,
  onPlay,
}: {
  server: DiscoveredServer;
  disabled: boolean;
  onPlay: (s: DiscoveredServer, h: HealthReport) => void;
}) {
  const [checking, setChecking] = useState(false);
  const [copied, setCopied] = useState(false);
  const [showMods, setShowMods] = useState(false);
  const address = `${server.ip}:${server.port}`;
  const name = serverName(server);
  const canSyncMods = server.bonfirelanReady && !!server.manifestUrl;

  async function copyAddress() {
    try {
      await navigator.clipboard.writeText(address);
      setCopied(true);
      setTimeout(() => setCopied(false), 1200);
    } catch {}
  }

  async function play() {
    setChecking(true);
    try {
      onPlay(server, await invoke<HealthReport>("run_health_check", { server }));
    } catch (err) {
      console.error(err);
    } finally {
      setChecking(false);
    }
  }

  return (
    <li className="rounded-lg border border-stone-800/80 bg-stone-900/40 p-3 transition hover:border-stone-700">
      <div className="flex items-center gap-3">
        <Favicon src={server.faviconBase64} name={name} />
        <div className="min-w-0 flex-1">
          <p className="truncate text-sm font-medium">{name}</p>
          <p className="mt-0.5 flex items-center gap-1.5 text-xs text-stone-500">
            <span>
              {server.mcVersion}
              {server.loader === "fabric" && " · Fabric"}
            </span>
            <span className="text-stone-700">·</span>
            <span className="h-1.5 w-1.5 rounded-full bg-green-500" />
            <span>
              {server.players.online}/{server.players.max}
            </span>
          </p>
        </div>
        <button
          onClick={play}
          disabled={disabled || checking}
          className="shrink-0 rounded-md bg-fire px-4 py-1.5 text-sm font-medium text-stone-950 transition hover:bg-fire-light disabled:cursor-not-allowed disabled:opacity-40"
        >
          {checking ? "…" : "Play"}
        </button>
      </div>

      <div className="mt-2 flex items-center gap-3 pl-14 text-xs text-stone-600">
        <button onClick={copyAddress} className="font-mono transition hover:text-stone-400">
          {copied ? "copied" : address}
        </button>
        {canSyncMods && (
          <button onClick={() => setShowMods(!showMods)} className="transition hover:text-stone-400">
            {showMods ? "hide mods" : "mods"}
          </button>
        )}
      </div>

      {showMods && server.manifestUrl && (
        <div className="mt-2 ml-14 max-h-40 overflow-auto rounded-md bg-stone-950/60 p-2 text-xs">
          <ModList manifestUrl={server.manifestUrl} />
        </div>
      )}
    </li>
  );
}

function StatusBar({
  phase,
  prepare,
  playing,
  modSync,
  error,
  onDismiss,
}: {
  phase: LaunchPhase;
  prepare: PrepareProgress | null;
  playing: DiscoveredServer | null;
  modSync: ModSyncResult | null;
  error: string | null;
  onDismiss: () => void;
}) {
  if (phase === "preparing" || phase === "launching") {
    const pct = prepare && prepare.total > 0 ? Math.round((prepare.current / prepare.total) * 100) : null;
    return (
      <div>
        <div className="flex justify-between text-xs">
          <span className="text-stone-300">
            {phase === "launching" ? "Starting Minecraft…" : prepare?.message ?? "Preparing…"}
          </span>
          {pct !== null && <span className="tabular-nums text-stone-500">{pct}%</span>}
        </div>
        <div className="mt-2 h-1 overflow-hidden rounded-full bg-stone-800">
          {pct !== null ? (
            <div className="h-full bg-fire transition-all" style={{ width: `${pct}%` }} />
          ) : (
            <div className="h-full w-1/3 animate-pulse bg-fire/60" />
          )}
        </div>
      </div>
    );
  }

  if (phase === "in_game") {
    return (
      <div className="flex items-center gap-2 text-xs">
        <span className="h-2 w-2 animate-pulse rounded-full bg-fire" />
        <span className="truncate text-stone-300">Playing on {playing ? serverName(playing) : "the server"}</span>
        {modSync && modSync.installed.length > 0 && (
          <span className="ml-auto shrink-0 text-stone-600">
            {modSync.installed.length} {modSync.installed.length === 1 ? "new mod" : "new mods"}
          </span>
        )}
      </div>
    );
  }

  if (phase === "error" && error) {
    const [title, ...rest] = error.split("\n");
    const details = rest.join("\n").trim();
    return (
      <div className="text-xs">
        <div className="flex items-start gap-3">
          <p className="flex-1 text-red-400">{title}</p>
          <button onClick={onDismiss} className="text-stone-500 transition hover:text-stone-300">
            Dismiss
          </button>
        </div>
        {details && (
          <pre className="mt-2 max-h-48 select-text overflow-auto rounded-md bg-stone-900 p-2 font-mono text-[11px] whitespace-pre-wrap text-stone-400">
            {details}
          </pre>
        )}
      </div>
    );
  }

  return null;
}

function App() {
  const [servers, setServers] = useState<DiscoveredServer[]>([]);
  const [scanning, setScanning] = useState(true);
  const [scanError, setScanError] = useState<string | null>(null);
  const [phase, setPhase] = useState<LaunchPhase>("idle");
  const [launchError, setLaunchError] = useState<string | null>(null);
  const [prepare, setPrepare] = useState<PrepareProgress | null>(null);
  const [modSync, setModSync] = useState<ModSyncResult | null>(null);
  const [playing, setPlaying] = useState<DiscoveredServer | null>(null);
  const [manualIp, setManualIp] = useState("");
  const [manualError, setManualError] = useState<string | null>(null);
  const [manualBusy, setManualBusy] = useState(false);
  const [username, setUsername] = useState<string | null>(loadUsername);
  const [defaultName, setDefaultName] = useState("Player");

  function fail(message: string) {
    setLaunchError(message);
    setPhase("error");
  }

  async function handlePlay(server: DiscoveredServer, health: HealthReport) {
    setLaunchError(null);
    setModSync(null);
    setPlaying(server);
    setPhase("preparing");

    let javaPath = health.java.state === "ok" ? health.java.path : undefined;
    if (!javaPath) {
      setPrepare({ phase: "java", message: "Installing Java…", current: 0, total: 0 });
      try {
        javaPath = await invoke<string>("ensure_java", { mcVersion: server.mcVersion });
      } catch (err) {
        return fail(`Couldn't install Java\n${err}`);
      } finally {
        setPrepare(null);
      }
    }

    // The host manifest wins over what the server ping reports.
    let mcVersion = server.mcVersion;
    let useFabric = server.loader === "fabric";
    let fabricVersion: string | undefined;
    let gameDir: string | undefined;
    if (server.bonfirelanReady && server.manifestUrl) {
      setPrepare({ phase: "mods", message: "Syncing mods…", current: 0, total: 0 });
      try {
        const sync = await invoke<ModSyncResult>("sync_mods", {
          serverId: server.id,
          manifestUrl: server.manifestUrl,
          ramMb: health.ram.assignedMb,
        });
        setModSync(sync);
        mcVersion = sync.manifest.minecraft || mcVersion;
        useFabric = sync.manifest.loader === "fabric";
        fabricVersion = sync.manifest.fabricVersion || undefined;
        gameDir = sync.gameDir;
      } catch (err) {
        return fail(`Couldn't sync mods\n${err}`);
      } finally {
        setPrepare(null);
      }
    }

    setPrepare({ phase: "vanilla", message: "Preparing…", current: 0, total: 0 });
    let profileId: string;
    try {
      const prepared = await invoke<PrepareResult>("prepare_instance", { mcVersion, useFabric, fabricVersion });
      profileId = prepared.profileId;
    } catch (err) {
      return fail(`Couldn't prepare Minecraft\n${err}`);
    } finally {
      setPrepare(null);
    }

    setPhase("launching");
    const config: LaunchConfig = {
      javaPath: javaPath!,
      mcVersion,
      ramMb: health.ram.assignedMb,
      serverIp: server.ip,
      serverPort: server.port,
      useFabric,
      profileId,
      gameDir,
      username: username ?? undefined,
    };
    try {
      const result = await invoke<LaunchResult>("launch_minecraft", { config });
      if (result.success) setPhase("in_game");
      else fail(`Minecraft didn't start\n${result.message}`);
    } catch (err) {
      fail(`Minecraft didn't start\n${err}`);
    }
  }

  function upsertServer(server: DiscoveredServer) {
    setServers((prev) => {
      const i = prev.findIndex((s) => s.id === server.id);
      if (i === -1) return [...prev, server];
      const next = [...prev];
      next[i] = server;
      return next;
    });
  }

  async function addManual(e: React.FormEvent) {
    e.preventDefault();
    const input = manualIp.trim();
    if (!input) return;
    const [ip, portStr] = input.split(":");
    const port = portStr ? Number(portStr) : undefined;
    if (portStr && (!Number.isInteger(port) || port! < 1 || port! > 65535)) {
      setManualError("Invalid port");
      return;
    }
    setManualBusy(true);
    setManualError(null);
    try {
      upsertServer(await invoke<DiscoveredServer>("ping_server", { ip, port }));
      saveFavorite(input);
      setManualIp("");
    } catch {
      setManualError("No response");
    } finally {
      setManualBusy(false);
    }
  }

  async function scan() {
    setScanning(true);
    setScanError(null);
    try {
      const found = await invoke<DiscoveredServer[]>("scan_lan");
      found.forEach(upsertServer);
    } catch (e) {
      setScanError(String(e));
    } finally {
      setScanning(false);
    }
  }

  async function refreshAddress(ip: string, port: number) {
    try {
      upsertServer(await invoke<DiscoveredServer>("ping_server", { ip, port }));
    } catch {}
  }

  useEffect(() => {
    const unlisten = listen<DiscoveredServer>("scan:server-found", (event) => upsertServer(event.payload));
    scan();
    for (const addr of loadFavorites()) {
      const [ip, portStr] = addr.split(":");
      refreshAddress(ip, portStr ? Number(portStr) : 25565);
    }
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  useEffect(() => {
    invoke<string>("default_username").then(setDefaultName).catch(() => {});
  }, []);

  useEffect(() => {
    const unlisten = listen<PrepareProgress>("provision:progress", (event) => setPrepare(event.payload));
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  const phaseRef = useRef(phase);
  phaseRef.current = phase;
  useEffect(() => {
    const unlisten = listen<LaunchExit>("launch:exited", (event) => {
      if (phaseRef.current !== "in_game" && phaseRef.current !== "launching") return;
      const { code, logTail } = event.payload;
      if (code === 0) {
        setPhase("idle");
      } else {
        fail(`Minecraft closed (code ${code ?? "?"})\n${logTail}`);
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  const serversRef = useRef(servers);
  serversRef.current = servers;
  useEffect(() => {
    const id = setInterval(() => {
      for (const s of serversRef.current) refreshAddress(s.ip, s.port);
    }, REFRESH_MS);
    return () => clearInterval(id);
  }, []);

  const busy = phase === "preparing" || phase === "launching" || phase === "in_game";
  const subtitle = scanning
    ? "Searching…"
    : `${servers.length} ${servers.length === 1 ? "server" : "servers"}`;

  return (
    <div className="flex h-full flex-col bg-[#1c1714] text-stone-100 select-none">
      <header className="flex items-center justify-between px-5 pt-5 pb-4">
        <div className="flex items-center gap-3">
          <img src="/flame.svg" alt="" className="h-8 w-8" />
          <div>
            <h1 className="text-base font-semibold tracking-tight">
              bonfire<span className="text-fire">LAN</span>
            </h1>
            <p className="text-xs text-stone-500">{subtitle}</p>
          </div>
        </div>
        <div className="flex items-center gap-1">
          <NameField
            value={username}
            fallback={defaultName}
            onChange={(name) => {
              setUsername(name);
              saveUsername(name);
            }}
          />
          <button
            onClick={scan}
            disabled={scanning}
            title="Refresh"
            className="rounded-md p-2 text-stone-400 transition hover:bg-stone-900 hover:text-stone-200 disabled:cursor-default disabled:hover:bg-transparent"
          >
            <RefreshIcon spinning={scanning} />
          </button>
        </div>
      </header>

      <main className="flex-1 overflow-y-auto px-5 pb-4">
        {scanError && <p className="mb-3 text-xs text-red-400">Couldn't scan the network: {scanError}</p>}

        {servers.length === 0 && !scanning && (
          <p className="py-20 text-center text-sm text-stone-600">No servers found</p>
        )}

        <ul className="space-y-2">
          {servers.map((s) => (
            <ServerRow key={s.id} server={s} disabled={busy} onPlay={handlePlay} />
          ))}
        </ul>
      </main>

      <footer className="border-t border-stone-900 px-5 py-4">
        {phase === "idle" ? (
          <form onSubmit={addManual} className="flex gap-2">
            <input
              value={manualIp}
              onChange={(e) => {
                setManualIp(e.currentTarget.value);
                setManualError(null);
              }}
              placeholder="Add by IP"
              spellCheck={false}
              className="min-w-0 flex-1 rounded-md border border-stone-800 bg-stone-900/50 px-3 py-1.5 text-sm placeholder:text-stone-600 focus:border-stone-600 focus:outline-none"
            />
            {manualError && <span className="self-center text-xs text-red-400">{manualError}</span>}
            <button
              type="submit"
              disabled={manualBusy || !manualIp.trim()}
              className="shrink-0 rounded-md bg-stone-800 px-3 py-1.5 text-sm text-stone-200 transition hover:bg-stone-700 disabled:opacity-40"
            >
              {manualBusy ? "…" : "Add"}
            </button>
          </form>
        ) : (
          <StatusBar
            phase={phase}
            prepare={prepare}
            playing={playing}
            modSync={modSync}
            error={launchError}
            onDismiss={() => {
              setLaunchError(null);
              setPhase("idle");
            }}
          />
        )}
      </footer>
    </div>
  );
}

export default App;
