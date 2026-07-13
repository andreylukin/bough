// Client-side TUI state persisted across runs (~/.bough/tui.json). Best-effort:
// a missing file, bad JSON, or a denied write just means no session memory.
const STATE_PATH = `${Deno.env.get("HOME")}/.bough/tui.json`;

interface TuiState {
  lastSessionId?: string;
  /** bough_session auth cookie ("name=token") from a previous login. */
  cookie?: string;
  /** Composer history (sent messages, oldest first) — survives restarts. */
  history?: string[];
}

const HISTORY_CAP = 50;

function load(): TuiState {
  try {
    const s = JSON.parse(Deno.readTextFileSync(STATE_PATH));
    return typeof s === "object" && s !== null ? s as TuiState : {};
  } catch {
    return {};
  }
}

function save(patch: Partial<TuiState>): void {
  try {
    Deno.writeTextFileSync(STATE_PATH, JSON.stringify({ ...load(), ...patch }) + "\n");
  } catch {
    // read-only fs or missing --allow-write — skip silently.
  }
}

export function loadLastSession(): string | null {
  return load().lastSessionId ?? null;
}

export function saveLastSession(id: string): void {
  save({ lastSessionId: id });
}

export function loadCookie(): string | null {
  return load().cookie ?? null;
}

export function saveCookie(cookie: string): void {
  save({ cookie });
}

export function loadHistory(): string[] {
  const h = load().history;
  return Array.isArray(h) ? h.filter((x) => typeof x === "string") : [];
}

export function appendHistory(entry: string): void {
  save({ history: [...loadHistory(), entry].slice(-HISTORY_CAP) });
}
