/**
 * `bough net validate` — the offline validate pass, upstream Claw Patrol's
 * `clawpatrol validate` reshaped for the native gate. Runs the SAME load paths the
 * server does, but loudly: where loadConfig silently falls back to the default
 * posture on a corrupt policy.json, this prints exactly what's wrong, and where the
 * plugin loader lists a broken file and moves on, this fails the run.
 *
 * Checks, in order:
 *   1. policy.json parses (JSON + NetConfig schema — every rule condition compiles
 *      inside the zod parse);
 *   2. every plugin in the library loads (shape, fixtures, name collisions);
 *   3. cross-references: activations naming a plugin that isn't loaded, and rule
 *      approver chains naming a plugin that isn't loaded or has no gate() (both
 *      fail closed at runtime — flagged here as warnings so the operator learns
 *      BEFORE a request dies on them).
 *
 * Exit 0 when nothing errored (warnings allowed), 1 otherwise. Global scope only —
 * per-session overrides live in the db and are validated on write (PUT /net/policy).
 */
import { join } from "node:path";
import { existsSync, readFileSync } from "node:fs";
import { z } from "zod/v4";
import { NetConfig } from "./config.ts";
import { netDir } from "./install.ts";
import { PluginHost, pluginsDir } from "./plugins.ts";

export interface ValidateReport {
  ok: boolean;
  lines: string[];
}

export async function validateNet(dir = netDir()): Promise<ValidateReport> {
  const lines: string[] = [];
  let errors = 0;
  const error = (msg: string) => {
    lines.push(`error: ${msg}`);
    errors++;
  };
  const warn = (msg: string) => lines.push(`warning: ${msg}`);

  // 1. policy.json — parse loudly.
  let config: NetConfig | undefined;
  const policyPath = join(dir, "policy.json");
  if (!existsSync(policyPath)) {
    lines.push(`policy.json: not found (seeded with the default posture on first run)`);
  } else {
    try {
      const raw = JSON.parse(readFileSync(policyPath, "utf8"));
      config = NetConfig.parse(raw);
      lines.push(
        `ok: policy.json — mode ${config.mode}, ${config.allowHosts.length} allowed host(s), ` +
          `${config.rules.length} rule(s), ${config.plugins.length} plugin activation(s)`,
      );
    } catch (e) {
      if (e instanceof z.ZodError) {
        for (const issue of e.issues) {
          error(`policy.json ${issue.path.join(".") || "(root)"}: ${issue.message}`);
        }
      } else {
        error(`policy.json: ${(e as Error).message}`);
      }
    }
  }

  // 2. the plugin library — every file must load (fixtures run inside load()).
  const host = new PluginHost(pluginsDir(dir));
  await host.load();
  const infos = host.list();
  for (const p of infos) {
    if (p.status === "error") {
      error(`plugin ${p.name}: ${p.error}`);
    } else {
      const parts = [
        p.ops ? `${p.ops.length} op(s)` : undefined,
        p.hasClassify ? "classify()" : undefined,
        p.hasExtract ? "extract()" : undefined,
        p.hasGate ? "gate()" : undefined,
        `${p.fixtures} fixture(s)`,
      ].filter(Boolean);
      lines.push(`ok: plugin ${p.name} [${p.hosts.join(", ")}] — ${parts.join(", ")}`);
    }
  }
  if (infos.length === 0) lines.push("plugin library: empty");

  // 3. cross-references — inert-at-runtime configs the operator should know about.
  if (config) {
    const loaded = new Map(
      infos.filter((p) => p.status === "loaded").map((p) => [p.name, p]),
    );
    for (const a of config.plugins) {
      if (!loaded.has(a.name)) {
        warn(`activation "${a.name}" names a plugin that isn't loaded — it gates nothing`);
      } else if (a.expires !== undefined && Date.parse(a.expires) <= Date.now()) {
        warn(`activation "${a.name}" expired ${a.expires} — its hosts fall back to the host gate`);
      }
    }
    for (const rule of config.rules) {
      for (const approver of rule.approve ?? []) {
        if (!approver.startsWith("plugin:")) continue;
        const name = approver.slice("plugin:".length);
        const p = loaded.get(name);
        if (!p) {
          warn(`rule "${rule.name}" approver ${approver} isn't loaded — matches will DENY`);
        } else if (!p.hasGate) {
          warn(`rule "${rule.name}" approver ${approver} has no gate() — matches will DENY`);
        } else if (!config.plugins.some((a) => a.name === name)) {
          warn(`rule "${rule.name}" approver ${approver} has no activation — matches will DENY`);
        }
      }
    }
  }

  lines.push(errors === 0 ? "valid" : `${errors} error(s)`);
  return { ok: errors === 0, lines };
}

if (import.meta.main) {
  const report = await validateNet();
  console.log(report.lines.join("\n"));
  Deno.exit(report.ok ? 0 : 1);
}
