/**
 * Structured agent output — the reliability mechanism under fan-out.
 *
 * WHY THIS EXISTS. A workflow's only machine-readable product is what its agents
 * report, and there is no acceptance gate anywhere in bough (spec §17): the model
 * says what it did and the user verifies. So a 300-agent audit that reports prose
 * gives the script nothing to branch on but string matching, and one agent that
 * decides to answer in a table silently changes the shape of the whole run.
 * `agent(prompt, {schema})` is the fix — the call resolves to a PARSED, VALIDATED
 * object and the script branches on typed data (spec §8, plan §8.2).
 *
 * THE INVARIANT THIS HOLDS: **a schema mismatch retries; an exhausted retry fails
 * the call.** `agent()` either resolves with an object that validates against the
 * supplied schema or it throws — it never resolves with junk. That matters more
 * than it sounds: `parallel()` maps a throwing thunk to `null` and `pipeline()`
 * drops the item, so a *thrown* schema failure is visible in the result and is
 * handled by combinators the script already uses, whereas a malformed object
 * resolved into a slot propagates as a `TypeError` three stages later, in a
 * detached run nobody is watching.
 *
 * Two consequences shape the code below:
 *
 *   - The schema itself is rejected BEFORE the first subagent launches. An
 *     unsupported schema is an authoring mistake, and finding out mid-run — after
 *     forty agents have billed — is the expensive way to learn it (plan T5.3).
 *   - Success returns the CANONICAL JSON text of the validated value, not the
 *     subagent's raw report. `harness/wf_worker.ts` does `JSON.parse(report)` when
 *     `schema` is set, and `workflow/run.ts` journals the same string, so a
 *     replayed call and a live one must be byte-identical to the script. Handing
 *     back a fenced markdown block would work live and fail on replay.
 *
 * ---------------------------------------------------------------------------
 * BUILD VS BUY — the call, and why.
 * ---------------------------------------------------------------------------
 * Plan §1 says do not hand-roll structured outputs: the Anthropic SDK ships
 * `output_config.format` / `client.messages.parse()`, with validation and model
 * retry at the API layer and schemas cached server-side. That is the right default
 * and it is what this module is SHAPED like. It is not what this module CALLS, for
 * three reasons, in order of how hard they are to work around:
 *
 *   1. **The pinned SDK does not have it.** `deno.json` pins
 *      `@anthropic-ai/sdk@0.68.0`, whose `resources/messages` declares no
 *      `output_config`, no `parse()`, and no `zodOutputFormat` — only the tool
 *      runner's `betaZodTool`. `deno.json` is frozen (plan §4), so a task that
 *      needs a newer dependency stops and asks rather than editing the import map.
 *   2. **`LlmParams` has no slot for it.** `types.ts` is frozen and its provider
 *      boundary carries `system`/`messages`/`tools`/`toolChoice`/`effort` and
 *      nothing else, so there is no way to pass an output format through
 *      `LlmClient` without editing the interface every provider satisfies.
 *   3. **A subagent's report is not one model response.** It is the final text of a
 *      whole turn — many rounds of `run_steps`, ending in prose plus `stop` (spec
 *      §5, §6). Response-level format constraint applies to a request; the thing
 *      being constrained here is an agent's conclusion after it has finished
 *      working. And two of bough's three provider routes are not Anthropic at all
 *      (spec §12), so an Anthropic-only path would leave OpenAI and OpenRouter
 *      workflows silently unvalidated.
 *
 * So: validate-and-retry here, built to the SDK's contract rather than around it.
 * `checkOutputSchema` enforces exactly the documented limits of the API's
 * structured-output schemas — no recursion, no numeric or string-length
 * constraints, `additionalProperties: false` on every object — so a script written
 * against this module is already API-legal the day items 1–3 are resolved and this
 * validator can be deleted in favour of the SDK's. The one deliberate divergence:
 * the SDKs silently STRIP unsupported constraints and check them client-side,
 * where this rejects them by name at submit. Stripping is the wrong trade for a
 * detached fan-out — a `minItems: 3` that quietly stops constraining the model is a
 * script whose author believes something about the data that is not true.
 *
 * Everything here is pure except `structuredRunner`, which is a decorator over the
 * injected `AgentRunner` and therefore drivable offline with no LLM and no key.
 *
 * Write fresh (plan T5.3). There is no `src/` predecessor.
 */

import { WorkflowError } from "../errors.ts";
import type { AgentCall, AgentRunner, WorkflowCtx } from "./run.ts";

// ---------------------------------------------------------------------------
// Limits
// ---------------------------------------------------------------------------

/**
 * Attempts per schema-bearing `agent()` call, INCLUDING the first — 3 means one
 * try and two retries. Each attempt is a whole subagent turn, so this is a real
 * multiplier on a fan-out's bill; two retries is the point where "the model
 * slipped" has been ruled out and the schema is the more likely suspect.
 */
export const DEFAULT_ATTEMPTS = 3;

/** Env-overridable so the exhaustion path is testable without three real turns. */
export function structuredAttempts(): number {
  const n = Number(Deno.env.get("BOUGH_SCHEMA_ATTEMPTS"));
  return Number.isFinite(n) && n >= 1 ? Math.floor(n) : DEFAULT_ATTEMPTS;
}

/** How many validation errors travel back to the model, and into the final error. */
export const MAX_ERRORS = 12;

/** How much of a malformed report is quoted back. Enough to see the shape. */
const REPORT_CLIP = 800;

function clip(s: string, n: number): string {
  return s.length > n ? `${s.slice(0, n - 1)}…` : s;
}

// ---------------------------------------------------------------------------
// Schema validation (pure) — the submit-time gate
// ---------------------------------------------------------------------------

type Json = Record<string, unknown>;

const isObj = (v: unknown): v is Json =>
  typeof v === "object" && v !== null && !Array.isArray(v);

/** The seven types the API's structured-output schemas accept. */
const TYPES = ["object", "array", "string", "integer", "number", "boolean", "null"] as const;

/** Structural keywords this validator understands and the API accepts. */
const SUPPORTED = new Set([
  "type",
  "properties",
  "required",
  "additionalProperties",
  "items",
  "enum",
  "const",
  "anyOf",
  "allOf",
  "$ref",
  "$defs",
  "definitions",
]);

/** Annotations: carried to the model in the contract, never enforced. */
const ANNOTATIONS = new Set([
  "title",
  "description",
  "default",
  "examples",
  "format",
  "$comment",
  "$schema",
  "$id",
]);

/**
 * Keywords that are rejected by name rather than ignored, with the reason. Each
 * message says the move, because "unsupported" alone leaves the author guessing
 * whether to drop the keyword or the whole approach (spec §6: error text is a
 * product surface).
 */
const REJECTED: Record<string, string> = {
  minimum: "numeric bounds are not supported",
  maximum: "numeric bounds are not supported",
  exclusiveMinimum: "numeric bounds are not supported",
  exclusiveMaximum: "numeric bounds are not supported",
  multipleOf: "numeric bounds are not supported",
  minLength: "string-length bounds are not supported",
  maxLength: "string-length bounds are not supported",
  pattern: "regex constraints are not supported",
  minItems: "array-length bounds are not supported",
  maxItems: "array-length bounds are not supported",
  uniqueItems: "array uniqueness is not supported",
  contains: "array `contains` is not supported",
  minContains: "array `contains` is not supported",
  maxContains: "array `contains` is not supported",
  prefixItems: "tuple schemas are not supported",
  oneOf: "`oneOf` is not supported — use `anyOf`",
  not: "`not` is not supported",
  if: "conditional schemas are not supported",
  then: "conditional schemas are not supported",
  else: "conditional schemas are not supported",
  patternProperties: "`patternProperties` is not supported",
  propertyNames: "`propertyNames` is not supported",
  dependentSchemas: "schema dependencies are not supported",
  dependentRequired: "schema dependencies are not supported",
  unevaluatedProperties: "`unevaluatedProperties` is not supported",
  unevaluatedItems: "`unevaluatedItems` is not supported",
};

const ADVICE = "The model is not constrained by it, so leaving it in would promise the " +
  "script something the schema cannot deliver. Drop the keyword and check the value in " +
  "the script instead.";

/**
 * Check a script's JSON Schema against the subset structured outputs accept.
 * Returns the message to hand the author, or `null` when the schema is usable.
 *
 * Pure, and separate from `assertOutputSchema` so a caller can look before it
 * leaps — a route or a linter can report every schema in a script without
 * throwing.
 */
export function checkOutputSchema(schema: unknown): string | null {
  if (!isObj(schema)) {
    return "agent(prompt, {schema}): schema must be a JSON Schema object — e.g. " +
      `{type: "object", properties: {…}, required: […], additionalProperties: false}`;
  }
  if (schema.type !== "object") {
    return "agent(prompt, {schema}): the schema's root must be `type: \"object\"`. A bare " +
      "array or scalar root is not accepted; wrap it, e.g. " +
      `{type: "object", properties: {items: {type: "array", items: …}}, ` +
      `required: ["items"], additionalProperties: false}.`;
  }
  try {
    walkSchema(schema, "", schema, []);
    return null;
  } catch (err) {
    if (err instanceof SchemaRejection) return `agent(prompt, {schema}): ${err.message}`;
    throw err;
  }
}

/**
 * Reject an unusable schema at SUBMIT time — before a subagent is launched, before
 * a semaphore slot is taken, before anything bills. Throws `WorkflowError(400)`,
 * which the worker bridge turns into a catchable exception inside the script.
 */
export function assertOutputSchema(schema: unknown): void {
  const bad = checkOutputSchema(schema);
  if (bad) throw new WorkflowError(400, bad);
}

class SchemaRejection extends Error {}

function reject(path: string, message: string): never {
  throw new SchemaRejection(path ? `${message} (at \`${path || "/"}\`)` : message);
}

/** `refs` is the `$ref` chain currently being expanded — the recursion detector. */
function walkSchema(node: unknown, path: string, root: Json, refs: string[]): void {
  const at = path || "the root";
  if (!isObj(node)) {
    reject(path, `every subschema must be an object; \`${at}\` is ${describe(node)}`);
  }

  for (const key of Object.keys(node)) {
    if (key in REJECTED) {
      reject(path, `\`${key}\` is not supported in a structured-output schema — ` +
        `${REJECTED[key]}. ${ADVICE}`);
    }
    if (!SUPPORTED.has(key) && !ANNOTATIONS.has(key)) {
      reject(path, `unknown schema keyword \`${key}\`. Supported: ` +
        `${[...SUPPORTED].join(", ")} (plus title/description/format, which are ` +
        `passed to the model as documentation).`);
    }
  }

  if (typeof node.$ref === "string") {
    const name = refName(node.$ref, path);
    if (refs.includes(name)) {
      reject(path, `recursive schema: \`$ref\` to \`${name}\` re-enters itself ` +
        `(${[...refs, name].join(" → ")}). Structured outputs cannot express recursion — ` +
        `flatten the shape to a fixed depth, or return the nesting as a list of nodes ` +
        `with parent ids.`);
    }
    const defs = defsOf(root);
    const target = defs[name];
    if (target === undefined) {
      reject(path, `\`$ref\` points at \`${node.$ref}\`, which the schema does not define. ` +
        `Add it under \`$defs\`.`);
    }
    walkSchema(target, `${path}/$ref(${name})`, root, [...refs, name]);
    return;
  }

  for (const key of ["$defs", "definitions"] as const) {
    const defs = node[key];
    if (defs !== undefined && !isObj(defs)) {
      reject(path, `\`${key}\` must be an object of named subschemas`);
    }
  }

  for (const key of ["anyOf", "allOf"] as const) {
    const branches = node[key];
    if (branches === undefined) continue;
    if (!Array.isArray(branches) || branches.length === 0) {
      reject(path, `\`${key}\` must be a non-empty array of subschemas`);
    }
    branches.forEach((b, i) => walkSchema(b, `${path}/${key}/${i}`, root, refs));
  }

  if (node.enum !== undefined && (!Array.isArray(node.enum) || node.enum.length === 0)) {
    reject(path, "`enum` must be a non-empty array of allowed values");
  }

  const type = node.type;
  if (type === undefined) {
    // A pure combinator node (anyOf/allOf/enum/const) is fine; a node with no
    // constraint at all would validate anything, which is not a schema.
    const constrained = node.anyOf !== undefined || node.allOf !== undefined ||
      node.enum !== undefined || node.const !== undefined;
    if (!constrained) {
      reject(path, `\`${at}\` declares no \`type\` — an unconstrained subschema accepts ` +
        `anything, which defeats the point of passing a schema`);
    }
    return;
  }
  if (Array.isArray(type)) {
    reject(path, "a `type` array is not supported — express a union with `anyOf`, e.g. " +
      `anyOf: [{type: "string"}, {type: "null"}]`);
  }
  if (typeof type !== "string" || !(TYPES as readonly string[]).includes(type)) {
    reject(path, `unknown \`type\`: ${JSON.stringify(type)}. One of ${TYPES.join(", ")}.`);
  }

  if (type === "object") {
    if (node.additionalProperties !== false) {
      reject(path, `every object must set \`additionalProperties: false\` — ` +
        `\`${at}\` ${node.additionalProperties === undefined ? "omits it" : "sets it to " +
          JSON.stringify(node.additionalProperties)}. A closed object is what makes an ` +
        `extra invented field a validation failure instead of silent noise in the result.`);
    }
    const props = node.properties;
    if (!isObj(props) || Object.keys(props).length === 0) {
      reject(path, `\`${at}\` is an object with no \`properties\``);
    }
    const required = node.required;
    if (required !== undefined) {
      if (!Array.isArray(required) || required.some((r) => typeof r !== "string")) {
        reject(path, "`required` must be an array of property names");
      }
      for (const name of required as string[]) {
        if (!(name in props)) {
          reject(path, `\`required\` names \`${name}\`, which is not in \`properties\``);
        }
      }
    }
    for (const [name, sub] of Object.entries(props)) {
      walkSchema(sub, `${path}/${name}`, root, refs);
    }
    return;
  }

  if (type === "array") {
    if (node.items === undefined) {
      reject(path, `\`${at}\` is an array with no \`items\` schema — say what the ` +
        `elements are, or the script gets a list of anything`);
    }
    if (Array.isArray(node.items)) {
      reject(path, "an `items` array (tuple form) is not supported — use one `items` schema");
    }
    walkSchema(node.items, `${path}/items`, root, refs);
  }
}

function refName(ref: string, path: string): string {
  const m = /^#\/(?:\$defs|definitions)\/([^/]+)$/.exec(ref);
  if (!m) {
    reject(path, `\`$ref\` must be a local reference of the form \`#/$defs/Name\`; ` +
      `got \`${ref}\``);
  }
  return m[1];
}

function defsOf(root: Json): Json {
  const a = isObj(root.$defs) ? root.$defs : {};
  const b = isObj(root.definitions) ? root.definitions : {};
  return { ...b, ...a };
}

function describe(v: unknown): string {
  if (v === null) return "null";
  if (Array.isArray(v)) return "an array";
  return `a ${typeof v}`;
}

// ---------------------------------------------------------------------------
// Instance validation (pure) — what the retry loop branches on
// ---------------------------------------------------------------------------

/**
 * Validate a parsed value against a schema `checkOutputSchema` has accepted.
 * Returns the errors, most useful first, capped at `MAX_ERRORS` — the list is fed
 * back to the model verbatim on the retry, and forty near-identical "missing
 * field" lines teach it less than the first few do while costing real context.
 *
 * Paths are JSON-pointer shaped (`/findings/0/title`) because the model has to
 * locate the fault in its own output from this string alone.
 */
export function validateInstance(schema: unknown, value: unknown): string[] {
  const errors: string[] = [];
  if (!isObj(schema)) return ["the schema is not an object"];
  check(schema, value, "", schema, errors);
  return errors.slice(0, MAX_ERRORS);
}

function check(node: Json, value: unknown, path: string, root: Json, errors: string[]): void {
  if (errors.length >= MAX_ERRORS) return;
  const at = path || "/";

  if (typeof node.$ref === "string") {
    const m = /^#\/(?:\$defs|definitions)\/([^/]+)$/.exec(node.$ref);
    const target = m ? defsOf(root)[m[1]] : undefined;
    if (isObj(target)) check(target, value, path, root, errors);
    return;
  }

  if (Array.isArray(node.allOf)) {
    for (const branch of node.allOf) {
      if (isObj(branch)) check(branch, value, path, root, errors);
    }
  }

  if (Array.isArray(node.anyOf)) {
    const matched = node.anyOf.some((branch) => {
      if (!isObj(branch)) return false;
      const sub: string[] = [];
      check(branch, value, path, root, sub);
      return sub.length === 0;
    });
    if (!matched) {
      errors.push(`\`${at}\`: matched none of the ${node.anyOf.length} allowed shapes`);
      return;
    }
  }

  if (node.const !== undefined && !same(node.const, value)) {
    errors.push(`\`${at}\`: expected the constant ${JSON.stringify(node.const)}, got ${show(value)}`);
    return;
  }

  if (Array.isArray(node.enum)) {
    if (!node.enum.some((allowed) => same(allowed, value))) {
      errors.push(
        `\`${at}\`: ${show(value)} is not one of ${node.enum.map((e) => JSON.stringify(e)).join(", ")}`,
      );
      return;
    }
  }

  const type = node.type;
  if (typeof type !== "string") return;

  if (!typeMatches(type, value)) {
    errors.push(`\`${at}\`: expected ${type}, got ${show(value)}`);
    return;
  }

  if (type === "object") {
    const obj = value as Json;
    const props = isObj(node.properties) ? node.properties : {};
    const required = Array.isArray(node.required) ? node.required as string[] : [];
    for (const name of required) {
      if (!(name in obj)) {
        errors.push(`\`${at}\`: missing required property \`${name}\``);
        if (errors.length >= MAX_ERRORS) return;
      }
    }
    if (node.additionalProperties === false) {
      for (const name of Object.keys(obj)) {
        if (!(name in props)) {
          errors.push(
            `\`${at}\`: unexpected property \`${name}\` — the schema declares only ` +
              `${Object.keys(props).map((p) => `\`${p}\``).join(", ")}`,
          );
          if (errors.length >= MAX_ERRORS) return;
        }
      }
    }
    for (const [name, sub] of Object.entries(props)) {
      if (!(name in obj) || !isObj(sub)) continue;
      check(sub, obj[name], `${path}/${name}`, root, errors);
      if (errors.length >= MAX_ERRORS) return;
    }
    return;
  }

  if (type === "array" && isObj(node.items)) {
    const items = value as unknown[];
    for (let i = 0; i < items.length; i++) {
      check(node.items, items[i], `${path}/${i}`, root, errors);
      if (errors.length >= MAX_ERRORS) return;
    }
  }
}

function typeMatches(type: string, value: unknown): boolean {
  switch (type) {
    case "object":
      return isObj(value);
    case "array":
      return Array.isArray(value);
    case "string":
      return typeof value === "string";
    case "integer":
      return typeof value === "number" && Number.isInteger(value);
    case "number":
      return typeof value === "number" && Number.isFinite(value);
    case "boolean":
      return typeof value === "boolean";
    case "null":
      return value === null;
    default:
      return true;
  }
}

function same(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (typeof a !== typeof b) return false;
  if (a === null || b === null) return false;
  if (typeof a === "object") return JSON.stringify(a) === JSON.stringify(b);
  return false;
}

function show(v: unknown): string {
  if (v === undefined) return "nothing";
  if (v === null) return "null";
  if (Array.isArray(v)) return `an array (${v.length} items)`;
  if (typeof v === "object") return "an object";
  return clip(JSON.stringify(v) ?? String(v), 60);
}

// ---------------------------------------------------------------------------
// Reading JSON back out of a report (pure)
// ---------------------------------------------------------------------------

/**
 * Find the JSON value in a subagent's report.
 *
 * A subagent's report is the final TEXT of a whole turn (spec §5: every turn must
 * produce user-visible text), so even a perfectly compliant agent commonly wraps
 * its answer in a fenced block, and a chatty one puts a sentence in front of it.
 * Insisting on a bare JSON body would burn retries on agents that got the data
 * right, so this reads the LAST complete JSON value in the report — the last one
 * being the conclusion, where an earlier one is usually an example the agent was
 * quoting from its own instructions.
 */
export function extractJson(report: string): { ok: true; value: unknown } | { ok: false } {
  const text = report.trim();
  if (!text) return { ok: false };

  // The whole report, when the agent did exactly what it was asked.
  const whole = tryParse(text);
  if (whole.ok) return whole;

  // Fenced blocks, last first.
  const fences = [...text.matchAll(/```(?:json|jsonc|json5)?\s*\r?\n([\s\S]*?)```/gi)];
  for (let i = fences.length - 1; i >= 0; i--) {
    const parsed = tryParse(fences[i][1].trim());
    if (parsed.ok) return parsed;
  }

  // Anything balanced left in the prose.
  const spans = balancedSpans(text);
  for (let i = spans.length - 1; i >= 0; i--) {
    const parsed = tryParse(text.slice(spans[i][0], spans[i][1]));
    if (parsed.ok) return parsed;
  }
  return { ok: false };
}

function tryParse(s: string): { ok: true; value: unknown } | { ok: false } {
  if (!s) return { ok: false };
  try {
    return { ok: true, value: JSON.parse(s) };
  } catch {
    return { ok: false };
  }
}

/**
 * Top-level `{…}` / `[…]` spans, string- and escape-aware. Nested braces are
 * skipped rather than reported, so a candidate is always an outermost value.
 */
function balancedSpans(text: string): Array<[number, number]> {
  const spans: Array<[number, number]> = [];
  for (let i = 0; i < text.length; i++) {
    const open = text[i];
    if (open !== "{" && open !== "[") continue;
    const close = open === "{" ? "}" : "]";
    let depth = 0;
    let inString = false;
    let escaped = false;
    for (let j = i; j < text.length; j++) {
      const c = text[j];
      if (inString) {
        if (escaped) escaped = false;
        else if (c === "\\") escaped = true;
        else if (c === '"') inString = false;
        continue;
      }
      if (c === '"') inString = true;
      else if (c === open) depth++;
      else if (c === close) {
        depth--;
        if (depth === 0) {
          spans.push([i, j + 1]);
          i = j;
          break;
        }
      }
    }
  }
  return spans;
}

// ---------------------------------------------------------------------------
// The prompt contract (pure)
// ---------------------------------------------------------------------------

/**
 * What is appended to a schema-bearing prompt. A workflow agent gets no context
 * beyond its prompt string (spec §8), so if the contract is not in the prompt the
 * agent has no way to know one exists — and an agent that answers the question
 * perfectly in prose has still failed the call.
 */
export function schemaContract(schema: unknown): string {
  return [
    "",
    "---",
    "RETURN FORMAT — required, and checked.",
    "",
    "Finish your report with exactly one JSON value that validates against the schema",
    "below, and write nothing after it. A ```json fenced block is fine. Every object in",
    "the schema is closed, so an extra field you invented fails the whole report; include",
    "every required field, and when you could not determine a value say so inside the",
    "structure the schema gives you rather than dropping the field or answering in prose.",
    "",
    "JSON Schema:",
    "```json",
    JSON.stringify(schema, null, 2),
    "```",
  ].join("\n");
}

/**
 * What is appended on a retry. It names what failed and quotes the report back,
 * because the agent that produced it is a FRESH session with no memory of the
 * previous attempt — a bare "try again" would re-run the same task with the same
 * information and get the same answer.
 */
export function repairContract(previous: string, errors: string[], attempt: number): string {
  return [
    "",
    "---",
    `PREVIOUS ATTEMPT REJECTED (attempt ${attempt}).`,
    "",
    "An earlier agent was given this exact task and its report did not match the schema:",
    "",
    ...errors.map((e) => `  - ${e}`),
    "",
    "Its report began:",
    "```",
    clip(previous.trim(), REPORT_CLIP),
    "```",
    "",
    "Do the work again and return a report that satisfies every point above. The schema",
    "is not negotiable — if the task genuinely cannot produce a required field, fill it",
    "with the schema-legal value that says so and explain inside the structure.",
  ].join("\n");
}

// ---------------------------------------------------------------------------
// The runner decorator
// ---------------------------------------------------------------------------

export interface StructuredOpts {
  /** Total attempts including the first. Absent = `BOUGH_SCHEMA_ATTEMPTS`, then 3. */
  attempts?: number;
}

/**
 * Wrap an `AgentRunner` so `{schema}` calls resolve to validated, canonical JSON.
 *
 * Calls WITHOUT a schema pass straight through untouched — a workflow mixes both,
 * and a prose report must not be second-guessed.
 *
 * Failure semantics, which the combinators depend on:
 *   - Unusable schema → `WorkflowError(400)` before anything launches.
 *   - Mismatch → retry with the errors fed back, up to `attempts`.
 *   - Exhausted → `WorkflowError(422)` naming the attempts, the last errors and the
 *     move. It THROWS rather than resolving, so `parallel()` slots it `null` and
 *     `pipeline()` drops the item (spec §8).
 *   - The inner runner itself rejecting (child errored, interrupted, orphaned, or
 *     the run was stopped) is NOT retried and propagates as-is. That is a
 *     different failure from "the report was the wrong shape", and retrying it
 *     would spend a stopped run's budget and hide an interrupt.
 */
export function structuredRunner(inner: AgentRunner, opts: StructuredOpts = {}): AgentRunner {
  const attempts = Math.max(1, opts.attempts ?? structuredAttempts());

  return async (call: AgentCall, signal: AbortSignal, onSpawned): Promise<string> => {
    if (call.schema === undefined) return await inner(call, signal, onSpawned);

    // Submit time: before the first launch, before a semaphore slot, before cost.
    assertOutputSchema(call.schema);

    const contract = schemaContract(call.schema);
    let previous = "";
    let errors: string[] = [];

    for (let attempt = 1; attempt <= attempts; attempt++) {
      if (signal.aborted) throw new WorkflowError(409, "workflow stopped");

      const prompt = attempt === 1
        ? `${call.prompt}\n${contract}`
        : `${call.prompt}\n${contract}\n${repairContract(previous, errors, attempt - 1)}`;

      const report = await inner({ ...call, prompt }, signal, onSpawned);

      const found = extractJson(report);
      if (!found.ok) {
        previous = report;
        errors = [
          "the report contained no JSON value at all — the whole answer was prose",
        ];
        continue;
      }

      const bad = validateInstance(call.schema, found.value);
      if (bad.length === 0) {
        // CANONICAL text, not the raw report: the worker parses this and the
        // journal replays it, so live and replayed calls must be identical.
        return JSON.stringify(found.value);
      }
      previous = report;
      errors = bad;
    }

    throw new WorkflowError(
      422,
      `agent(prompt, {schema}) failed after ${attempts} attempt(s): the subagent's report ` +
        `never matched the schema. Last mismatch:\n${errors.map((e) => `  - ${e}`).join("\n")}\n` +
        `Last report began: ${clip(previous.trim() || "(empty)", 300)}\n` +
        `A schema the agent cannot satisfy is usually asking for something the task has no ` +
        `way to know — simplify the schema, split the work, or say in the prompt where the ` +
        `agent should look for the missing field.`,
    );
  };
}

/**
 * The boot seam. `main.ts` fills it and the workflow start path (T5.5) reads it,
 * so every `WorkflowCtx` built in this process gets structured output without the
 * call site having to remember — the same shape as `WithTurnStarter` in
 * `server/sessions.ts`. A reader that finds it absent falls back to the identity,
 * which is exactly the pre-T5.3 behavior.
 */
export interface WithStructuredWorkflow {
  workflowCtx?: (base: WorkflowCtx) => WorkflowCtx;
}

/** Apply the decorator to a workflow context. Idempotent in effect, not in identity. */
export function structuredWorkflowCtx(base: WorkflowCtx, opts: StructuredOpts = {}): WorkflowCtx {
  return { ...base, runner: structuredRunner(base.runner, opts) };
}
