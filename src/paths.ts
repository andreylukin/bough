// Single source of truth for the ~/.bough data root and its subpaths. Two HOME
// resolutions coexist ON PURPOSE and are NOT interchangeable:
//
//   • boughHome() / boughPath() use node:os homedir(), which falls back to the
//     passwd entry (getpwuid) when $HOME is unset and never throws — what the
//     config / cache / data subsystems want.
//   • homeStrict(ctx) reads $HOME directly and THROWS when it is unset. The
//     sandbox and vcs layers use it deliberately: they must hard-fail rather than
//     silently write to a wrong (getpwuid-derived) root.
//
// Callers keep their own env override (BOUGH_NET_DIR, BOUGH_SHADOW_BASE, …) in
// FRONT of these — the overrides point at full subpaths, not the root itself.

import { homedir } from "node:os";
import { join } from "node:path";

/** The `~/.bough` data root (lenient: getpwuid fallback, never throws). */
export function boughHome(): string {
  return join(homedir(), ".bough");
}

/** A path under the `~/.bough` data root. */
export function boughPath(...segs: string[]): string {
  return join(boughHome(), ...segs);
}

/** `$HOME`, or throw — for the sandbox/vcs sites that must hard-fail rather than
 * fall back to a getpwuid-derived home. `ctx` names the failing subsystem. */
export function homeStrict(ctx: string): string {
  const home = Deno.env.get("HOME");
  if (!home) throw new Error(`${ctx}: no $HOME`);
  return home;
}
