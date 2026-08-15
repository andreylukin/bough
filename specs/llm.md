# Port spec: `llm` — the provider boundary

Source files (all under `src/llm/`): `client.ts` (1247 ln), `stream.ts` (181 ln),
`pricing.ts` (119 ln) + `pricing.json` (vendored catalog, ~37k ln), `trace.ts` (203 ln).
Shared types live in `src/types.ts` ("the LLM boundary" section) and `src/errors.ts`
(`LlmError`). Tests: `client.test.ts`, `stream.test.ts`, `pricing.test.ts`, `trace.test.ts`
— behavioral contracts mined below.

---

## 1. Purpose & invariants

One trait (`LlmClient`) hides five providers behind one `run()`. The rest of the tree —
turn runner, subagent launcher, history ops, TUI — never learns which provider it is
talking to.

Invariant comments, verbatim:

**client.ts** (module header):
> The provider boundary. Everything that knows Anthropic, OpenAI or OpenRouter exists is
> in this file, and the only thing that leaves it is `LlmClient`.
>
> The invariant: **the turn runner must not know which provider it is talking to.** Three
> wire protocols, three message encodings, three usage shapes, three ways of admitting
> that a stream died — all of it collapses to one `run()`. If a provider name, an
> `openai:` prefix check, or a `cache_control` block appears in any file outside `llm/`,
> the leak does not stay local […]
>
> Routing is by model id and nothing else (spec §12):
> - `openai:gpt-5`        → OpenAI proper, the Responses API
> - `cerebras:gpt-oss-120b` → Cerebras Inference, the chat-completions API
> - `@cf/vendor/model`    → Cloudflare Workers AI, the chat-completions API
> - `vendor/model`        → OpenRouter, the chat-completions API
> - `claude-opus-5`       → Anthropic, the official SDK
>
> **Retries are part of the boundary, not part of the runner.** Every client is wrapped in
> `withRetries`, which is sound because a round has no side effects until `run()` resolves
> — the turn loop executes tools afterwards — so re-sending identical params can at worst
> repeat streamed text deltas. That is what `onRetry` is for: the caller resets its
> streaming buffer and emits `message.retry`.
>
> **Nothing here reaches for a global.** The API key reader and `fetch` are injected […]
> Keys are read at `run()` time, not at construction, so a key set through the running
> server applies without a restart.

**stream.ts** (module header — "all of them learned the hard way"):
> **1. A stream that stops without its completion marker is a failure, not a short
> answer.** […] Returning the partial round as success hands the turn runner a
> half-assembled tool call, which it then executes. `sseEvents` therefore guards every
> read with a stall timeout, and the callers treat "ended without a completion marker" as
> a retryable transport fault.
>
> **2. A tool call with missing arguments was truncated; it is not a call with no
> arguments.** `parseToolArgs` refuses to invent `{}` for a tool whose schema has required
> fields. […] `{}` stays legitimate for a tool that requires nothing — the schema, not the
> emptiness, decides.
>
> **3. Reasoning is persisted WITH its provider payload.** […] providers hand back a
> thinking block whole or not at all, and they reject one whose content was altered rather
> than one that was merely read.
>
> Nothing here knows a provider by name — the provider string is a label for error text,
> and `meta` is never opened, only carried.

**pricing.ts**:
> **a price is a lookup, never a negotiation**: `pricing.json` is a snapshot committed to
> the repo, so a cost figure never depends on the network being up […] A model the
> snapshot does not know is reported as `null` — an honest "we don't price this" — rather
> than silently costed at zero, because a zero would read as "free" in the status bar and
> that is a lie the user cannot detect.
>
> Second invariant […]: **the catalog is keyed by the same routing the client uses.**
> `client.ts` decides which provider a model id belongs to; this file has to reach the
> same conclusion to find the row […] if they drift, the catalog silently stops pricing a
> whole provider and every cost quietly becomes `null`.

**trace.ts**:
> Raw provider I/O, on disk, for harness experiments. […] this decorator writes the
> request and the response verbatim, per round, including the rounds that FAILED — an
> error is evidence too, and the retry wrapper would otherwise swallow it.
>
> OFF UNLESS ASKED. No `BOUGH_TRACE_DIR`, no sink, no cost […]
>
> WHY THE DECORATOR SITS INSIDE THE RETRIES. `clientFor` composes
> retries(trace(pricing(provider))): outside the retries it would record one line per
> `run()` and silently collapse five failed attempts into the sixth's success.

Composition order (load-bearing): `clientFor` = **withRetries( withTrace( withPricing(
providerClient ) ) )** — trace inside retries (each attempt recorded), outside pricing
(recorded rounds already carry `costUsd`).

---

## 2. Public API

### Core types (from `src/types.ts`)

```ts
type Effort = "low" | "medium" | "high" | "xhigh" | "max";

type LlmBlock =
  | { type: "text"; text: string }
  | { type: "reasoning"; text: string; meta?: unknown }   // meta = opaque provider payload, replayed VERBATIM
  | { type: "tool_use"; id: string; name: string; input: unknown };

type LlmContentBlock =
  | LlmBlock
  | { type: "tool_result"; toolUseId: string; content: string; isError: boolean }
  | { type: "image"; data: string /*base64*/; mediaType: string; name: string };

interface LlmMessage { role: "user" | "assistant"; content: LlmContentBlock[]; }

interface LlmToolDef { name: string; description: string; inputSchema: Record<string, unknown>; }

interface LlmParams {
  model: string;
  system?: string;          // STABLE prefix: byte-identical across sessions per tier
  systemVolatile?: string;  // per-session suffix: own cache breakpoint
  maxTokens: number;
  messages: LlmMessage[];
  tools: LlmToolDef[];
  toolChoice?: "none";      // forbids tool calls for the round (forces text)
  effort?: Effort;
}

interface LlmResult { content: LlmBlock[]; stopReason: string; usage?: Usage; }

interface LlmClient {
  run(params: LlmParams, onText: (delta: string) => void, signal?: AbortSignal): Promise<LlmResult>;
}
```

`Usage` (zod, `src/schema/parts.ts`): `{ inputTokens: number, outputTokens: number,
reasoningTokens?: number|null, cacheReadTokens?: number|null, cacheWriteTokens?: number|null,
costUsd?: number|null }`. **`inputTokens` is normalized INCLUSIVE of cache reads and
writes on every route** (so the context meter shows true prompt size); pricing subtracts
them back out.

Normalized `stopReason` vocabulary: `"end_turn" | "tool_use" | "max_tokens"` plus any
passthrough finish_reason (e.g. `"stop"` from chat-completions is passed through as-is —
only `tool_calls`→`tool_use` and `length`→`max_tokens` are remapped).

`LlmError` (`src/errors.ts`): `class LlmError extends HttpError { constructor(message,
status = 502, retryAfterMs?: number) }`. `HttpError` sets `this.name = new.target.name`.
Default status **502** is what makes a status-less transport error retryable.

### client.ts exports

| Export | Signature | Semantics |
|---|---|---|
| `Provider` | `"anthropic" \| "openai" \| "openrouter" \| "cloudflare" \| "cerebras"` | |
| `providerFor(model)` | `string → Provider` | Pure routing: `openai:` prefix → openai; `cerebras:` prefix → cerebras; `@cf/` prefix → cloudflare (MUST test before slash); contains `/` → openrouter; else anthropic. |
| `API_KEY_ENV` | `Record<Provider, string>` | `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `OPENROUTER_API_KEY`, `CLOUDFLARE_API_KEY`, `CEREBRAS_API_KEY`. Read at `run()` time, never cached. |
| `CLOUDFLARE_ACCOUNT_ENV` | `"CLOUDFLARE_ACCOUNT_ID"` | Account id is part of the URL. |
| `Env` | `(key: string) → string \| undefined` | Injected env reader. |
| `ProviderOpts` | `{ env?: Env; fetch?: typeof fetch }` | The two seams; defaults `process.env` / global fetch. |
| `isToolProtocol400(err)` | `unknown → bool` | The ONE retryable 400: `status === 400 && /tool_calls\|tool_call_id\|must be followed by tool/i` — because `toOpenAIMessages` self-repairs the encoding, a resend succeeds. |
| `errName(err)` | `unknown → string` | `.name` if set and ≠ `"Error"`, else `constructor.name`. (Anthropic SDK classes never set `.name`.) |
| `isNetworkFault(err, depth=4)` | | True if `err.code` ∈ NETWORK_CODES, recursing through `.cause` up to 4 deep. |
| `isRetryable(err)` | `unknown → bool` | See §4. |
| `RetryOpts` | `{ onRetry?; maxAttempts?; baseDelayMs? }` | `onRetry({attempt, maxAttempts, error, delayMs})` fires after a retryable failure, before the sleep. |
| `MAX_ATTEMPTS` / `BASE_DELAY_MS` | `6` / `1000` | ~15–31 s total of jittered backoff. |
| `withRetries(inner, opts?)` | `LlmClient → LlmClient` | Retry decorator. |
| `withPricing(inner)` | `LlmClient → LlmClient` | Stamps `usage.costUsd` from catalog if usage present and `costUsd == null`. Unpriced model → `costUsd: null`, never 0. |
| `anthropicSystemBlocks(p)` | `→ TextBlockParam[] \| undefined` | `[system, systemVolatile]`, empties filtered, each with `cache_control: {type:"ephemeral", ttl:"1h"}`. `undefined` when both empty. Stable MUST precede volatile. |
| `joinedSystem(p)` | `→ string \| undefined` | `system + systemVolatile` concatenated (no separator); `undefined` when empty. For single-system-field providers. |
| `toApiMessage(m)` | `LlmMessage → Anthropic MessageParam` | Anthropic encoding (see §3). |
| `effortParams(effort?, model?)` | `→ object` | `{thinking:{type:"adaptive",display:"summarized"}, output_config:{effort}}` only when effort set AND model matches `/claude-(fable\|mythos\|sonnet\|opus)-5\|opus-4-[89]/` (or model undefined); else `{}`. Haiku 4.5 hard-400s on the param. |
| `anthropicClient(opts?)` / `openaiClient(opts?)` / `openrouterClient(opts?)` / `cloudflareClient(opts?)` / `cerebrasClient(opts?)` | `ProviderOpts → LlmClient` | Bare per-provider clients. |
| `toResponsesInput(messages)` | `LlmMessage[] → unknown[]` | Responses-API input items (see §3). |
| `fromResponsesOutput(output, tools?)` | `→ LlmBlock[]` | Decodes Responses `output`; `tools` lets a truncated call be told from an argument-less one. |
| `toOpenAIMessages(system, messages)` | `→ unknown[]` | Chat-completions messages incl. the orphan-tool_call repair pass (see §4). |
| `providerClient(model, opts?)` | `→ LlmClient` | Bare client for a model id (switch on `providerFor`). |
| `clientFor(model, opts?)` | `ClientOpts → LlmClient` | **The only entry point the rest of the tree uses.** `ClientOpts = ProviderOpts & { retry?: RetryOpts; trace?: TraceLabel \| null }`. |
| `completeText(llm, {model, system, maxTokens, prompt})` | `→ Promise<string>` | One-shot: single user message, no tools, no-op onText; returns concatenated text blocks untrimmed. |
| `ModelRow` | `{ id; label; provider }` | |
| `MODELS` | `ModelRow[]` | The curated static picker table (20 entries; every entry's id must route to its declared provider — pinned by test). |
| `filterOpenAIModels(ids)` | `string[] → ModelRow[]` | Include `/^(gpt-\|o\d\|chatgpt-)/`, exclude `/audio\|realtime\|tts\|whisper\|embed\|dall\|image\|moderation\|transcribe\|search-preview\|instruct/`, drop dated `/-\d{4}-\d{2}-\d{2}$/`, sort newest-first with **numeric digit-run comparison** (`byNewest`: split on `/(\d+)/`, digit-vs-digit compares as numbers so `gpt-5.10` > `gpt-5.6`; number-vs-word stays lexicographic so `o3` sorts above `gpt-5`), cap 25, map to `openai:`-prefixed rows. |
| `mergeModels(static, dynamic)` | | Static first, dynamic appended, deduped by id (static wins). |
| `discoverOpenAIModels` / `discoverAnthropicModels` / `discoverOpenRouterModels` / `discoverCloudflareModels` / `discoverCerebrasModels` | `ProviderOpts → Promise<ModelRow[]>` | Picker discovery. **Never throws, never caches**; any failure (no key, bad key, non-2xx, garbage body, dead socket) → `[]`. 10 s timeout (`AbortSignal.timeout(10_000)`). |
| `discoverModels(opts?)` | | All five concurrently via `Promise.allSettled`; rejected slots contribute `[]`. |

### stream.ts exports

| Export | Semantics |
|---|---|
| `STALL_TIMEOUT_MS = 60_000` | No bytes for 60 s → stream treated as dropped. |
| `SseOpts = { stallMs? }` | Tests turn the stall down to ms. |
| `sseEvents(body, provider, opts?)` | Async iterator of raw SSE `data:` payloads **including the `[DONE]` sentinel**. Each `read()` guarded by the stall timer; on stall, cancel the reader and throw status-less (502 ⇒ retryable) `LlmError("… stream stalled (no data for Ns)")`. Splits on `\n`, trims lines, skips non-`data:` lines (comments/keepalives/`event:`), yields `line[5..].trim()`. A trailing un-newlined fragment at stream end is DROPPED (incomplete by definition; the completion-marker check catches the truncation). |
| `throwHttpError(provider, res)` | Non-2xx → `LlmError(\`${provider}: ${status} ${bodyText}\`.trimEnd(), status, retryAfterSecs*1000?)`. Retry-After parsed from header as seconds; invalid/absent → undefined. |
| `parseToolArgs(provider, raw, tool, name)` | raw present + parses → value. raw present + malformed JSON → `LlmError("provider: name call has malformed arguments (truncated mid-call)")` (status-less ⇒ retryable). raw absent: if `tool.inputSchema.required` is a non-empty array → `LlmError("… call arrived with no arguments (truncated mid-call)")`; else (incl. unknown tool, no schema) → `{}`. |
| `blocksToParts(blocks, model?)` | Finished round → persisted `Part[]`. text kept if non-empty; reasoning kept if `text.trim()` non-empty OR `meta !== undefined` (redacted thinking has no text but must be echoed back whole), persisted as `{type:"reasoning", text, meta, model}` — `model` stamps replay validity (a signature is only valid for the model that produced it; no model ⇒ display-only); tool_use → `{type:"tool_call", id, name, input}` (kept even with `{}` input). |

### pricing.ts exports

| Export | Semantics |
|---|---|
| `CostRates` | `{ input, output, cacheRead, cacheWrite }` USD per **million** tokens. |
| `BillableTokens` | `{ inputTokens, outputTokens, cacheReadTokens?, cacheWriteTokens? }` (nullish = 0). |
| `catalogKeys(model)` | Candidate catalog keys, most specific first — MUST mirror `providerFor`: `openai:x` → `["openai/x"]`; `cerebras:x` → `["cerebras/x"]`; `@cf/x` → `["cloudflare-workers-ai/@cf/x"]` (note: full id including `@cf/` after the prefix); has `/` → `["openrouter/vendor/model", "vendor/model"]` (models.dev lists many vendors directly; the bare row is the fallback); bare → `["anthropic/x"]`. |
| `catalogKey(model)` | First candidate present in the catalog, or undefined. |
| `isPriced(model)` | `catalogKey !== undefined`. |
| `ratesFor(model)` | `CostRates \| null`. Null cache slots fall back to the input rate. |
| `contextWindowFor(model)` | `number \| null` — used by turn runner to name the limit in context-overflow errors; unknown must stay null (never a plausible default). |
| `usageCostUsd(model, u)` | `null` if unpriced. Else `fresh = max(0, inputTokens - read - write)` (clamped — over-counted cache must not go negative); `(fresh*input + read*cacheRead + write*cacheWrite + outputTokens*output) / 1e6`. |

### trace.ts exports

| Export | Semantics |
|---|---|
| `TraceLabel` | `{ dir, sessionId, turnId }`. |
| `traceLabel(sessionId, turnId, env?)` | `BOUGH_TRACE_DIR` (trimmed) set → label, else null. |
| `tracePath(label)` | `dir/sessionId/turnId.jsonl`. |
| `manifestPath(label)` | `dir/sessionId/turnId.manifest.json`. |
| `TurnManifest` | `{ sessionId, turnId, model, effort?, workspace?, sections: SectionSha[], startedAt }` — written from where prompt assembly happened (this module cannot know section identity). |
| `writeManifest(label, manifest)` | Pretty-printed JSON, mkdir -p, **all write errors swallowed** (diagnostic, never fatal). |
| `RoundRecord` | See §3. |
| `withTrace(inner, label)` | `label === null` → returns `inner` **identity-untouched** (test pins `strictEqual`). Else per-run: emits each prefix tier's text ONCE per turn (`{type:"prompt", tier:"system"\|"volatile", sha, text}` keyed by sha via a `seen` set), then appends one round line per attempt, counting failed attempts in `n` (1-based, monotonic per wrapped client). Success line has `response`; failure line has `error: {name: constructor.name \| typeof, message}` and rethrows. |

Prefix sha = `sectionSha(text)` from `src/prompt/assemble.ts`: **sha256 hex truncated to
16 chars**, computed over `params.system ?? ""` / `params.systemVolatile ?? ""`.

---

## 3. Data structures & wire shapes

No DB tables. Filesystem: only trace JSONL/manifest (above). `pricing.json`: flat object,
key `"provider/model-id"`, value `[inputUsdPerM, outputUsdPerM, cacheReadUsdPerM|null,
cacheWriteUsdPerM|null, contextWindowTokens|null]`, auto-derived from a models.dev
snapshot.

### 3a. Anthropic — Messages API (the TS uses the SDK; Rust talks raw HTTP)

Request `POST {ANTHROPIC_API_BASE|https://api.anthropic.com}/v1/messages`, headers
`x-api-key: <key>`, `anthropic-version: 2023-06-01`, `content-type: application/json`.
Key env: `ANTHROPIC_API_KEY`, alternative `ANTHROPIC_AUTH_TOKEN` (checked in that order;
values trimmed; first non-empty wins). Body:

```jsonc
{
  "model": "...", "max_tokens": N, "stream": true,
  "system": [                                     // anthropicSystemBlocks; omit when undefined
    {"type":"text","text":STABLE,  "cache_control":{"type":"ephemeral","ttl":"1h"}},
    {"type":"text","text":VOLATILE,"cache_control":{"type":"ephemeral","ttl":"1h"}}
  ],
  "messages": [...],                              // toApiMessage per message; see below
  "tools": [{"name","description","input_schema": {...}}],
  "tool_choice": {"type":"none"},                 // only when params.toolChoice set
  // effortParams spread (only for supported models):
  "thinking": {"type":"adaptive","display":"summarized"},
  "output_config": {"effort":"high"}
}
```

**Cache breakpoints — three, order load-bearing (budget is four; longer TTLs must precede
shorter):** (1) stable system block @1h, (2) volatile system block @1h, (3)
`cache_control: {"type":"ephemeral"}` (default 5-min TTL) stamped onto the **last content
block of the last message** (only when the last message's content is a non-empty array).
The API caches everything *before* a breakpoint; one per-session byte early in the prefix
defeats cross-session sharing.

`toApiMessage` block mapping (`{role, content:[...]}`):
- text → `{"type":"text","text"}`
- image → `{"type":"image","source":{"type":"base64","media_type","data"}}` (name dropped)
- reasoning: if `meta.type` is `"thinking"` or `"redacted_thinking"` → emit `meta`
  **verbatim** (signature included; the API rejects a tool_use whose preceding thinking
  was altered or dropped). Otherwise (foreign reasoning) degrade to
  `{"type":"text","text"}` if `text.trim()` non-empty, else emit nothing (empty text
  block is rejected).
- tool_use → `{"type":"tool_use","id","name","input": input ?? {}}`
- tool_result → `{"type":"tool_result","tool_use_id","content","is_error"}`

Response (SDK `messages.stream` + `finalMessage()`; in Rust, consume the Messages SSE
stream — events `message_start`, `content_block_start`, `content_block_delta`
(`text_delta`/`thinking_delta`/`input_json_delta`/`signature_delta`),
`content_block_stop`, `message_delta`, `message_stop`, `ping`, `error` — and assemble the
final message; `onText` fires per text delta). Final-message mapping (`fromApiBlock`):
- `text` → text block
- `thinking` → `{type:"reasoning", text: block.thinking, meta: block}` (raw block kept whole)
- `redacted_thinking` → `{type:"reasoning", text:"", meta: block}`
- `tool_use` → tool_use block
- anything else (server tools etc.) → dropped

Usage: `inputTokens = usage.input_tokens + (cache_read_input_tokens ?? 0) +
(cache_creation_input_tokens ?? 0)` (input_tokens is the uncached remainder — add back);
`outputTokens = output_tokens`; `cacheReadTokens`/`cacheWriteTokens` from the two cache
fields; no `reasoningTokens`. `stopReason = stop_reason ?? "end_turn"`.

SDK is constructed with `maxRetries: 0` — **the retry policy is `withRetries` only**; a
Rust port with reqwest has no hidden retries to disable, but must not add its own.

### 3b. OpenAI proper — Responses API

`POST {OPENAI_API_BASE|https://api.openai.com}/v1/responses`, headers
`authorization: Bearer <OPENAI_API_KEY>`, `content-type: application/json`. Body:

```jsonc
{
  "model": "gpt-5",                    // "openai:" routing prefix STRIPPED
  "instructions": joinedSystem,        // stable+volatile concatenated; implicit prefix caching
  "max_output_tokens": N,
  "stream": true,
  "store": false,                      // stateless: full history replayed each round
  "include": ["reasoning.encrypted_content"],
  "reasoning": {"effort": "low"|"medium"|"high"},  // only when params.effort set; "xhigh"/"max" clamp to "high"
  "input": [...],                      // toResponsesInput
  "tools": [{"type":"function","name","description","parameters": inputSchema}],
  "tool_choice": "none"                // only when set (bare string, not an object)
}
```

`toResponsesInput` per content block, flattened in order:
- user text → `{"role":"user","content":[{"type":"input_text","text"}]}`
- assistant text → `{"role":"assistant","content":[{"type":"output_text","text"}]}`
- image → `{"role":"user","content":[{"type":"input_image","image_url":"data:<mediaType>;base64,<data>"}]}`
- reasoning → emit `meta` verbatim **iff present**; meta-less reasoning is DROPPED, not
  sent bare (test-pinned). The raw reasoning item (with `encrypted_content`) must precede
  its `function_call` — the API rejects a function_call whose reasoning item is missing —
  within the live turn's round loop; across turns the replay mapper (outside llm/) drops
  them and bare function_calls are accepted.
- tool_use → `{"type":"function_call","call_id": id,"name","arguments": JSON.stringify(input ?? {})}`
- tool_result → `{"type":"function_call_output","call_id": toolUseId,"output": content}`

Response SSE (`data:` JSON events; `[DONE]` sentinel ignored/skipped):
- `{"type":"response.output_text.delta","delta"}` → `onText(delta)`
- `{"type":"response.completed","response":{...}}` → final (content assembled ONLY from
  this whole payload — deltas are display-only, no per-item assembly)
- `{"type":"response.incomplete","response":{...}}` → also final
- `{"type":"response.failed"|"error", ...}` (only when no final yet) → throw
  `LlmError("openai: <whole event JSON>", code.includes("rate_limit") ? 429 : 500)` —
  mid-stream failure is server-side ⇒ retryable
- unparseable data lines silently skipped
- stream ends with no final → `LlmError("openai: stream ended without response.completed")`
  (502 ⇒ retryable). Non-OK response → `throwHttpError`; missing body →
  `LlmError("openai: empty response body")`.

`fromResponsesOutput` over `final.output[]`:
- `{"type":"message","content":[{"type":"output_text","text"}...]}` → join output_text
  texts; push text block if non-empty
- `{"type":"function_call","call_id","name","arguments"}` → tool_use with
  `parseToolArgs("openai", arguments, toolByName, name)` (truncation semantics §2)
- `{"type":"reasoning","summary":[{"text"}...]}` → `{type:"reasoning", text: summaries
  joined "\n", meta: whole item}`

stopReason: any tool_use present → `"tool_use"`; else `status === "incomplete" &&
incomplete_details.reason === "max_output_tokens"` → `"max_tokens"`; else `"end_turn"`.
Usage: `inputTokens = usage.input_tokens ?? 0` (Responses input_tokens already includes
cached), `outputTokens`, `reasoningTokens = output_tokens_details.reasoning_tokens ?? 0`,
`cacheReadTokens = input_tokens_details.cached_tokens ?? 0`, `cacheWriteTokens: 0`.

### 3c. OpenRouter / Cloudflare / Cerebras — chat-completions (one shared family)

OpenRouter: `POST https://openrouter.ai/api/v1/chat/completions`, extra header
`x-title: bough`. Cloudflare: `POST {CLOUDFLARE_API_BASE|https://api.cloudflare.com/client/v4}/accounts/{CLOUDFLARE_ACCOUNT_ID}/ai/v1/chat/completions`;
key env `CLOUDFLARE_API_KEY` or `CLOUDFLARE_API_TOKEN`; URL is a **function of env
resolved per run()** (account/base changes apply without restart — test-pinned). Missing
account id → `LlmError(..., 401)` thrown before any fetch. Cerebras:
`POST {CEREBRAS_API_BASE|https://api.cerebras.ai}/v1/chat/completions`; key env
`CEREBRAS_API_KEY`; the `cerebras:` routing prefix is STRIPPED before the body is
sent (same as OpenAI strips `openai:`). All three send `authorization: Bearer <key>`.
Body:

```jsonc
{
  "model": params.model,               // full id incl. "@cf/" for cloudflare;
                                       // cerebras: prefix STRIPPED
  "max_tokens": N, "stream": true,
  "stream_options": {"include_usage": true},
  "messages": toOpenAIMessages(joinedSystem, messages),
  "tools": [{"type":"function","function":{"name","description","parameters"}}],
  "tool_choice": "none"                // only when set
}
```

`toOpenAIMessages` (system first as `{"role":"system","content"}` when present):
- assistant msg → one `{"role":"assistant","content": joinedText || null,
  "tool_calls": [{"id","type":"function","function":{"name","arguments": JSON.stringify(input ?? {})}}]}`
  (tool_calls key omitted when none)
- user msg → text blocks joined `"\n"` + images into ONE user message: content is a plain
  string when no images (wire shape unchanged), else parts array
  `[{"type":"text","text"}?, {"type":"image_url","image_url":{"url":"data:...;base64,..."}}...]`
  (non-vision models reject that — surfaced as-is); then each tool_result becomes its own
  `{"role":"tool","tool_call_id","content"}` message
- **repair pass**: after every assistant message with tool_calls, collect the
  `tool_call_id`s of the immediately following run of `role:"tool"` messages; for each
  call id not covered, append `{"role":"tool","tool_call_id": id,"content":"(interrupted)"}`
  right after that run. Without it the provider 400s the whole request (interrupt can
  leave a call with no result).

Response SSE chunks (`data:` JSON; `[DONE]` marks proper end):
- `chunk.error` present → throw `LlmError("provider: message", numericCode || 502)` —
  upstream failure arrives as terminal error chunk on an otherwise-200 stream
- `chunk.usage` → `{inputTokens: prompt_tokens, outputTokens: completion_tokens,
  reasoningTokens: completion_tokens_details.reasoning_tokens ?? 0,
  cacheReadTokens: prompt_tokens_details.cached_tokens ?? 0, cacheWriteTokens: 0}`
  (may arrive in a chunk with empty `choices`, after finish_reason)
- `choices[0].finish_reason` → record; sets `ended = true`
- `choices[0].delta.content` → append + `onText`
- `choices[0].delta.tool_calls[]` → accumulate by `index`: `id` set once, `function.name`
  set once, `function.arguments` string-concatenated across chunks
- `[DONE]` → `ended = true`; unparseable lines skipped
- stream closes with `!ended` → `LlmError("provider: stream truncated before completion")`

Assembly: text block first (if any), then tool_calls sorted by index; each id defaults to
`crypto.randomUUID()` when the stream never sent one; input via
`parseToolArgs(provider, arguments, toolByName, name)`. stopReason:
`tool_calls`→`"tool_use"`, `length`→`"max_tokens"`, else finish_reason verbatim (default
`"stop"` if none seen — unreachable since `!ended` throws).

### 3d. Discovery endpoints

- OpenAI: `GET {base}/v1/models`, `authorization: Bearer`, parse `{data:[{id}]}` →
  `filterOpenAIModels`. No key → `[]` without a request.
- Anthropic: `GET {base}/v1/models?limit=1000`, headers `x-api-key`,
  `anthropic-version: 2023-06-01`, parse `{data:[{id, display_name?}]}` → bare ids,
  label = display_name || id. No key → no request.
- OpenRouter: `GET {OPENROUTER_API_BASE|https://openrouter.ai/api}/v1/models` — **public**:
- Cerebras: no key → `GET {CEREBRAS_API_BASE|https://api.cerebras.ai}/public/v1/models`
  (public, no auth); with a key → `GET {base}/v1/models` with Bearer. Parse
  `{data:[{id, name?}]}` → ids prefixed `cerebras:`, label
  `"<name || id> (Cerebras)"`.
  called even with no key (key sent when present; it scopes the list); parse
  `{data:[{id, name?}]}`. Hundreds of rows (this is what makes the picker need search).
- Cloudflare: `GET {base}/accounts/{acct}/ai/models/search?task=Text+Generation&per_page=100&hide_experimental=true`,
  Bearer key; answers `{result:[{name}]}` (NOT `{data}`); keep only names starting
  `@cf/`, label `"<name minus @cf/> (Workers AI)"`. No key OR no account → `[]`, no request.

### 3e. Trace JSONL line shapes

```jsonc
{"type":"prompt","tier":"system"|"volatile","sha":"16-hex","text":"..."}
{"type":"round","n":1,"ts":ms,"latencyMs":ms,"model":"...","effort":"...?",
 "systemSha":"...","volatileSha":"...",
 "request":{"maxTokens":N,"toolChoice":"none"?,"tools":["name",...],"messages":[...]},
 "response":{"content":[...],"stopReason":"...","usage":{...}}   // XOR
 "error":{"name":"TypeError","message":"boom"}}
```

---

- `openai:ft/custom-model` → openai (prefix wins over slash).
- `cerebras:org/custom` → cerebras (prefix wins over slash).
- `MODELS` entries must each route to their declared provider; `catalogKeys(id)[0]` must
  start with the provider's catalog prefix (`anthropic/`, `openai/`, `openrouter/`,
  `cloudflare-workers-ai/`, `cerebras/`) — drift test.
  every Workers AI model goes to OpenRouter and 400s.
- `openai:ft/custom-model` → openai (prefix wins over slash).
- `MODELS` entries must each route to their declared provider; `catalogKeys(id)[0]` must
  start with the provider's catalog prefix (`anthropic/`, `openai/`, `openrouter/`,
  `cloudflare-workers-ai/`) — drift test.
- Client construction must not read a key or touch the network; a missing key surfaces
  only at `run()`, as `LlmError` **401** naming the env var(s) — 401 so `isRetryable`
  says no ("a missing key will still be missing in 15 seconds").

**Retry classification (`isRetryable`)**
- Non-retryable: `errName` ∈ {`AbortError`, `APIUserAbortError`}; `LlmError` with
  non-retryable status; anything with numeric `.status` not in {408, 429, ≥500}.
- Retryable: `LlmError` status 408/429/≥500 (default 502 counts); `isToolProtocol400`;
  numeric `.status` retryable; `errName` ∈ {`APIConnectionError`,
  `APIConnectionTimeoutError`}; `isNetworkFault` (Bun throws a *plain Error* with only
  `code:"ECONNRESET"` on mid-stream connection death — codes: ECONNRESET, ECONNREFUSED,
  ECONNABORTED, EPIPE, ETIMEDOUT, ENOTFOUND, EAI_AGAIN, EHOSTUNREACH, ENETUNREACH,
  ENETDOWN, UND_ERR_SOCKET, ERR_STREAM_PREMATURE_CLOSE; recurses `.cause` ≤4); plain
  `TypeError` (fetch network failure). `ENOENT` is NOT retryable. In Rust: classify
  `reqwest::Error` connect/timeout/body errors + `std::io::ErrorKind` equivalents.
- `errName`: the Anthropic SDK never sets `.name`, so it reads `"Error"` — must fall
  through to constructor name. In Rust this whole dance collapses into typed error enums,
  but the *classification table* above is the contract.

**Retry loop**
- attempt counter 1-based; give up when `attempt >= maxAttempts` OR `signal.aborted` OR
  not retryable — rethrowing the LAST error. Fake with 3 scripted failures &
  maxAttempts=3 sees exactly 3 calls.
- Backoff: `base * 2^(attempt-1) * uniform(0.5, 1.0)`; delay = `round(max(retryAfterHint ?? 0, backoff))`.
- `retryAfterHint`: `LlmError.retryAfterMs`, else SDK-style `err.headers.get("retry-after")`
  seconds → ms (finite, >0).
- `onRetry` fires before the sleep. Sleep is abort-aware: abort during backoff rejects
  with AbortError("interrupted during retry backoff") immediately.
- An abort raised *during* a failing run (signal aborted by the time catch runs) rethrows
  the original error without sleeping (test: 503 with 50 s base delay returns promptly).

**Pricing**
- `withPricing` skips when `usage` absent or `costUsd` already non-null.
- Unpriced model → `costUsd: null` on the usage object — never 0, never omitted.
- fresh-input clamp `max(0, …)`: over-reported cache reads must not produce a negative bill.
- Nullish cache token counts behave as 0, not NaN.
- Cache read rate falls back to input rate when the catalog slot is null (so does write).

**System tiers**
- Empty-string tiers filter out: `anthropicSystemBlocks({system:""})` → undefined;
  volatile-only → 1 block. `joinedSystem` has NO separator (`"A"+"B" = "AB"`, test-pinned).

**Streaming / truncation**
- A frame split across TCP chunks must reassemble (decoder is streaming + line buffer).
- `[DONE]` passes through `sseEvents` — callers interpret it.
- Stall guard cancels the reader (ignore cancel errors) and throws retryable.
- Trailing un-newlined fragment dropped silently.
- Chat-completions: a stream that closes without `[DONE]`/finish_reason ⇒ "stream
  truncated before completion" (retryable) — do NOT return the partial round.
- Responses: no `response.completed`/`response.incomplete` ⇒ "stream ended without
  response.completed" (retryable).
- Tool-arg truncation: see `parseToolArgs` — the tool's declared `required` decides;
  unknown tool + no args → `{}` (dispatcher reports the unknown name properly).

**Anthropic specifics**
- Thinking blocks replay verbatim (signature included) via `meta`; foreign (unstamped)
  reasoning degrades to a text block, and empty-text foreign reasoning emits nothing.
- effort params only for `/claude-(fable|mythos|sonnet|opus)-5|opus-4-[89]/`; when model
  is undefined they ARE sent (the guard is for known-incompatible models).
- Final-message cache breakpoint only stamped when last message content is a non-empty array.

**Trace**
- All file I/O failures swallowed (full disk must never kill a turn).
- `n` counts attempts including failures (retry wrapper sits outside).
- Prefix text emitted once per distinct sha per wrapped-client lifetime.

**Cheap-tier contract (context)**: `completeText` consumers (titles, blurbs) are outside
this module but the interface must stay drivable by a scripted fake — the entire upstream
test suite depends on `LlmClient` being trivially mockable.

---

## 5. Dependencies

Imports (from llm/): `../errors.ts` (`LlmError`/`HttpError`), `../types.ts` (boundary
types), `../schema/parts.ts` (`Part` — for `blocksToParts`), `../prompt/assemble.ts`
(`sectionSha` — trace only), `@anthropic-ai/sdk` (Anthropic route only; drop in Rust).

Imported by (outside llm/): `turn/runner.ts` and `turn/queue.ts` (the agent loop —
`clientFor`, `blocksToParts`, `contextWindowFor`), `server/models.ts` (+test)
(MODELS/discovery/merge), `server/sessions.ts`, `history/compact.ts`,
`history/explore.ts`, `history/handoff.ts`, `history/sections.ts` (mostly
`completeText`/`clientFor`), `worker/titles.ts` (cheap tier).

---

## 6. External deps → Rust equivalents

| TS / Bun | Rust |
|---|---|
| `@anthropic-ai/sdk` (messages.stream) | none — hand-rolled reqwest + SSE per §3a (this spec exists so the SDK isn't needed) |
| global `fetch` (injected) | `reqwest::Client` behind a small `Transport` trait (so tests inject canned SSE) |
| SSE hand-parser over `ReadableStream` | keep hand-rolled over `bytes_stream()` (it's 40 lines and the `[DONE]`/stall semantics are custom); `eventsource-stream` crate optional but it hides the stall/trailing-fragment behavior |
| `AbortSignal` / abort-aware sleep | `tokio_util::sync::CancellationToken` + `tokio::select!` with `tokio::time::sleep` |
| `setTimeout` stall guard | `tokio::time::timeout` around each stream chunk read |
| `JSON.parse` / stringify | `serde_json` (`serde_json::Value` for opaque `meta` and `input`) |
| `process.env` (injected `Env`) | `Fn(&str) -> Option<String>` trait object / closure, default `std::env::var` |
| `crypto.randomUUID()` | `uuid` crate v4 |
| `node:crypto` sha256 (sectionSha) | `sha2` crate, hex via `hex` or manual, truncate 16 chars |
| `node:fs` appendFileSync etc. (trace) | `std::fs` (`OpenOptions::append`, `create_dir_all`), errors ignored |
| `pricing.json` import | `include_str!` + `serde_json` parsed once via `LazyLock` (or build-script into phf); row type `(f64, f64, Option<f64>, Option<f64>, Option<u64>)` |
| `Promise.allSettled` | `futures::join!` on functions that already return `Vec` (they never error) |
| zod `Usage` | plain serde struct with `Option` fields, `camelCase` rename |

---

## 7. Suggested Rust layout (crate `bough-llm` or module `llm/` in the main crate)

```
llm/
  mod.rs        — pub use; clientFor equivalent: fn client_for(model, opts) -> Box<dyn LlmClient>
  types.rs      — LlmBlock/LlmContentBlock/LlmMessage/LlmParams/LlmResult/Usage/Effort/LlmToolDef
                  (or live in a shared bough-types crate since turn/server use them)
  error.rs      — LlmError { message, status: u16, retry_after: Option<Duration> } + classification
  routing.rs    — Provider enum, provider_for(), API key env table, MODELS static table
  retry.rs      — with_retries (decorator struct), is_retryable, backoff
  openai_compat.rs — chat-completions family: to_openai_messages (+repair pass),
                  streamed tool-call accumulator; OpenRouter + Cloudflare + Cerebras
                  are configs of it
  anthropic.rs  — request builder (system blocks, cache_control, toApiMessage, effortParams),
                  SSE assembly, usage normalization
  openai.rs     — Responses API: to_responses_input / from_responses_output / client
  openai_compat.rs — chat-completions family: to_openai_messages (+repair pass),
                  streamed tool-call accumulator; OpenRouter + Cloudflare are two configs of it
  pricing.rs    — catalog load, catalog_keys/rates_for/context_window_for/usage_cost_usd
  parts.rs      — blocks_to_parts (or in the shared parts module)
  trace.rs      — TraceLabel, RoundRecord, with_trace decorator (sync fs writes are fine)
  discovery.rs  — the four discover_* fns + filter_openai_models (by_newest natural sort) + merge
```

Trait:

```rust
#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn run(
        &self,
        params: &LlmParams,
        on_text: &mut (dyn FnMut(&str) + Send),   // or tokio::sync::mpsc::Sender<String>
        cancel: &CancellationToken,
    ) -> Result<LlmResult, LlmError>;
}
```

Decorators (`Retry<C>`, `Trace<C>`, `Pricing<C>`) as structs wrapping `Box<dyn LlmClient>`
— composition order fixed in `client_for`: `Retry(Trace(Pricing(provider)))`. Prefer an
mpsc/channel for `on_text` if the TUI consumes deltas across task boundaries; the TS
contract is just "called in order with each delta, possibly repeated after a retry".
Error type: one `LlmError { status, message, retry_after }` struct is truer to the TS than
a rich enum — classification is by status + message + a `NetworkFault` marker; map
reqwest connect/timeout/decode errors into it at the transport edge with status 502.
Async boundary: everything in `run()` is async (reqwest + streams); trace/pricing are sync
work inside it. `by_newest` natural sort: split on digit runs, compare digit-vs-digit
numerically, else lexicographically, descending.

## 8. v1 scope cut

**Core (needed for a working loop):**
- `provider_for` routing, `LlmClient` trait, `client_for` composition
- Anthropic client raw-HTTP: full message encoding, thinking replay via `meta`, the three
  cache breakpoints, usage normalization (this is the daily-driver route)
- `with_retries` + full `is_retryable` classification + stall guard + truncation checks —
  do NOT cut these; they are the difference between "flaky" and "usable", and every one
  encodes a shipped production bug
- `parse_tool_args`, `blocks_to_parts`
- pricing lookup + `usage_cost_usd` + `context_window_for` (status bar + overflow errors
  depend on it; it's ~80 lines over an included JSON)
- `complete_text` (titles/summaries call it)

**High (daily driver, port soon after):**
- OpenRouter/chat-completions family (`openai_compat.rs`) incl. the tool_calls repair pass
  and fragment accumulator
- `MODELS` static table + `merge_models`

**Later:**
- OpenAI Responses client (distinct wire shape, one user-facing model family)
- Cloudflare Workers AI (a config of the compat family + account-scoped URL)
- Discovery (all four) + `filter_openai_models`/`by_newest` — picker works from the static
  table meanwhile
- `effortParams` model-gating regex can start as Claude-5-family only

**Stub in v1:**
- `trace.rs` — `with_trace(inner, None)` identity; the tuning harness is the only consumer
  (`BOUGH_TRACE_DIR` unset in normal use). Keep the `RoundRecord` shape in the spec so the
  stub can grow without breaking readers.
