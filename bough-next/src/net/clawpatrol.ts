/**
 * Real Claw Patrol integration: run the actual `clawpatrol` binary against a
 * rendered policy. The offline policy.ts mirror stays the first line (fast, no
 * dependency); this is the authoritative regression the design doc calls for —
 * `clawpatrol validate` compiles the HCL exactly as the gateway would, and
 * `clawpatrol test` replays each bundle fixture and reports verdict drift.
 *
 * Fixture wire format (one JSON file per fixture): { action, match } where
 * bough's BundleFixture carries the same shapes as { action, expect }.
 */
import { join } from "node:path";
import type { BundleFixture } from "./bundles.ts";

export interface ClawpatrolResult {
  ran: boolean; // false = binary not installed; caller treats as skipped
  ok: boolean;
  output: string;
  /**
   * True when an earlier attempt drifted and a retry passed. KNOWN UPSTREAM BUG
   * (observed 2026-07-01, ~1/6 runs on identical inputs): clawpatrol's rule
   * evaluation order is nondeterministic — a request matching several rules
   * (e.g. POST /graphql mutation matches both github-graphql-mutation and
   * github-rest-writes) is attributed to a different rule per run, and verdicts
   * that depend on first-match order can flip. Symptoms point at Go map
   * iteration ordering the rules. Fix belongs in clawpatrol; until then we
   * retry and surface the instability instead of failing installs on a coin flip.
   */
  flaky?: boolean;
}

/** The binary to run; BOUGH_CLAWPATROL overrides (tests / custom installs). */
function bin(): string {
  return Deno.env.get("BOUGH_CLAWPATROL") ?? "clawpatrol";
}

export function clawpatrolAvailable(): boolean {
  try {
    const out = new Deno.Command(bin(), { args: ["version"], stdout: "null", stderr: "null" })
      .outputSync();
    return out.success;
  } catch {
    return false;
  }
}

async function run(args: string[]): Promise<{ ok: boolean; output: string }> {
  const out = await new Deno.Command(bin(), { args, stdout: "piped", stderr: "piped" }).output();
  const dec = new TextDecoder();
  return {
    ok: out.success,
    output: (dec.decode(out.stdout) + dec.decode(out.stderr)).trim(),
  };
}

/**
 * Compile the HCL and replay the fixtures through the real binary. Returns
 * `ran: false` when clawpatrol isn't installed (callers skip, not fail).
 */
export async function clawpatrolTest(
  hcl: string,
  fixtures: BundleFixture[],
  attempts = 3,
): Promise<ClawpatrolResult> {
  if (!clawpatrolAvailable()) return { ran: false, ok: true, output: "clawpatrol not installed" };

  const dir = await Deno.makeTempDir({ prefix: "bough-clawpatrol-" });
  try {
    const hclPath = join(dir, "policy.hcl");
    await Deno.writeTextFile(hclPath, hcl);
    const fixturesDir = join(dir, "fixtures");
    await Deno.mkdir(fixturesDir);
    for (const f of fixtures) {
      await Deno.writeTextFile(
        join(fixturesDir, `${f.name}.json`),
        JSON.stringify({ action: f.action, match: f.expect }, null, 2),
      );
    }

    const validated = await run(["validate", hclPath]);
    if (!validated.ok) return { ran: true, ok: false, output: `validate: ${validated.output}` };
    if (fixtures.length === 0) return { ran: true, ok: true, output: validated.output };

    // Retry loop for the upstream nondeterminism (see ClawpatrolResult.flaky).
    // A consistently-wrong policy fails every attempt; only order-flip drift
    // passes on retry — and that gets flagged, not swallowed.
    let last = { ok: false, output: "" };
    for (let i = 0; i < attempts; i++) {
      last = await run(["test", hclPath, fixturesDir]);
      if (last.ok) return { ran: true, ok: true, output: last.output, flaky: i > 0 };
    }
    return { ran: true, ok: false, output: last.output };
  } finally {
    await Deno.remove(dir, { recursive: true }).catch(() => {});
  }
}
