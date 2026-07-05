/**
 * AI-drafted rule sets. Given a plain-language intent ("research chocolate shops in
 * Boston with exa") and the branch's current effective config + recent egress, the
 * model proposes a least-privilege NetConfig the user reviews in the rule editor —
 * nothing is enforced until they save it. The same path refines rules around a live
 * request: the session's recent (including pending) rows ride along as context, so
 * "let this through but keep writes held" has the request in front of the model.
 *
 * The response is Zod-validated against NetConfig; one retry with the parse error
 * appended keeps a malformed first answer from failing the request.
 */
import { z } from "zod/v4";
import { NetConfig } from "./config.ts";
import type { LlmClient } from "../supervisor/llm.ts";
import type { NetRequest } from "../schema/parts.ts";

export const Suggestion = z.object({
  config: NetConfig,
  /** One short paragraph: what was allowed/held and why. Shown above the editor. */
  rationale: z.string(),
});
export type Suggestion = z.infer<typeof Suggestion>;

const SYSTEM = [
  "You design egress firewall rule sets for bough's Claw Patrol proxy. Every HTTPS",
  "request a sandboxed command makes is MITM-terminated, classified, and gated by a",
  "config with these fields:",
  '- mode: baseline for allowed hosts — "read_only" (writes deny), "review" (writes hold for human approval), "all".',
  "- allowHosts: trusted hostnames (exact, no wildcards). A host not listed gets hostMiss.",
  "- denyHosts: hosts denied outright (win over allowHosts).",
  '- hostMiss: verdict for a host missing from a non-empty allowHosts — "allow" | "deny" | "hold".',
  "- k8sHosts: API-server hosts to classify as kubernetes (HTTP verb = action).",
  "- allowVerbs / denyVerbs / holdVerbs: per-action overrides on classified verbs.",
  "- rules: ordered condition rules, evaluated FIRST (first match wins). Each is",
  '  {"name", "condition", "verdict" ("allow"|"deny"|"hold"), "hosts"? (scope), "reason"?}.',
  "  The condition is a CEL-like expression over facets: http (method UPPERCASE, path,",
  "  query, headers, body, body_json), action (service, verb, kind), and the classifier's",
  "  facet when present (k8s: verb/resource/namespace/name; graphql: operation). Operators:",
  "  == != && || ! in [..], .startsWith() .endsWith() .contains() .matches(), has(a.b).",
  '  e.g. {"name": "no-exec", "condition": "has(k8s.resource) && k8s.resource.endsWith(\'/exec\')", "verdict": "deny"}.',
  "  Unevaluable conditions fail closed. Prefer a rule over a verb list when the intent",
  "  needs request SHAPE (a body field, a path prefix, a k8s resource), verb lists for",
  "  exact classified verbs.",
  "- bundles: metadata; copy it from the base config unchanged.",
  "Verb grammar (what classification produces):",
  '- generic HTTP: "<METHOD> <path>" e.g. "GET /v1/search", "DELETE /repos/o/r"',
  '- GraphQL (any host, path ending /graphql — the proxy reads the decrypted body): "graphql:query" or "graphql:mutation"',
  '- AWS: the operation name, e.g. "TerminateInstances"',
  '- kubernetes (hosts in k8sHosts): "<METHOD> <resource-path>"',
  "Principles: least privilege for the stated task. Prefer a tight allowHosts plus",
  'hostMiss "hold" (fail-closed-but-approvable) over broad allows. Keep writes held',
  'unless the task clearly needs them (e.g. holdVerbs ["graphql:mutation"] while reads flow).',
  "Include package registries or auxiliary hosts ONLY if the task plainly needs them.",
  "Start from the BASE CONFIG and change as little as necessary; never drop entries the",
  "user plainly still needs (e.g. hosts appearing in recent allowed traffic) unless asked",
  "to be stricter.",
  'Respond with ONLY a JSON object {"config": <NetConfig>, "rationale": "<short paragraph>"}.',
  "No markdown fences, no commentary outside the JSON.",
].join("\n");

/** Strip ```json fences if the model wrapped its answer anyway. */
function unfence(text: string): string {
  const m = text.trim().match(/^```(?:json)?\s*([\s\S]*?)\s*```$/);
  return m ? m[1] : text.trim();
}

function requestLines(recent: NetRequest[]): string {
  return recent
    .map((r) => `- ${r.verdict.toUpperCase().padEnd(7)} ${r.host}  ${r.action}`)
    .join("\n");
}

export async function suggestPolicy(opts: {
  llm: LlmClient;
  model: string;
  /** The user's plain-language intent or refinement instruction. */
  intent: string;
  /** The effective config the proposal starts from. */
  base: NetConfig;
  /** The branch's recent egress, newest (incl. any pending hold) first. */
  recent?: NetRequest[];
  /**
   * Requests the user hand-picked from the feed to be grouped into rules. When set,
   * the proposal must cover exactly these (hosts/verbs generalized sensibly), with
   * `recent` demoted to ambient context.
   */
  selected?: NetRequest[];
}): Promise<Suggestion> {
  const context = [
    `BASE CONFIG:\n${JSON.stringify(opts.base, null, 2)}`,
    opts.selected?.length
      ? `SELECTED REQUESTS (group exactly these into rules — generalize hosts/verbs only as far as the pattern they form):\n${
        requestLines(opts.selected)
      }`
      : "",
    opts.recent?.length
      ? `RECENT REQUESTS (newest first; PENDING rows are awaiting approval right now):\n${
        requestLines(opts.recent)
      }`
      : "",
    `TASK / INSTRUCTION:\n${opts.intent}`,
  ].filter(Boolean).join("\n\n");

  let prompt = context;
  let lastError = "";
  for (let attempt = 0; attempt < 2; attempt++) {
    const result = await opts.llm.run(
      {
        model: opts.model,
        system: SYSTEM,
        maxTokens: 4_000,
        messages: [{ role: "user", content: [{ type: "text", text: prompt }] }],
        tools: [],
      },
      () => {},
    );
    const text = result.content
      .filter((b) => b.type === "text")
      .map((b) => (b as { text: string }).text)
      .join("");
    try {
      return Suggestion.parse(JSON.parse(unfence(text)));
    } catch (e) {
      lastError = (e as Error).message;
      prompt = `${context}\n\nYour previous answer failed to parse (${
        lastError.slice(0, 300)
      }). Respond with ONLY the JSON object, nothing else.`;
    }
  }
  throw new Error(`model returned no valid suggestion: ${lastError.slice(0, 300)}`);
}
