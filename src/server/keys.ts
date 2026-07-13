/**
 * API-key management. Reports which provider keys are configured (booleans only,
 * never values) and applies a new key two ways: into the live process env
 * (Deno.env.set — the LLM clients read the env at run() time, so it takes effect
 * immediately) and into the launchd env file (~/.bough/env) so it survives a restart.
 *
 * The env file is the KEY=VALUE list the `bough` launcher sources; scripts/bough's
 * env_set writes the same format. We update the existing line for the var (even a
 * commented "# VAR=" template), else append, and keep the file 0600.
 *
 * Caveat: today the server runs as the login user and writes ~/.bough/env directly.
 * In a future agent-user cutover the env file would live in the agent user's 0600
 * home, unwritable from here — persistence would then have to route through
 * scripts/bough. Not built for that now.
 */
import { chmodSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { z } from "zod";

export type KeyProvider = "anthropic" | "openrouter" | "openai";

/** The env var backing each provider's key. */
export const KEY_ENV: Record<KeyProvider, string> = {
  anthropic: "ANTHROPIC_API_KEY",
  openrouter: "OPENROUTER_API_KEY",
  openai: "OPENAI_API_KEY",
};

/** PUT /config/keys body: a provider and its single-line, non-empty key. */
export const KeysBody = z.object({
  provider: z.enum(["anthropic", "openrouter", "openai"]),
  key: z.string().refine(
    (k) => k.trim().length > 0 && !/[\r\n]/.test(k),
    "key must be non-empty and single-line",
  ),
});

/** Which provider keys are configured right now (non-empty env var). Never values. */
export function keyStatus(): Record<KeyProvider, boolean> {
  const out = {} as Record<KeyProvider, boolean>;
  for (const [provider, env] of Object.entries(KEY_ENV) as [KeyProvider, string][]) {
    out[provider] = !!Deno.env.get(env)?.trim();
  }
  return out;
}

function envPath(dir?: string): string {
  return join(dir ?? join(homedir(), ".bough"), "env");
}

/**
 * Update-or-append `VAR=value` in env-file text. Replaces the first line that sets
 * VAR — or a commented "# VAR=" / "#VAR=" template — preserving every other line;
 * appends when absent. Pure (text in, text out), so it's testable in isolation.
 */
export function setEnvVar(text: string, varName: string, value: string): string {
  const lines = text.split("\n");
  // A trailing newline leaves a final "" element; drop it so we don't accrete blanks.
  if (lines.length > 0 && lines.at(-1) === "") lines.pop();
  const re = new RegExp(`^#? ?${escapeRe(varName)}=`);
  let replaced = false;
  const out = lines.map((line) => {
    if (!replaced && re.test(line)) {
      replaced = true;
      return `${varName}=${value}`;
    }
    return line;
  });
  if (!replaced) out.push(`${varName}=${value}`);
  return out.join("\n") + "\n";
}

function escapeRe(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * Apply a provider key: live process env (immediate) plus ~/.bough/env (survives
 * restart). Returns the refreshed status. `dir` overrides the config dir for tests.
 */
export function setKey(
  provider: KeyProvider,
  key: string,
  dir?: string,
): Record<KeyProvider, boolean> {
  const varName = KEY_ENV[provider];
  const value = key.trim();
  Deno.env.set(varName, value);
  const path = envPath(dir);
  mkdirSync(join(path, ".."), { recursive: true });
  let current = "";
  try {
    current = readFileSync(path, "utf8");
  } catch {
    current = ""; // no env file yet — create it
  }
  writeFileSync(path, setEnvVar(current, varName, value), { mode: 0o600 });
  chmodSync(path, 0o600); // writeFileSync's mode only applies on create
  return keyStatus();
}
