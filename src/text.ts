// Neutral string helpers shared by the server and the TUI (no theme/terminal deps).

export function clip(s: string, n: number): string {
  return s.length > n ? `${s.slice(0, n)}…` : s;
}
