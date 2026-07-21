// Client-side TUI state persisted across runs (~/.bough/tui.json). Best-effort:
// a missing file, bad JSON, or a denied write just means no session memory.
const STATE_PATH = `${Deno.env.get("HOME")}/.bough/tui.json`;

interface TuiState {
  /** bough_session auth cookie ("name=token") from a previous login. */
  cookie?: string;
  /** Composer history (sent messages, oldest first) — survives restarts. */
  history?: string[];
  /** `!` shell commands (oldest first) — the backwards-fzf corpus. */
  shellHistory?: string[];
}

const HISTORY_CAP = 50;
const SHELL_HISTORY_CAP = 200;

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

export function loadShellHistory(): string[] {
  const h = load().shellHistory;
  return Array.isArray(h) ? h.filter((x) => typeof x === "string") : [];
}

/** Append a `!` command, fzf-style: an earlier duplicate moves to the tip. */
export function appendShellHistory(cmd: string): void {
  save({
    shellHistory: [...loadShellHistory().filter((c) => c !== cmd), cmd].slice(-SHELL_HISTORY_CAP),
  });
}
