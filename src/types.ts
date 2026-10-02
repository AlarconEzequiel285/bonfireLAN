export interface DiscoveredServer {
  id: string;
  ip: string;
  port: number;
  name: string;
  mcVersion: string;
  loader: "fabric" | "vanilla" | "unknown";
  players: { online: number; max: number };
  faviconBase64?: string;
  bonfirelanReady: boolean;
  manifestUrl?: string;
}

export type CheckState = "ok" | "warn" | "missing" | "incompatible";

export interface HealthReport {
  java: { state: CheckState; version?: string; path?: string };
  minecraft: { state: CheckState; version?: string; path?: string };
  loader: { state: CheckState; version?: string; path?: string };
  ram: { state: CheckState; totalMb: number; recommendedMb: number; assignedMb: number };
}

export interface ModEntry {
  name: string;
  version: string;
  hash: string;
  source?: "lan" | "modrinth" | "url" | "manual";
  url?: string;
  fileName?: string;
  modId?: string;
  environment?: string;
  size?: number;
}

export interface ServerManifest {
  serverName: string;
  minecraft: string;
  loader: "fabric";
  fabricVersion: string;
  recommendedRam: number;
  mods: ModEntry[];
}

export type ModStatus = "ok" | "missing" | "mismatch" | "extra";

export interface ModDiffReport {
  entries: Array<{ name: string; status: ModStatus; required?: string; local?: string }>;
  toDownload: ModEntry[];
}

export type LaunchPhase = "idle" | "preparing" | "launching" | "in_game" | "error";

export interface LaunchConfig {
  javaPath: string;
  mcVersion: string;
  ramMb: number;
  serverIp: string;
  serverPort: number;
  useFabric: boolean;
  profileId?: string;
  gameDir?: string;
  username?: string;
}

export interface LaunchResult {
  success: boolean;
  message: string;
  pid?: number;
  logPath: string;
}

export interface LaunchExit {
  code: number | null;
  logTail: string;
  logPath: string;
}

export interface PrepareProgress {
  phase: string;
  message: string;
  current: number;
  total: number;
}

export interface PrepareResult {
  profileId: string;
}

export interface ModSyncResult {
  gameDir: string;
  manifest: ServerManifest;
  installed: string[];
  upToDate: string[];
  removed: string[];
  skippedServerOnly: string[];
}
