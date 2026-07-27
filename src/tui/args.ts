/**
 * The TUI's command line.
 *
 * It did not have one. `main.tsx` never touched `Deno.args`, so every flag a user
 * passed was silently discarded — `bough -w /other/repo` opened a session in the
 * current directory and said nothing. That is the worst possible failure for this
 * particular flag: bough edits the real checkout with the user's authority and no
 * sandbox (spec §2), so a silently-ignored workspace means the agent writes to a
 * repository the user did not choose and believes it is not touching.
 *
 * The rule is `cli/exec.ts`'s rule, for the same reason: **an unknown flag is an
 * error.** A typo that silently starts anyway is worse than one that stops, and a
 * flag this app does not implement is indistinguishable from a typo.
 *
 * Pure and total, so the whole surface is asserted without a terminal.
 */

export const USAGE = "usage: bough [-w DIR]\n" +
  "\n" +
  "  -w, --workspace DIR   where new conversations start (default: the cwd)\n" +
  "  -h, --help            this message\n" +
  "\n" +
  "  the server port comes from BOUGH_PORT (default 4321). It is an env var and\n" +
  "  not a flag because the API client is bound at import, before a flag could be\n" +
  "  read — a --port that parsed and did nothing would be the bug this file fixes.\n" +
  "\n" +
  "programs run as you, with your authority — there is no sandbox.";

export interface TuiArgs {
  /** Where a new conversation starts. Absent = the process cwd. */
  workspace?: string;
}

export interface TuiUsageError {
  usageError: string;
}

export interface TuiHelpRequest {
  help: true;
}

export type TuiArgsResult = TuiArgs | TuiUsageError | TuiHelpRequest;

export function isTuiUsageError(x: TuiArgsResult): x is TuiUsageError {
  return "usageError" in x;
}

export function isTuiHelpRequest(x: TuiArgsResult): x is TuiHelpRequest {
  return "help" in x;
}

const SHORT: Record<string, string> = { w: "workspace" };
const VALUE_FLAGS = new Set(["workspace"]);

export function parseTuiArgs(argv: readonly string[]): TuiArgsResult {
  const values: Record<string, string> = {};

  for (let i = 0; i < argv.length; i++) {
    const token = argv[i];
    let name: string | undefined;
    let inline: string | undefined;

    if (token === "--help" || token === "-h") return { help: true };

    if (token.startsWith("--")) {
      const eq = token.indexOf("=");
      name = eq === -1 ? token.slice(2) : token.slice(2, eq);
      inline = eq === -1 ? undefined : token.slice(eq + 1);
    } else if (token.startsWith("-") && token.length > 1) {
      const eq = token.indexOf("=");
      const short = eq === -1 ? token.slice(1) : token.slice(1, eq);
      name = SHORT[short];
      if (!name) return { usageError: `unknown flag -${short}\n${USAGE}` };
      inline = eq === -1 ? undefined : token.slice(eq + 1);
    } else {
      // The TUI takes no positional argument — it is not `bough exec`, and a
      // stray prompt here would otherwise vanish into a screen that ignores it.
      return {
        usageError: `bough takes no positional argument (got "${token}").\n` +
          `Did you mean: bough exec "${token}"?\n${USAGE}`,
      };
    }

    if (!VALUE_FLAGS.has(name)) return { usageError: `unknown flag --${name}\n${USAGE}` };
    if (inline !== undefined) {
      values[name] = inline;
      continue;
    }
    if (i + 1 >= argv.length) return { usageError: `--${name} needs a value\n${USAGE}` };
    values[name] = argv[++i];
  }

  const out: TuiArgs = {};
  if (values.workspace !== undefined) {
    if (values.workspace.trim() === "") return { usageError: `--workspace needs a path\n${USAGE}` };
    out.workspace = values.workspace;
  }
  return out;
}
