// Neutral string helpers shared by the server and the TUI (no theme/terminal deps).

export function clip(s: string, n: number): string {
  return s.length > n ? `${s.slice(0, n)}…` : s;
}

/**
 * One-line excerpt of a tool call's input: the first meaningful code line (or
 * compact JSON). A bare tool name ("run_steps") tells the reader nothing about
 * what ran — the transcript folds and the workflow agent view both label calls
 * with this.
 */
export function codeGist(input: unknown, width = 60): string {
  const raw = input as Record<string, unknown> | null | undefined;
  const code = raw && typeof raw.code === "string" ? raw.code : null;
  const src = code ?? (input === undefined ? "" : JSON.stringify(input));
  const line = src.trim().split("\n").map((l) => l.trim())
    .find((l) => l.length > 0 && !l.startsWith("//")) ?? "";
  return clip(line, width);
}
