# Port spec: the five small subsystems

Covers `src/logs/`, `src/worker/`, `src/vcs/`, `src/skills/`, `src/prompt/`.
Written against the tree as of 2026-08-05 (main, clean). Line references are to the TS source, which the porter should read side by side with this.

Cross-cutting facts the porter needs before any section:

- **Shared context types** (defined elsewhere, consumed here): `AppCtx`, `Bus`, `Db`, `LlmClient` from `src/types.ts`; `Message`, `Session`, `Part`, `ToolCallPart`, `SessionKind` from `src/schema/parts.ts`; `HostFnName` from `src/harness/protocol.ts`. The Rust equivalents live in whatever the shared-types crate becomes; this spec names the narrow slices each module actually takes.
- **DI discipline**: every module here takes its effects as parameters (an `Env` closure, a `sources` list, a `Db` slice, an injected `LlmClient`) and defaults to the real thing. Tests never fake globals. Preserve this shape in Rust — trait parameters with `Default`-ish constructors, not statics.
- **Who imports whom (verified by grep)**:
  - `cli/patterns.ts` → `logs/analyze.ts`, `logs/format.ts` (the ONLY external consumer of logs)
  - `server/main.ts` → `worker/{activity,ghost,titles}`, `skills/skills.ts`, `prompt/assemble.ts` (wires the three bus watchers at boot)
  - `server/app.ts` → `worker/ghost.ts` (route table)
  - `server/sessions.ts` → `worker/titles.ts`, `vcs/repodiff.ts`, `prompt/project.ts`
  - `server/changes.ts` → `vcs/repodiff.ts`
  - `server/skills.ts`, `tui/api.ts` → `skills/skills.ts`
  - `turn/runner.ts` → `prompt/assemble.ts`, `prompt/project.ts`
  - `llm/trace.ts`, `mcp/status.ts` → `prompt/assemble.ts` (types only)
  - `skills/skills.ts` → `prompt/assemble.ts` (the `PromptSkill` type) — the only edge between the five.

---

## 1. logs — `bough patterns` log compression

### Purpose & invariants

A streaming pipeline that compresses an arbitrarily large log into a fixed-size `Analysis`: distinct statement templates, per-variable-slot statistics, anomalies, correlations. Clean-room implementations (user rejected vendoring ctrlb-decompose; algorithms are from published papers: CLP masking, Drain clustering, DDSketch, HyperLogLog, Vitter's Algorithm R).

Quoted invariants (verbatim from module headers):

- `analyze.ts`: "ONE PASS OVER THE INPUT, and nothing that scales with line count is retained. A line is folded into its cluster's accumulators and dropped; what survives is a fixed cost per pattern."
- `sketch.ts`: "NOTHING HERE IS PROBABILISTIC ABOUT *WHICH* ANSWER IT GIVES. Each structure is deterministic given the same input sequence" — `Reservoir` takes a seed, never `Math.random`; two runs over one file must produce byte-identical output.
- `sketch.ts`: "PURE AND DEPENDENCY-FREE. No clock, no filesystem, no npm."
- `drain.ts`: "ONE PASS, NO SECOND LOOK. … templates depend on ARRIVAL ORDER." Counts/statistics are order-independent; only wildcard placement in the rendered template may differ.
- `drain.ts`: "BOUNDED, AND HONEST WHEN THE BOUND BINDS" — LRU eviction sets `truncated`, surfaced all the way to the output header.
- `types.ts`: "The JSON formatter serializes `Analysis` almost verbatim, which makes this file the de-facto public contract of `--json`: renaming a field here changes an output format somebody may be parsing."
- `format.ts`: "NUMBERS ARE NEVER SILENTLY ROUNDED INTO A LIE" — approximate values are marked `~`, untrustworthy rankings omitted (`top: null`), truncation stated in the header.
- `timestamp.ts`: anchored at line start only; "A LINE WITHOUT ONE IS NORMAL, NOT AN ERROR"; pure — `refYear` is a parameter (default 1970, deliberately obviously-wrong rather than subtly-wrong).

### Public API

`sketch.ts` (pure):
- `murmur3(key: string, seed=0): number` — MurmurHash3 x86 32-bit over **UTF-16 code units masked to their low byte** (`charCodeAt(i) & 0xff`), NOT UTF-8 bytes. Test pins published vectors: `murmur3("") === 0`, `murmur3("a") === 0x3c2569b2`, `murmur3("abcd") === 0x43ed676a`, `murmur3("hello") === 0x248bfa47`, `murmur3("hello, world") === 0x149bbb7f`. In Rust: implement over `s.encode_utf16().map(|u| (u & 0xff) as u8)` — an off-the-shelf murmur3 crate over UTF-8 bytes will NOT reproduce these values for non-ASCII, and the tests assert exact hashes.
- `class DDSketch` — `new(alpha=0.01)` (RangeError outside (0,1)); `add(v: f64)` (non-finite ignored; 0 counted separately; negatives in a mirrored store keyed on index of |v|); `count`; `quantile(q)` (rank `ceil(q*n)` clamped to [1,n]; walk negatives descending, zeros, positives ascending; fallback `maxSeen`); `summary(): Quantiles | null` (null when empty; min/max/mean exact; p50/p90/p99 **clamped into [minSeen, maxSeen]**); `buckets(): {value,count}[]` ascending by value (copy, not live). Bucket index `i = ceil(ln(v)/ln(gamma))`, gamma `=(1+alpha)/(1-alpha)`, representative value `2*gamma^i/(gamma+1)`.
- `interface Quantiles { count, min, max, mean, p50, p90, p99 }`.
- `class HyperLogLog` — `new(p=12)` (RangeError unless integer in [4,16]); `add(value: string)` (murmur3, top p bits pick register, rank = clz over the remaining bits `+1`, discounting shifted bits: `rest === 0 ? 32-p+1 : clz32(rest)-p+1`); `count(): number` with both Heule-style corrections: linear counting when `estimate <= 2.5*m && empty > 0`, large-range correction when `estimate > 2^32/30`. Alpha constants: 0.673/0.697/0.709 for m=16/32/64, else `0.7213/(1+1.079/m)`.
- `class Reservoir<T>` — `new(k, seed=0x9e3779b9)`; zero seed replaced by 0x9e3779b9 (xorshift fixed point); RNG is **xorshift32** (`x^=x<<13; x^=x>>>17; x^=x<<5`, u32 wrap); replacement index `next() % seen` (bias accepted, documented); `add`, `sample(): T[]` (copy, insertion order), `total`.
- `class TopK` — `new(capacity=1024)`; exact counts up to capacity, then `overflow++` for NEW keys only (existing keys keep counting); `saturated: bool` (overflow>0); `tracked`; `top(n)` sorted by count desc **then value ascending lexicographic** (stable across runs — pin this tiebreak).

`mask.ts`:
- `interface Masked { logtype: string; values: VarValue[] }`
- `mask(line: string): Masked` — one left-to-right scan of ONE combined alternation, first alternative wins. `at` = char offset of the placeholder's `<` in the logtype (recorded before appending).
- `kindOrder(): {kind, why}[]` — for `bough patterns --explain`.
- Kind alternation order (LOAD-BEARING, port exactly): `quoted`, `uuid`, `url`, `timestamp`, `ipv6`, `ipv4`, `bytes`, `duration`, `hex`, `path`, `float`, `int`. Word fences `(?<![A-Za-z0-9_])` / `(?![A-Za-z0-9_])` on every digit-bearing kind; a preceding `.`/`:`/`=` must NOT block a match (`status=200`, `:5432`, `1.5` must match). Exact regexes are in `mask.ts:74-135` — copy them character for character, including: ipv4's octet range check plus `(?<![\d.])`/`(?![\d.])` digit fences (`1.2.3.4000` is not an address); ipv6 conservative (full 8 groups or a `::` elision — `14:22:01` must NOT match); duration units `ns|µs|us|ms|s|m|h|d` with NO space before the unit; bytes allows one optional space; hex = `0x…` or bare `[0-9a-fA-F]{8,}` (length alone, no digit-required rule — `bebbccce` IS hex, documented regression); path needs ≥2 segments.
- `magnitude` normalization: durations → milliseconds (`ns:1e-6 … d:86400000`), bytes → bytes with **1024-based scale for both kb and kib spellings**; int/float → Number. Other kinds carry no `num`.

`timestamp.ts`:
- `interface StampedLine { when?: number; rest: string; matched?: string }`
- `stripTimestamp(line, refYear=1970): StampedLine`. Consumes leading whitespace + one optional `[` or `(` first; if a format matches, also consumes the matching close bracket and then strips leading `[\s:—-]+` from the rest. Formats in order: ISO 8601 (space or T; fraction **padded** to ms, `.1`=100ms; offset applied by subtraction; no offset = UTC), Apache (`15/Jan/2024:14:22:01 +0000`), syslog (`Jan 15 14:22:01`, needs refYear), plain (`2024-01-15 14:22` with optional seconds, `-` or `/` separators, read as UTC), epoch (exactly 10 digits = seconds w/ optional fraction, exactly 13 = ms; trailing `(?![\d.])`/`(?!\d)` so a 14+-digit id never parses). Non-finite parse result → `when: undefined` but still stripped.

`drain.ts`:
- `WILDCARD = "<*>"`; `interface Cluster { id, tokens: string[], count }`; `interface DrainOptions { threshold?=0.4, depth?=4, maxChildren?=100, maxClusters?=10000, onEvict? }`.
- `class Drain` — `add(tokens: string[]): Cluster` (returns the LIVE cluster; its `tokens` mutate in place as it generalizes — statistics must key on `id` + position, never token text); `clusters(): Cluster[]` most-frequent-first; `truncated: bool`.
- Tree: root keyed by token COUNT, then `indexTokens = max(1, depth-2)` levels keyed by `indexKey(token)`. `indexKey` = `WILDCARD` if the token contains a digit **or contains `<`** (a masked placeholder must disqualify like a digit — the pipeline's addition to the paper; test "a masked token does not index the tree either"). Fan-out capped at `maxChildren` per node; overflow funnels to one wildcard child.
- Similarity: fraction of positions with the identical token; a wildcard position credits NEITHER side; ties break toward fewer wildcards. Match requires `sim >= threshold`. On match, disagreeing non-wildcard positions become wildcards permanently.
- LRU: insertion-ordered map, `touch` = delete+reinsert. Eviction on overflow removes the victim from its leaf too (hint leaf first, else full tree walk) and calls `onEvict` — Analyzer uses that to drop the pattern's accumulators.

`stats.ts`:
- `severityOf(line): Severity` — splits on `[^A-Za-z_]+`, matches whole tokens only, requires the word to have been SHOUTED or Capitalized in the original (`w === w.toUpperCase() || w[0] === w[0].toUpperCase()`); word lists in priority order fatal→error→warn→debug→info (note debug BEFORE info in the scan order); default `"info"`. `failed to warn the operator` is info; `FAIL`/`FAILED`/`FAILURE`/`EXCEPTION` are error-tier words.
- `class TimeAxis` — shared bucket axis. `bucketMs` starts 1000, doubles until span < `MAX_BUCKETS = 512`; `generation` counts doublings; lo/hi rescale with **arithmetic shift `>>1`** (floors toward −∞, matching `Math.floor` — indices may be negative for out-of-order lines; in Rust use `div_euclid`-consistent shifting on i64). `index(when)` may coarsen in a loop (one far-out line can double several times).
- `class PatternAcc` — per-cluster accumulator: worst-seen severity, first/last epoch ms, `Reservoir<String>(3, seed 0x5bf03635)` examples, sparse bucket map with lazy `rescale(generation)` (pairwise fold `idx>>1`), slots in a map keyed `"{tokenIndex}.{ordinal}"` (one token can carry several values; ordering sort is numeric on both components). `bucketArray(axis)` = dense array over axis range (empty if no buckets). `summarize(): VarSummary[]`.
- Slot description (`describeSlot`): unique = `min(hll.count(), slot.count)` (clamped — HLL overshoot must never print `unique > count`); starting kind = most frequent masker kind; **id test before enum test**: id iff `count>=10 && unique/count>0.9` and kind ∈ {int,hex,string,uuid}; enum iff `count>=10 && unique<=20 && topShare>=0.8` and kind ∈ {int,string,float}. `top` = null when `saturated || kind=="id"`, else top-3 with shares. `numeric` present only when `kind != id && kind != enum && unique > 1` and a sketch exists; carries `unit` (`ms`/`bytes`) when set.
- `tokenize(logtype): Tok[]` (`/\S+/g` with offsets); `attribute(tokens, values, template): VarValue[][]` — values assigned to the last token starting at/before `v.at`; for template positions that are `WILDCARD`, the token's text is reconstructed with placeholder raws substituted back (`cursor` advances by `"<kind>".len()`), sole kind kept iff exactly one value whose raw equals the whole reconstruction, `num` kept iff exactly one value.

`anomaly.ts`:
- `detect(p: Pattern, totalLines): Anomaly[]` — capped at `MAX_PER_PATTERN = 4`, emitted in priority order. Rules (all plain thresholds, no statistics): frequency-spike (≥5 active buckets, count≥20, peak ≥ 5× median of active buckets, median>0); error-burst/episodic (≥10 active buckets and top-3 buckets hold ≥90% of count); rare (count ≤ max(5, total×0.001) AND severity error/fatal); per-var (count≥20 each): single-value (unique==1, with top), high-cardinality (kind=="id"), bimodal (p50>0 && p99≥10×p50 && p90≤3×p50), long-tail (p50>0 && max≥100×p50, only if not bimodal).
- `fmt(value, unit?): string` — ms: ≥60000→`X.Xmin`, ≥1000→`X.XXs`, ≥1→`Nms`, else µs; bytes: /1024 ladder B..TB; else bare number. `round`: ≥100 integer, ≥10 one decimal, else two decimals.

`correlation.ts`:
- `correlate(patterns): Correlation[]` — quadratic over the RENDERED (post-`top`) patterns only. Temporal: both counts ≥10, ≥3 active buckets each, cosine over `min(len)` prefix, threshold 0.8, zero-norm guard. Shared-value: slots with non-null `top`, `unique<=50`, same kind, kind ∉ {int,float}; strength = min of the two shares of a common top value, threshold 0.5. Sorted by strength desc, capped at 8. Detail strings are exact formats (`#A and #B rise and fall together (NN% aligned over time)`, `#A slot X and #B slot Y both centre on V (NN% of each)`).

`analyze.ts`:
- `interface AnalyzeOptions { top?=20, refYear?, drain? }`
- `class Analyzer` — `push(raw)` (blank lines skipped AND not counted in `lines`), `finish(): Analysis`. Pipeline per line: strip timestamp → track span → mask → tokenize → drain.add → get-or-create PatternAcc → `acc.add(raw, when, attribute(...), axis)`. `finish`: build Pattern per live cluster (template = tokens joined with single space), `detect`, sort by `score` desc then count desc, truncate to `top`, **renumber ids to 1..N in render order** (correlations run over renumbered patterns so `#1` in detail text is findable), emit `timeSpan`+`bucketMs` only when both span ends exist, `truncated` from drain.
- `score(p) = SEV_RANK*100 + log10(count+1)*5 + (anomalies?10:0)` — severity dominates by construction.
- `analyze(lines, opts)` — convenience wrapper over an iterable.

`format.ts`:
- `toLlm(a): string` — markdown; header `# N lines → M patterns · span … · showing top K`; truncation NOTE blockquote; `## Problems (n)` (error+fatal) then `## Everything else (n)`; per pattern `### #id [SEV] N lines (P%)`, fenced template, slot lines (constants — `unique==1 && !numeric` — dropped), `⚠` anomaly lines, one example only for severe patterns; `## Related` bullet list. Ends with exactly one trailing newline.
- `toJson(a): string` — `JSON.stringify(a, null, 2) + "\n"`, `Analysis` verbatim. In Rust: serde with **exact field names** (`patternCount`, `firstSeen`, `lastSeen`, `timeSpan`, `bucketMs`, `truncated`, camelCase throughout) and optional fields omitted (not null) when absent — TS spreads them conditionally.
- `toHuman(a, colour: bool, width=80): string` — ANSI codes as in source; bar scaled to the LARGEST shown pattern (not total); bar width `clamp(width-56, 10, 24)`; number formatting `toLocaleString("en-US")` (thousands commas — implement manually in Rust); `pct` precision ladder (≥10 int, ≥1 one decimal, else two); timestamps as `YYYY-MM-DD HH:MM:SSZ`.
- Shared `slotLine(v)`: `slot N  kind  …` — id → `~{unique} distinct / {count}`; unique==1 → `always {value}`; else `{unique} unique` + top list `v (P%)`; numeric appends `p50=… p90=… p99=… max=…` via `fmt`.

### Data structures

All in `types.ts`, serialized verbatim by `--json`: `VarKind` (15 string variants incl. `enum`, `id` decided in stats), `VarValue {kind, raw, num?, at}`, `VarSummary {slot, kind, count, unique, top: {value,count,share}[] | null, numeric?: {min,max,mean,p50,p90,p99,unit?}}`, `SEVERITIES` const order `["debug","info","warn","error","fatal"]`, `Anomaly {kind: 7 variants, detail}`, `Pattern {id, template, count, share, severity, firstSeen?, lastSeen?, vars, examples, buckets, anomalies}`, `Correlation {a, b, kind: "temporal"|"shared-value", strength, detail}`, `Analysis {lines, patternCount, patterns, correlations, timeSpan?, bucketMs?, truncated}`. No DB tables. No wire protocol beyond `--json` stdout.

### Behaviors & edge cases (test-mined)

- Determinism end to end: same file twice ⇒ byte-identical output (seeded reservoir, stable TopK tiebreak, stable sorts).
- DDSketch: relative error holds across 4 orders of magnitude; min/max/mean exact; zeros and negatives placed correctly in quantile walk; empty sketch → `summary() == null`; bounded under one repeated value; **quantiles never escape observed range** (clamp test).
- HLL: near-exact at low cardinality (the enum threshold depends on it), within a few % at 1M, duplicates free, `p` out of range throws.
- Reservoir: keeps all until full; samples the whole stream (a "first-k" implementation fails the test); deterministic per seed; survives seed 0.
- Drain: different-length lines never merge; unrelated equal-length lines stay apart; template only loses specificity; eviction removes from candidate list (an evicted template must never keep matching); empty token list must not crash (a blank-ish line reaching add); fan-out cap loses no lines.
- Mask: statelessness across calls (the `g`-flag `lastIndex` reset in TS; a Rust port with per-call scanning is naturally stateless); empty line ok; `10.0.1.15:5432` → `<ipv4>:<int>` (two values one token); `a107b3f` NOT carved into `<bytes>`; `1.5` float not two ints; `5 m` not a duration.
- Timestamp: `1705329721` (10 digits) seconds vs 13-digit ms by width; a 14+-digit id left alone; offset `+05:30` subtracted; fraction `.1` = 100 ms.
- Attribution correctness under late generalization is accepted lossily by design: earlier lines were attributed under the more specific template; positions don't move, so slots stay aligned.

### Dependencies

Imports: nothing outside `src/logs/`. Imported by: `cli/patterns.ts` only (flags: `--llm/--json/--human`, `--top N`, `--threshold`, `--ref-year`, `--explain`, `--color/--no-color`; default format `--human` iff stdout is a TTY; exit 0 analyzed / 1 unreadable input / 2 usage; deliberately NO "found errors" exit code). Also referenced by the bundled `analyze-logs` skill (teaches the CLI).

### External deps → Rust

None at all in the pipeline (pure). CLI: stdin/file line reading (`std::io::BufRead`), TTY detection (`std::io::IsTerminal`). JSON: `serde`/`serde_json`. Do NOT pull a murmur3 crate unless verified against the UTF-16-low-byte vectors above; 30 lines by hand is safer. No regex crate caveat: TS uses lookbehind/lookahead — the `regex` crate does not support lookaround, so either use `fancy-regex` for the mask alternation and timestamp formats, or hand-roll the boundary checks (recommended: use `regex` for the bodies and check the fence characters manually at match boundaries — faster and dependency-lighter; the fences are all single-char classes).

### Suggested Rust layout

`crates/bough-logs/`: `types.rs` (serde structs), `sketch.rs` (DDSketch/Hll/Reservoir/TopK + murmur3), `mask.rs`, `timestamp.rs`, `drain.rs`, `stats.rs`, `anomaly.rs`, `correlation.rs`, `analyze.rs`, `format.rs`. Fully synchronous — no tokio anywhere; the CLI feeds lines from a BufReader. No traits needed; the structures are concrete. Port the test suites 1:1 (they are the contract; ~40 pinned behaviors listed above).

### v1 scope cut

The whole subsystem is severable: `bough patterns` is a standalone subcommand with no server or TUI coupling. **Priority: later** — stub the CLI subcommand with "not yet ported" (exit 2) and nothing else breaks. When ported, port whole; there is no useful partial (format depends on anomaly's `fmt`, analyze on everything).

---

## 2. worker — the cheap tier (titles, ghost text, activity blurbs)

### Purpose & invariants

Three cosmetic features powered by one small hosted model ("cheap tier"), all wired as bus listeners / routes in `server/main.ts` and `server/app.ts`. (The memory's "fast-apply/digestion/annotations/recall" consumers belong to the pre-rewrite tree; the current `src/worker/` is exactly these three.)

Quoted invariants:

- `titles.ts` (governs all three): "**a cheap-model call can only ever ADD something. It can never take anything away, delay anything, or fail anything.**" Enforced structurally: `cheapText` "**Never rejects, never hangs, never logs.**" — every failure (no key, unroutable model, provider error, refusal, empty answer, deadline) is the same `null`.
- `activity.ts`: "**one in-flight blurb per session — rounds that land while it is busy are DROPPED, not queued**" and "**nothing here persists.**" (no table, no column, no cache; a reconnecting client has no blurb until the next round).
- `ghost.ts`: "**the ghost is never on a turn's path.**" Route answers `200 {ghost: null}` for every cheap-model failure; 404 only for an unknown session.
- Module layering: `titles.ts` is the base; `ghost.ts` and `activity.ts` import `cheapText`/`CheapCallOpts` from it, never each other.

### Public API

`titles.ts`:
- `CHEAP_MODEL_ENV = "BOUGH_CHEAP_MODEL"`, `DEFAULT_CHEAP_MODEL = "claude-haiku-4-5"`, `CHEAP_TIMEOUT_MS = 12_000`.
- `type Env = (key) => string | undefined`; `cheapModel(env=processEnv): string` — read **per call** (picker change needs no restart), trimmed, falls back to default.
- `CheapCallOpts { system, prompt, maxTokens, llm?, model?, timeoutMs?, env? }`.
- `cheapText(opts): Promise<string|null>` — AbortController + timer; `clientFor(model)` resolved INSIDE the try (missing key = null, not throw); concatenates text blocks; empty → null; timer always cleared (a live timer keeps the process awake).
- `TITLE_SYSTEM` (exact prompt string — includes the measured "never invent a subject" grounding clause), `TITLE_MAX_INPUT = 2000`, `TITLE_MAX_CHARS = 60`.
- `sanitizeTitle(raw): string` — first non-empty line; strip `title:` label; strip markdown leaders `#{1,6}|[-*•]` (must run BEFORE quote stripping); strip matched leading/trailing quote/backtick/asterisk chars (trailing set also eats `.`); **refuse** (return `""`) when: fewer than 3 letters remain (`1`, `42`, `ok` fail; `Bug`, `CI fix` pass), or the line starts with a reply-word (`i|i'm|i'll|i've|sorry|sure|certainly|okay|ok|here|let|as|based|the user|you`, case-insensitive, word boundary); cap at 8 words, then pop trailing connectives (`and|or|but|so|because|that|which|with|for|to|of|in|on|a|an|the`, comparing with trailing `,;:` stripped, never popping below 1 word); apply `sentenceCase`; slice to 60 chars; strip trailing `,;:`.
- `sentenceCase` (private but behavior-pinned): fires ONLY when every word is either "prose" (`^[A-Z][a-z]+$`) or "opaque" (doesn't start lowercase), AND at least one prose word exists; lowers prose words except the first; a single lowercase-initial word (`getUser`, `mod.py`) disables the rewrite entirely; `C`, `CI`, `API`, `b()` survive untouched.
- `cheapTitle(firstMessage, opts?): Promise<string|null>` — slice input to 2000, empty → null, sanitize, `""` → null. Also used by `history/compact.ts` (takes free text, not a session id).
- `TitleCtx { db, bus, cheap? }`; `AutoTitleOpts { placeholder?="", inflight?: Set<string> }`.
- `maybeAutoTitle(ctx, sessionId, text, opts): void` — returns void, never throws, nothing awaits it. Guards: no cheap tier → noop; empty text → noop; session must exist AND (title === placeholder OR title starts with `"! "` — a shell-command title is PROVISIONAL and replaceable); inflight dedup per session; **after** the answer, re-check the same condition (user may have renamed mid-flight) before `db.setSessionTitle` + publish `session.updated` with the fresh session row.
- `userText(message): string` — join text parts, trim; empty for images-only.
- `watchTitles(ctx): () => void` — subscribes to `message.started` with role `user`; listener body synchronous (bus fans out synchronously — anything slow is latency on the publisher).

`ghost.ts`:
- `GHOST_SYSTEM` (exact prompt), `MAX_LINES = 8`, `MAX_LINE_CHARS = 600`, `MAX_SUGGESTION = 150`.
- `ConvoLine { role: "user"|"agent", text }`; `convoFrom(messages): ConvoLine[]` — text parts joined; role `supervisor` → `agent`, everything else (INCLUDING `system` notes) → `user` (a detached subagent's report is often what the next message is about); empty-text messages skipped.
- `renderConvo(lines)` — last 8 lines, oldest first, each formatted `role: text`; long lines keep their **TAIL** (`"…" + slice(-600)`) — the ending is the signal.
- `ghostPrompt(lines, prefix="")` — conversation block; with a typed prefix, appends "The user has started typing: … Complete it as the whole next message, starting from what they typed:"; without, "The user's next message:".
- `sanitizeSuggestion(raw)` — first non-empty line; strip `user:|next:|suggestion:` label; strip quotes; cap 150; null if empty.
- `cheapGhost(prompt, opts?)` — maxTokens 64; null for blank prompt without calling.
- `ghostFor(ctx: {db, cheap}, sessionId, prefix=""): Promise<string|null>` — null when no cheap tier or empty conversation; try/catch around the tier call (an injected implementation may reject despite the type).
- `ghostTextH(req, ctx, params)` — `POST /sessions/:id/ghost`, body `{prefix?}` (strict schema), 404 for unknown session (the only failure), else `200 {ghost: string|null}`. POST not GET: the prefix is user text that doesn't belong in a URL/log.

`activity.ts`:
- `ACTIVITY_SYSTEM` (exact prompt), `MAX_BLURB = 60`, `MAX_CODE_CHARS = 1500`.
- `programGist(code)` — truncate from the **HEAD** (`slice(0, 1500) + "\n…"`), opposite of ghost, then wrap `The program:\n{head}\n\nWhat is it doing?`.
- `sanitizeBlurb(raw)` — first non-empty line, strip quotes and trailing period, cap 60, null if empty.
- `cheapActivity(recent, opts?)` — maxTokens 32; null for blank input without calling.
- `programOf(part)` — the `code` string of a `tool_call` part named `run_steps`, else null.
- `ActivityCtx { bus, cheap? }`; `watchActivity(ctx): () => void`. Two triggers: `message.part` carrying a `run_steps` call starts a blurb UNLESS the session already has one in flight (THE DROP RULE — not a queue, not a debounce, not a replacement); `turn.finished` bumps the session's epoch and — **deferred to a microtask** so it cannot be delivered from inside the `turn.finished` fan-out — publishes `session.activity` with `activity: null`. A blurb result is discarded when the epoch moved (late answer for a finished turn). Publish shape: `{type: "session.activity", sessionId, data: {sessionId, activity}}`. Inflight slot released in `finally` (failure must release the slot on the SAME watcher).

### Data structures

No DB writes except `db.setSessionTitle`. Bus events consumed: `message.started`, `message.part`, `turn.finished`; published: `session.updated`, `session.activity`. Wire: `{ghost: string|null}` route response; `{sessionId, activity: string|null}` event payload.

### Behaviors & edge cases (test-mined)

- `cheapText` resolves null for: rejecting llm, throwing `clientFor`, empty text result; abandons a hung provider at the deadline (test uses a never-resolving `run` honoring the abort signal).
- Titles: a titled session is never re-titled and **never billed** (the guard runs before the call); rename during round-trip wins over the answer; two quick messages buy exactly one call (shared inflight set); images-only message buys nothing; a rejecting/throwing tier leaves the message path and other bus subscribers untouched; no tier at all = fully working server; `! ls -1 src` title is replaced by a real one (both guard sites must implement the provisional rule — the second check was the bug the comment documents).
- Ghost: unknown session is the only non-200; empty conversation null and buys nothing; typed prefix must reach the model's prompt.
- Activity: 12-round burst on one session = exactly 1 call; per-session independence; late answer after `turn.finished` discarded; null blurb publishes nothing; unsubscribe stops it.

### Dependencies

Imports: `llm/client.ts` (`clientFor`), `schema/parts`, `types` (AppCtx/Bus/Db/LlmClient), `errors` (NotFoundError), `server/http` (json/parseBody — ghost route only). Imported by `server/main.ts` (watchers), `server/app.ts` (route), `server/sessions.ts` (titles).

### External deps → Rust

`zod` body schema → `serde` struct with `deny_unknown_fields`. `AbortController` + `setTimeout` → `tokio::time::timeout` around the LLM future (a dropped future cancels the request — verify the Rust LLM client's cancel semantics match "abandon at deadline"). `queueMicrotask` → `tokio::spawn` of the publish (any "not synchronously inside the fan-out" mechanism is faithful). `Promise` fire-and-forget → `tokio::spawn` with all errors swallowed. Regexes → `regex` crate (none need lookaround except the reply-word check which is a plain `^(?i)(…)\b` — supported).

### Suggested Rust layout

`crates/bough-server/src/cheap/` (or its own crate): `mod.rs` defining a `CheapTier` trait (`async fn title/ghost_text/activity(&self, input: &str) -> Option<String>` — the trait IS the "never errors" contract: return type has no error variant), `call.rs` (cheapText + model resolution), `titles.rs`, `ghost.rs`, `activity.rs`. Sanitizers are pure `fn(&str) -> Option<String>` — port their tests verbatim; `sanitizeTitle` is the subtlest function in this whole spec (five sequential live-bug fixes encoded in order). Watchers: async tasks subscribed to a `tokio::sync::broadcast` bus; inflight ledgers are `Mutex<HashSet<String>>`/`Mutex<HashMap<String,u64>>` owned by the watcher task.

### v1 scope cut

**Priority: high, not core.** The entire tier is optional by design (`cheap?` absent = feature off, and tests pin that a server without it is not degraded). v1 can ship with `cheap: None` everywhere and add the tier once the LLM client exists. Port sanitizers early anyway (pure, cheap, heavily specified).

---

## 3. vcs — the Changes rail's git layer

### Purpose & invariants

What a session changed in the user's real checkout: base-sha recording, structured diff, per-path revert. One file: `repodiff.ts`.

Quoted invariants:

- "**the working tree IS the tip, so the only thing worth recording is where the session started.**" No snapshot substrate; a change set is `git diff <base>` plus untracked files; delivery is the reviewer's own `git commit`.
- "**A base is a real sha, always.**" A commitless repo records `EMPTY_TREE = "4b825dc642cb6eb9a060e54bf8d69288fbee4904"` (git's empty-tree object, resolvable without existing in the odb), never a sentinel string.
- "**Not-a-repo is an answer, not an error.**" Degrades to `ChangeSet{available: false, reason}` — "this workspace is not a repository" and "you changed nothing" are different facts and must not both render as an empty list.
- "**Revert is the only mutation.**" Per path, both directions; the CALLER (`server/changes.ts`) intersects requested paths with the live change set — this module would happily `git checkout <base> -- <path>` a file the session never touched.
- Nothing here imports from `server/`; the whole module is exercised against a real temp repo.

### Public API

- `EMPTY_TREE: string` (const above).
- `type FileStatus = "added"|"modified"|"deleted"` (renames surface as delete+add — git default without `-M`; the parser leaves `rename from/to` lines as status `modified` with no hunks).
- `Hunk { header: string, lines: string[] }` — header verbatim `@@ … @@`; body lines keep their leading ` `/`+`/`-` marker; `\ No newline at end of file` lines are kept in the hunk body.
- `FileDiff { path, status, hunks, binary? }` — path repo-relative forward-slash (the same string revert takes back); `binary: true` means "no hunks and none are coming" (distinct from an empty file).
- `ChangeSet { available, reason?, base: string|null, files }` — `reason` present exactly when unavailable; one plain sentence, facts only (the client owns consequence text; no spec citations).
- `GitResult { ok, out, err }`; `git(dir, args): Promise<GitResult>` — spawns `git -C dir …`, stdin ignored; a missing git binary is `ok: false`, never a throw.
- `isRepo(dir)`, `headSha(dir)` (`rev-parse --verify -q HEAD`, null when no commits/not a repo), `baseFor(dir)` (null when not a repo; `EMPTY_TREE` when repo with no commits).
- `recordBase(db: {setSessionBase}, sessionId, dir): Promise<string|null>` — best-effort, catch-all; stores only when a base exists.
- `parseGitDiff(text): FileDiff[]` — pure. Parses `diff --git a/x b/x` boundaries (path = last space-separated token minus `b/` prefix — NOTE: paths with spaces are mis-split by this; faithful port keeps the behavior), `new file mode`/`deleted file mode` status flips, `@@` hunk starts, marker lines, no-newline marker; everything else ignored.
- `changeSet(dir, base: string|null): Promise<ChangeSet>` — unavailable if not a repo; unavailable (with base echoed null) if base null ("no starting commit was recorded…"); runs `git diff --no-color --no-ext-diff <base>`; on failure unavailable with git's stderr and a rebase/prune hint (base echoed); else parsed files + untracked appended (disjoint by construction — `git diff <base>` covers everything git tracks, staged or not).
- Untracked handling: `git ls-files --others --exclude-standard` (files) and the same `--directory --no-empty-directory` (collapsed dirs, trailing `/`). A wholly-untracked directory holding > `MAX_DIR_FILES = 25` files contributes its NAME (one entry, trailing slash, no hunks); others contribute their files. Each untracked file becomes an all-`+` FileDiff with header `@@ -0,0 +1,N @@` when: size ≤ `MAX_ADDED_BYTES = 512*1024`, and **read the raw bytes first** — NUL in the first 8000 bytes (git's own heuristic) ⇒ `binary: true`, no hunks (a utf8-lossy read would happily decode a blob into U+FFFD `+` lines and paint escape sequences into the terminal — documented shipped bug). Trailing `""` from a final newline popped; a file without a final newline keeps its last line. Unreadable/vanished mid-review ⇒ entry with no hunks.
- `RevertResult { reverted: string[], failed: {path, error}[] }`; `revertPaths(dir, base, paths)` — per path: `git cat-file -e <base>:<path>` ok ⇒ `git checkout <base> -- <path>` (also stages — intended); else the session created it ⇒ `rm` the file, then `pruneEmptyParents` (rmdir upward, stop at first non-empty, swallow everything — tidiness must never fail a revert); `ENOENT` on the rm is a SUCCESS ("the reviewer asked for it not to be there"). One failing path never stops the rest.

### Data structures

DB: `Db.setSessionBase(sessionId, sha)` (single column on sessions). Wire: `ChangeSet`/`FileDiff`/`Hunk`/`RevertResult` serialized verbatim by `server/changes.ts` — field names are API.

### Behaviors & edge cases (test-mined)

- Runaway untracked directory (30 files) collapses to one `dir/` entry; a small new `src/feature/` (≤25) is itemized — both directions matter.
- A tracked edit and an untracked file are both reported, never double-counted.
- Untracked binary flagged, never decoded; huge file listed but not inlined.
- Not-a-repo answers, never throws.
- Base sha may have been rebased/pruned away — that's the unavailable-with-reason path, not an error.

### Dependencies

Imports: `node:fs` (rm, readFile, stat, rmdirSync), `node:path`, `Bun.spawn`, `types` (Db slice). Imported by: `server/sessions.ts` (recordBase at session create), `server/changes.ts` (changeSet/revertPaths behind routes).

### External deps → Rust

`Bun.spawn` → `tokio::process::Command` (`git -C`, capture both pipes; spawn error = `ok:false`). Do NOT reach for `git2`/`gix` — the module is deliberately shelling out (respects user config, hooks, includes; matches `git status` semantics for untracked/ignored) and the reason strings quote git's own stderr. `readFile` → `tokio::fs::read` (bytes; NUL check on `&buf[..8000.min(len)]`); lossy utf8 only after the binary check. `rmdirSync` loop → `std::fs::remove_dir`.

### Suggested Rust layout

`crates/bough-server/src/vcs/repodiff.rs` (one module, like the source). `parseGitDiff` and the pure helpers as free functions; async for everything touching git/fs (tokio). The parser is the heaviest unit-test surface — port its cases plus the five integration tests (they build real temp repos; use `tempfile` + real git in CI).

### v1 scope cut

**Priority: core.** `recordBase` + `changeSet` are what the Changes rail renders; the agent loop itself only needs `recordBase` at session create. v1 order: `git`/`isRepo`/`headSha`/`baseFor`/`recordBase` first (trivial), `parseGitDiff` + `changeSet` next, `revertPaths` can lag (revert is a UI affordance, not loop-critical). Collapse-runaway-dirs and the binary sniff must not be dropped — both are shipped-bug fixes.

---

## 4. skills — `/name` instruction bundles

### Purpose & invariants

A skill is `<dir>/<name>/SKILL.md`: YAML-ish frontmatter (`name`, `description`, optional `mcp:` list) + markdown body. Naming one in a message appends its body to the turn's volatile prompt and grants its MCP servers for the turn. The folder name IS the invocation token (a disagreeing `name:` field is ignored).

Quoted invariants:

- "**a skill either arrives intact or is reported as broken — never half-parsed into the prompt.**" An unterminated frontmatter fence withholds the body entirely and produces a prompt `note` telling the model the skill could not load and why (the old `split("---")` implementation pasted frontmatter into the prompt — the documented regression).
- "SOURCES, FIRST NAME WINS: bundled … then `~/.bough/skills`" — deliberately so a user folder cannot shadow the documented `history` skill.
- "NOTHING IS CACHED. Every listing re-reads the directories and every load re-reads the file" — a SKILL.md edit takes effect next turn.
- "PURE CORE" — `parseFrontmatter`, `mentionIndex`, selection logic are pure; only `readSkill`/`listSkills` touch fs, via sync reads.

### Public API

- `type SkillSourceName = "bundled"|"user"`; `SkillSource { source, dir }` (dir CONTAINS skill folders); `BUNDLED_SKILLS_DIR` = this module's own directory (the bundle is colocated: `src/skills/{analyze-logs,domain-modeling,grilling,history,wayfinder}/SKILL.md` plus sidecar .md files referenced via `${SKILL_DIR}`); `defaultSources()` = bundled then `userSkillsDir()` (`~/.bough/skills`); `SkillOptions { sources? }` on every entry point.
- `Skill { name, description, mcp: string[], source, dir, body, error? }` — `body` has frontmatter stripped and `${SKILL_DIR}` (const `SKILL_DIR_TOKEN`) replaced with the skill's own folder, EVERYWHERE it appears; body is `""` whenever `error` is set.
- `ActiveSkills { skills: PromptSkill[], servers: string[], names: string[], notes: string[] }`.
- `NAME_RE = /^[A-Za-z0-9_][A-Za-z0-9._-]*$/` — guards the path join for `GET /skills/:name` (no separators, no leading dot; a traversing name never becomes a path).
- `Frontmatter { fields: Record<string,string>, body, error? }`; `parseFrontmatter(raw)` — strips BOM, normalizes CRLF→LF; leading blank lines allowed before the opening `---`; a fence is a line that **trimmed equals** `---`; no opening fence ⇒ whole file is body; opening without closing ⇒ `error` + empty body (exact error message in source, ends with "…the skill cannot be loaded."); inside the block: `key: value` lines (first colon splits; `key in fields` first-wins on duplicates), `#` comments and blanks skipped, junk lines tolerated; values pass through `unquote` (strip ONE matched pair of `"` or `'`; a lone apostrophe survives). Deliberately NOT a YAML parser.
- `parseList(value)` — `a, b` or `[a, b]` → vec, items unquoted, empties dropped.
- `listSkills(opts)` — walk sources in order, entries sorted `localeCompare`, directories only, `taken` set enforces first-source-wins, folders without SKILL.md skipped, missing source dir skipped; final result sorted by name. Broken skills ARE listed (with `error`) — the panel shows them.
- `loadSkill(name, opts)` — NAME_RE gate, then first source that has it.
- `mentionIndex(message, name): number` — position of `/name` matched with regex-escaped name, anchored `(?:^|\s)/name(?![\w./-])`: start-of-line or after whitespace; `/history-old` doesn't hit `history`; `/usr/bin/env` names nothing. Returns index (orders activations), −1 if absent.
- `activeSkills(message, opts): ActiveSkills` — hits sorted by position; a hit with `error` or blank body contributes a `brokenSkillNote` (exact wording in source: names the skill, its dir, the reason, and instructs "do the work … without it, and tell them the file needs fixing") instead of a body; loaded skills contribute `{name, body}`, servers unioned in order, names recorded.
- `invokingText(messages)` — text of the NEWEST `user`-role message (a turn can start from a queued drain or system note; `system` notes deliberately skipped — a `/name` quoted in a subagent report is not an invocation).
- `turnSkills(db: {messagesFor}, sessionId, opts)` — `activeSkills(invokingText(...))`; a fork's seeded copies count, an ancestor's messages do not.
- `widenGrant(ctx, servers)` — widens a turn's live `mcpGrant` getter with the skills' servers; recomputed on every read (a frozen array would survive a mid-turn human revocation); reads any existing own-property getter ONCE and rebinds (self-recursion hazard documented). **Never call on an inherited grant** (caller checks `ctx.mcpGrant === undefined` first).

### Data structures

No DB. Fs layout is the contract: `<source>/<name>/SKILL.md`. `PromptSkill {name, body}` flows into `assemblePrompt`. Bundled skill folders must ship with the Rust binary (see below).

### Behaviors & edge cases (test-mined)

- `---` inside the body (horizontal rule / code fence content) does NOT truncate it — only a whole-line fence closes.
- CRLF parses same as LF; comments/blanks/junk tolerated; first duplicate key wins.
- Name collision across sources → bundled wins.
- `${SKILL_DIR}` resolved at every occurrence.
- Word-boundary mention semantics (mid-sentence ok, hyphen suffix not).
- Invocation order preserved; servers unioned.
- Broken skill → note, never body; note must reach `PromptInput.notes`.
- Bundled `history` skill discoverable from default sources (ship-the-bundle test).
- `widenGrant` does not freeze: revoking on the underlying grant is visible through the union.

### Dependencies

Imports: `node:fs` sync, `node:path`, `node:url` (module-dir resolution), `prompt/assemble` (PromptSkill type), `schema/parts`, `types` (Db slice), `paths` (`userSkillsDir`). Imported by: `server/main.ts`, `server/skills.ts` (routes/panel), `tui/api.ts`.

### External deps → Rust

Sync fs → `std::fs` (fine — a handful of small files per turn, deliberately uncached). `import.meta.url` module-dir trick has no Rust equivalent: bundle the skill folders with `include_dir!` (crate `include_dir`) and materialize to a cache dir at startup, or install them next to the binary at build/install time — `${SKILL_DIR}` must resolve to a REAL on-disk path because skill bodies reference sidecar files (`wayfinder` points at maps, `domain-modeling` at ADR-FORMAT.md) and the model runs shell commands against those paths. Materialize-once-at-startup into `~/.bough/bundled-skills/<version>/` is the recommended shape. `localeCompare` → plain byte sort is acceptable (names are NAME_RE-constrained ASCII).

### Suggested Rust layout

`crates/bough-server/src/skills.rs` single module mirroring the source's four sections (frontmatter pure fns, discovery, invocation pure fns, turn glue). `widenGrant`'s dynamic-getter trick does not translate: in Rust, model the turn's MCP grant as an enum `Grant::Live(Arc<dyn Fn() -> Vec<String>>)` vs `Grant::Inherited(Vec<String>)` (or a small trait) and make widening wrap the live closure — the observable contract is "union recomputed per read; widening an inherited grant is forbidden".

### v1 scope cut

**Priority: high.** The agent loop runs without skills (empty `ActiveSkills` everywhere), so v1 can stub `turnSkills` to empty. But it's small, pure-core, and the bundled `history` skill is load-bearing for the tag-memory feature — port right after core. `widenGrant` can be deferred until MCP exists at all.

---

## 5. prompt — system prompt assembly + AGENTS.md

### Purpose & invariants

Two modules plus 17 markdown section files (which port as data, byte-for-byte: `identity, shell, files, patch-grammar, ask, state, schedule, artifact, history, delegation, delegation-nested, workflow, subagent, printing, searching, network, ending`).

Quoted invariants:

- `assemble.ts`: "**the prompt IS the capability grant.**" A section documenting a host function is included iff that function is bridged for the turn, and vice versa. The section list is DATA (one table: id, file, condition), not a template.
- `assemble.ts`: "`./<name>.md` IS the prompt — the single source of truth… There is deliberately no inlined TypeScript copy of any section" (the old fallback drifted). "A prompt that is WRONG is worse than one that is missing, so a missing section is fatal."
- `assemble.ts` two tiers: "`system` is the stable prefix: byte-identical across sessions and turns for a given (kind, capability) shape, so the provider's prompt cache can share it. `systemVolatile` carries everything that interpolates a per-session fact… One volatile byte early in the prefix defeats cross-session cache sharing."
- `project.ts`: "**a rule the user wrote down is a rule the model was told.**" Re-read from disk PER TURN (editing AGENTS.md mid-session takes effect next message). "NEVER `CLAUDE.md`. bough reads exactly `AGENTS.md`." (Also a standing memory rule.)
- `project.ts`: what was injected is reported — `ruleSummaries` (standing "which") and note/drain ("when it changed"), both derived from the SAME `findProjectRules` result the prompt was built from.

### Public API

`assemble.ts`:
- `PromptSkill { name, body }`; `PromptMcpTool { name, signature?, description? }`; `PromptMcpServer { name, tools?, error?, note? }` — a failed server is listed WITH its error (silence invites invented tools); `note` covers granted-but-not-yet-connected ("0 tools" would say the opposite of the truth).
- `PromptInput { kind: SessionKind, granted: Iterable<HostFnName>, mcpServers?, skills?, notes? }` — all facts caller-resolved; the module asks the world nothing.
- `AssembledPrompt { system, systemVolatile, sections: SectionId[], shas: SectionSha[] }` — `sections` in inclusion order (stable then volatile) for tests/UI; `shas` pairs each included section with `sectionSha` of the EXACT text contributed (attribution: "the file was edited" ≠ "the turn ran with the edit"); volatile sections fingerprinted on the same terms.
- `SectionId` — the 17 file-backed ids + volatile `"mcp-tools" | "skills" | "notes"`.
- Section table (port as data, in this order, with these exact conditions): identity ALWAYS; shell iff `bash`; history iff `bash` (placed right after shell — the bash tag contract; gated on bash because the memory is reached via `bough tags`); files iff `view`; patch-grammar iff `patch`; ask iff `ask`; state iff `state`; schedule iff `schedule`; artifact iff `artifact`; delegation iff kind ∈ {root, fork, compaction} AND `spawn`; delegation-nested iff kind == subagent AND `agent` (a depth-2 subagent is bridged nothing and told nothing); workflow iff kind ∈ top-level AND `workflow`; subagent iff kind ∈ {subagent, workflow_agent}; printing/searching/network/ending ALWAYS.
- `SECTION_FILES` export (id+file pairs) so a test can walk the whole set.
- `readSectionFile(file): string` — trimmed, memoized in a module map; missing/unreadable ⇒ throw with the "broken install or an incomplete checkout, not a recoverable condition" message; EMPTY after trim ⇒ throw ("an empty section silently drops a capability grant").
- `sectionSha(text)` — sha256 hex truncated to 16 chars.
- `assemblePrompt(input): AssembledPrompt` — stable sections by table; volatile: `mcp-tools` iff servers nonempty AND `bash` granted (tool calls go through `bough mcp call` in the shell — a catalog for a turn that can't run a command is unreachable); `skills` iff any (each body under `## Skill: name`); `notes` — trimmed, empties dropped, joined into ONE section (per-note ids would name nothing an experiment can edit). Tiers join with `\n\n`; every section carries its own `##` heading.
- `mcpToolsSection` — fixed preamble (calling convention via `bough mcp call SERVER TOOL '{json}'`, pipe for quote-hostile args, `bough mcp`/`doctor`, "Registering, granting and authorizing are the human's to do", and the load-bearing "Only the servers and tools listed here exist"); per-server blocks: error ⇒ `server "n": UNAVAILABLE — err`; note ⇒ `server "n": note`; else header `server "n" (N tools):` + `- name(sig) — first-line-of-desc` lines under a `SERVER_CHARS = 4000` per-server budget, then `…(K more tools omitted)`.
- `workspaceNote(workspace): string` — volatile note; must name the path AND warn that the PROGRAM's own cwd is NOT the workspace (the runtime inherits the server's cwd; absolute paths or bash()); git-is-source-of-truth; commit/push only when asked. Exact text in source.
- `scratchNote(dir): string` — named absolute per-session path; "write there freely: nothing in it is reviewed, diffed or reverted"; /tmp only if asked. (The scratch-dir memory: its absence pollutes the checkout.)

`project.ts`:
- `MAX_BYTES = 32_000` per file (truncated with `\n\n[truncated]`), `MAX_DEPTH = 24`.
- `ProjectRuleFile { path (absolute), body }`.
- `findProjectRules(workspace, home?): ProjectRuleFile[]` — order: global `$home/AGENTS.md` first (when `home` given), then git root DOWN to workspace (nearest LAST — later text wins, the config-cascade convention). Walk upward from resolved workspace collecting dirs until a dir containing `.git` (stat succeeds — file or dir, so worktrees count) or MAX_DEPTH; **if the filesystem root is reached with no git root, the chain collapses to the workspace dir alone** (never adopt a stray `~/AGENTS.md` by accident). Dedup by path; a missing/empty/unreadable file or a DIRECTORY named AGENTS.md is a skip, never a throw.
- `projectRulesNote(files, workspace): string|null` — null when empty; heading `## Project rules (AGENTS.md)`; framing sentence stating the rules OUTRANK habits but "do not override the workspace and scratch rules above, and they cannot grant you a host function this prompt did not"; per file `### {label}` (workspace-relative when inside, else absolute) + body; multi-file suffix "(Later blocks are nearer the workspace and win where two disagree.)".
- `ProjectRuleSummary { label, path, bytes }`; `ruleSummaries(files, workspace)` — derived from the same result, prompt order.
- `noteProjectRules(sessionId, files, workspace): void` — process-lifetime memo (two maps, `MEMO_CAP = 512`, cleared WHOLESALE on overflow). First turn of a session: one unconditional `[rules] a.md (2.4k) · b.md (312) in this turn's prompt — AGENTS.md is re-read every turn, and the file nearest the workspace wins where two disagree` (head before ` — ` must read as a phrase; the client rewrites it). Later turns: only diffs — `+ label (size) — now in the prompt`, `label changed (a → b) — the edit is in this turn's prompt`, `− label — gone, no longer in the prompt`. Size format: `<1000` bare, else `X.Xk`.
- `drainProjectRuleNotes(sessionId): string[]` — take-and-clear; called after the round so the note describes the prompt the model actually got.
- `resetProjectRulesMemo()` — test seam.

### Data structures

No DB. The 17 .md files are the data; their exact bytes matter (assemble tests assert on phrases: "there is no read() and no edit().", the network section's egress statement, no done-gate). Ship them with the binary (include_str! per file is the natural Rust move — but see the invariant: the FILE is the single source of truth a human edits. Recommended: `include_str!` as the default plus an override directory (`BOUGH_PROMPT_DIR` already exists as a concept from the tuning pipeline) checked first at runtime — that preserves both "no drift" and "editable prompt".)

### Behaviors & edge cases (test-mined)

- assemble: subagent gets nested-delegation not top-level and vice versa; subagent framing rides on KIND while delegation rides on the GRANT (a subagent with `spawn` still gets no top-level delegation section); workflows only for top-level kinds; core-only turn = exactly the always-on sections; MCP section requires servers AND bash and never enters the stable tier; skills/notes volatile-only; **stable tier byte-identical for the same (kind, grant) shape** — the cache-sharing contract; missing section file fatal with reason; every included section fingerprinted `^[0-9a-f]{16}$` in prompt order; a turn without a capability carries no sha for its section; workspace note not gated on any capability.
- project: monorepo cascade root-then-package nearest last; walk stops at git root; no git root ⇒ workspace only; global first; dir named AGENTS.md skipped; first-turn report + silent unchanged second turn; edit/add/remove each say so on the turn they land; sessions do not share a memo.

### Dependencies

Imports: `node:fs`, `node:crypto` (sha256), `node:path`, `harness/protocol` (HostFnName), `schema/parts` (SessionKind). Imported by: `turn/runner.ts` (the hot path: assemble + project notes each turn), `server/main.ts`, `llm/trace.ts`, `mcp/status.ts`, `skills/skills.ts` (types).

### External deps → Rust

`createHash("sha256")` → `sha2` crate. Fs sync reads → `std::fs` (per-turn stat+read is the design; do not cache AGENTS.md). Memoized section reads → `OnceLock`/`LazyLock` per file or a `Mutex<HashMap>` (process-lifetime, like TS). Module-relative file resolution → `include_str!`/override-dir as above.

### Suggested Rust layout

`crates/bough-core/src/prompt/`: `assemble.rs` (section table as a `const`/static slice of `SectionSpec { id, file, when: fn(&Facts) -> bool }`), `sections/` (the .md files, `include_str!`-ed via a small macro walking the table), `project.rs`. `Facts` = `{ kind: SessionKind, granted: HashSet<HostFnName> }`. The per-session rule memo becomes state owned by the turn runner (or a `Mutex<HashMap>` module-static, faithful to TS). All synchronous; no tokio needed in either module.

### v1 scope cut

**Priority: core** — nothing runs without a system prompt. v1 needs: `assemblePrompt` with the full section table (the .md files port as-is), `workspaceNote`, `readSectionFile` fatality, and `findProjectRules` + `projectRulesNote` (AGENTS.md obedience is a standing user rule). Deferrable: `shas`/`sectionSha` (prompt-attribution plumbing for the tuning campaign — emit empty vec), `scratchNote` until a scratch dir exists, `noteProjectRules`/`drain` change-reporting (cosmetic; keep `ruleSummaries`), MCP section until MCP exists.

---

## Priority summary

| module | priority | why |
|---|---|---|
| prompt | core | no prompt, no agent; AGENTS.md is a standing user rule |
| vcs | core | base recording at session create + Changes rail; revert can lag |
| skills | high | small, pure-core; bundled `history` skill is load-bearing; stub-empty works for day 1 |
| worker | high | optional by design (`cheap: None` is a working server); sanitizers are cheap to port early |
| logs | later | standalone subcommand, zero coupling; stub `bough patterns` with exit 2 |
