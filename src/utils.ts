export function stripMinecraftColors(input: string): string {
  return input.replace(/§./g, "").trim();
}

const FAVORITES_KEY = "bonfirelan:favorites";

export function loadFavorites(): string[] {
  try {
    const raw = localStorage.getItem(FAVORITES_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.filter((x) => typeof x === "string") : [];
  } catch {
    return [];
  }
}

export function saveFavorite(address: string): void {
  const addr = address.trim();
  if (!addr) return;
  const current = loadFavorites();
  if (current.includes(addr)) return;
  try {
    localStorage.setItem(FAVORITES_KEY, JSON.stringify([...current, addr]));
  } catch {}
}

const USERNAME_KEY = "bonfirelan:username";

export function sanitizeUsername(input: string): string {
  return input.replace(/[^A-Za-z0-9_]/g, "").slice(0, 16);
}

export function loadUsername(): string | null {
  try {
    return localStorage.getItem(USERNAME_KEY);
  } catch {
    return null;
  }
}

export function saveUsername(name: string | null): void {
  try {
    if (name) localStorage.setItem(USERNAME_KEY, name);
    else localStorage.removeItem(USERNAME_KEY);
  } catch {}
}
