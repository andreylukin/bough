# Port spec: `src/history/` — command-tag memory + history operations

Two clusters share this directory. Cluster A is the **command-tag memory** (record,
hygiene, stats, echo, embed) — the cross-session memory of every shell command bough runs.
Cluster B is the **history operations** on the session tree (branch, fork, compact,
explore, extract, move, handoff, sections, unsend) — branching, summarizing, copying and
retracting turns. `docs/tags.md` is the narrative contract for cluster A and is worth
porting alongside the code; `docs/spec.md` §2.4/§14 for cluster B.

---

## 1. Purpose & invariants

### Cluster A — the tag memory

Design bet (record.ts header, verbatim): *"the model labels its own INTENT at generation
time (`bash(cmd, "psql:migrate")`), which is nearly free and far more accurate than
post-hoc clustering of command strings — the tag is the stable join key across sessions,
the exit code is the ground truth that weights it."*

Invariants (docs/tags.md §9, all load-bearing):

- **Recording never fails a turn.** *"Everything here is best-effort and MUST NEVER
  surface a failure into a turn: a broken git checkout, a locked database, or a weird
  command string loses one memory row, not the round."* (record.ts). Same contract in
  stats.ts (*"Stats are a garnish; a failure here must not touch the turn"*) and echo.ts
  (*"recall is a side channel, and a broken lookup must never be a broken round"*).
- **The insert is atomic.** History row + tag rows + dir rows + FTS row in ONE
  transaction (`Db::record_command`), or popularity joins skew silently.
- **The priming note is frozen per session.** The volatile prompt tier caches per session
  (1h TTL); a note whose text drifts mid-session busts that cache. Memoized by session id
  for process lifetime, bounded at 512 entries (cleared wholesale at cap, not LRU).
  Mid-turn information goes into the round's *result*, never the prompt.
- **CLI and prompt share one ranking.** `bough tags` default view IS the priming note's
  ranking, from the same functions.
- **All SQL lives in `db/`** (the one exception: the user-supplied string `bough tags sql`
  runs against a read-only handle).
- **Normalization is pure and total.** `normalize_tags` never throws/panics.
- **100% tag coverage never slips**: hygiene may never untag a command outright
  (hygiene.ts: if everything was an echo, keep the first tag).
- **Vector layer is optional & fully derived.** embeddings.db can be deleted freely;
  its absence is not an error, and vectors NEVER live in bough.db (other connections lack
  the vec0 module and must not walk a file containing a virtual table they can't parse).

### Cluster B — history operations

- **branch.ts**: *"a seeded message is stamped with the real clock, never with an
  advanced artificial one."* Messages order by `(created_at, rowid)`; insertion order
  breaks same-millisecond ties. Never derive a timestamp from the previous one
  (`base + i` reorders history under a turn that starts microseconds later).
  Also: **every seeded message is announced as `message.started`**, and the session is
  announced (`session.created`) *before* any message.
- **compact.ts**: *"compaction never mutates the session it compacts."* It branches a
  SIBLING (`parent_id = target.parent_id`) — source rows are byte-identical afterwards
  (tests assert JSON equality). Summarize FIRST, write second: a failed summarizer leaves
  no half-seeded branch.
- **fork.ts**: *"the source session is byte-identical afterwards."* "Edit & resend" is a
  new message on a new session; nothing is ever rewritten in place.
- **extract.ts**: *"extract is the one selection op that is not bounded by the session's
  own messages"* — picks resolve against the full visible thread (`thread_for`), because
  the new session is a ROOT (`parent_id = null`) that inherits nothing.
- **move.ts**: *"'move' is a lie the name tells and the implementation never does. It is
  a COPY."* Source untouched; target gains fresh-id copies at its tail.
- **handoff.ts**: *"nothing is copied and nothing is mutated"* — the distilled context
  lives entirely in the new root's `draft` (prefilled composer text, not a turn). Draft
  FIRST, session second: an empty/failed draft leaves no empty root.
- **explore.ts**: the compaction scout *"IS ENRICHMENT, AND IT NEVER FAILS THE
  COMPACTION"* — every failure path returns `None`.
- **sections.ts**: *"a stateless labeling pass, and the CLIENT decides what a turn is."*
  No DB reads, no writes, no sections table anywhere.
- **unsend.ts**: the take-back deletes IN PLACE — the only destructive write on the
  thread — and is defensible only because the rules are narrow (own messages, user role,
  last user message only).

---

## 2. Public API

### record.ts
- `struct FinishedCommand { command: String, tags: String /* normalized, "" = none */, exit_code: Option<i64> /* None = still running */, duration_ms: Option<i64>, output_head: String /* first ~2k chars as program saw them */, spill_path: Option<String> }`
- `const OUTPUT_HEAD_CHARS: usize = 2_000` — chars of output one row keeps inline.
- `fn spill_path_from(output: &str) -> Option<String>` — parses the spill marker
  regex `FULL OUTPUT SAVED[^\n]*\n\s+(\S+)\n` back out of output text.
- `type CommandRecorder = impl Fn(FinishedCommand)` (per-turn closure).
- `fn is_ref(tag: &str) -> bool` — `tag.contains('.')` (on a *normalized* tag).
- `fn normalize_tags(raw: Option<&str>) -> String` — lowercase, split, slugify, cap at 8,
  join with `:`; `""` = "no tags given".
- `fn split_tags(tags: &str) -> Vec<String>` — split on `:`, dedupe (preserving first
  occurrence); `""` → `[]`.
- `fn repo_identity(workspace: &str) -> String` — git `remote.origin.url` (via
  `git -C <ws> config --get remote.origin.url`, 2s timeout) else the workspace path.
  Cached per workspace for process lifetime.
- `fn find_git_root(dir: &str) -> Option<String>` — walk up ≤32 levels stat-ing `.git`;
  cached per starting dir.
- `struct Attribution { repo: String, rel_dirs: Vec<String>, abs_dirs: Vec<String> }`
- `fn attribute_command(command: &str, workspace: &str) -> Attribution` — scope from the
  paths a command TOUCHES, not the cwd.
- `struct RecorderCtx { db, session_id, workspace, message_id: Option<String>, now: Option<Clock>, touched: Option<SharedVec<String>> }`
- `fn create_command_recorder(ctx: RecorderCtx) -> CommandRecorder` — swallows every
  error; also pushes `abs_dirs` into `ctx.touched` (the dir-hint trigger input).

### hygiene.ts
- `fn canonical_by_stem(vocab: &HashMap<String, u32>) -> HashMap<String, String>` — for
  each stem, the most-used word reducing to it; ties broken alphabetically (smaller
  string wins).
- `fn clean_tags(tag_list: &[String], command: &str, vocab: &HashMap<String, u32>) -> Vec<String>`
  — snap-then-drop write-time hygiene (rules in §4).
- `fn clean_tag_string(tags: &str, command: &str, vocab) -> String` — over the
  colon-joined form.

### stats.ts
- `fn tag_weights(rows: &[CommandTagRow], now: i64) -> HashMap<String, f64>` — ACT-R
  base-level activation per tag: `Σ successFactor(exit) × (elapsed_days)^-0.5` with
  elapsed floored at 1h. successFactor: exit 0 → 1.0, None → 0.5, else 0.25. The ACT-R
  `ln` is deliberately dropped (monotone, and it would break the idf product / go
  negative).
- `struct RankedTag { tag: String, weight: f64, repos: u32, score: f64 }`
- `fn rank_tags(weights, spread: TagSpread, limit, uses: Option<&HashMap<String,u32>>) -> Vec<RankedTag>`
  — score = `weight × ln(1 + N_repos / repos_using(tag))`; filters singletons (only when
  `uses` given) and references; sort by score desc, tie by tag asc.
- `fn workspace_repo(workspace: &str) -> String` — `repo_identity(find_git_root(ws) ?? ws)`.
- `fn top_repo_tags(db, workspace, now, limit = 10) -> Vec<String>`
- `fn ranked_repo_tags(db, repo, now, limit = 10) -> Vec<RankedTag>` — same ranking with
  arithmetic attached, for `bough tags`.
- `fn tags_note_for(db, session_id, workspace, now) -> Option<String>` — the volatile-tier
  priming note; memoized per session; `None` for a project with no history (and stays
  None — the None is memoized too).
- `fn primed_tags(session_id) -> HashSet<String>` — what the session was primed with.
- `fn primed_tags_for(db, session_id, workspace, now) -> Vec<String>` — TUI snapshot view;
  computes-and-freezes via the same memo.
- `fn dir_tag_hints(db, session_id, workspace, abs_dirs: &[String], now) -> Vec<String>` —
  `[history] tags previously used in <label>/: …` lines; divergence-only, once per dir,
  ≤4 per session.
- `fn reset_stats_memo()` — test seam clearing all three memos.
- Constants: `LOOKBACK_MS = 150 days`, `TOP_TAGS = 10`, `DECAY_D = 0.5`,
  `RECENCY_FLOOR_MS = 1h`, `MAX_HINTS_PER_SESSION = 4`, `MEMO_CAP = 512`.

### echo.ts
- `struct EchoCtx { db, session_id, workspace, now }`
- `trait CommandEcho { fn note(&self, command, exit_code, output) -> Option<String>; fn guard(&self, command) -> Option<String>; }`
- `fn create_command_echo(ctx) -> impl CommandEcho`.
  - `note` — AFTER a failure: appends "[history] this exact command already failed here
    N× (last Xm ago): <first error line>" and/or the error-signature line "N other
    commands here failed the same way … The command has been changing; the mistake has
    not. Fix the mistake, not the arguments.", plus "this exited 0 here: <cmd>" when a
    2-token LIKE-prefix sibling succeeded. Returns None on success (exit 0 or None) and
    on any internal error.
  - `guard` — BEFORE running: returns the refusal text ("[not run] this identical command
    has failed N times in this session in the last 2 minutes…") instead of running, or
    None to run. Fires ONLY on: same session + byte-identical command + ≥3 failures
    inside 2 minutes. The error-signature path NEVER guards (debugging/enumeration look
    identical to a misconception from outside).
- Constants: `ECHO_WINDOW_MS = 14d`, `LOOP_WINDOW_MS = 2min`, `LOOP_THRESHOLD = 3`,
  `ERROR_CHARS = 220`, `PREFIX_TOKENS = 2`, `ERROR_WINDOW_MS = 24h`,
  `ERROR_SCAN_LIMIT = 400`, `ERROR_SPREAD_MIN = 2`.

### embed.ts
- `struct EmbedOptions { bough_db: Option<PathBuf>, embed_db: Option<PathBuf>, model_path: Option<PathBuf> }`
- `trait EmbedLayer { async fn drain(&self) -> usize; async fn similar(&self, text: &str) -> Result<Vec<SimilarRow>>; fn close(&self); }`
- `fn create_embed_layer(opts) -> Option<EmbedLayer>` — `None` when the process cannot
  host it (no extension support / `BOUGH_NO_EMBED`); callers treat None as "the feature
  does not exist". `drain` failure → 0 (retry next tick; a failed init is un-memoized so
  offline-first-boot retries). `similar` failure → a plain explanatory Error (it's only
  reachable when the layer exists; the model deserves to know why recall failed).
- `SimilarRow` fields (wire shape, from the SQL): `cmd, tags, repo, exit_code, ts,
  distance` (distance rounded to 4 places).
- Constants: `DRAIN_BATCH = 64`, `KNN_LIMIT = 10`, `DOC_CMD_CHARS = 500`,
  model = all-MiniLM-L6-v2 q8_0 GGUF (~25MB, 384 dims) auto-downloaded from HuggingFace
  to `~/.bough/models/` (temp-name + rename so a killed download can't leave a
  half-written file); `BOUGH_EMBED_MODEL` env or `model_path` opt overrides (and then a
  missing file is an error, not a download).

### branch.ts
- `fn merge_picks(picks: &[PartPick]) -> IndexMap<String, Option<Vec<usize>>>` — whole-
  message pick (`parts: None`) wins over partial; partials union + sort.
- `fn pick_parts(m: &Message, indexes: Option<&[usize]>) -> Option<Vec<Part>>` — `None`
  index-list = all parts; out-of-range index → `None` (caller's 400).
- `struct ResolvedPick { idx: usize, view: Message }`
- `fn resolve_picks(thread, picks, err: impl Fn(&str) -> E) -> Result<Vec<ResolvedPick>, E>`
  — merge, validate membership + ranges, return views **in thread order** (client sends a
  selection, not a sequence).
- `fn base_title(title: &str) -> String` — strips `^((fork|extract|subagent|handoff) · )+`
  once (note: `compacted` is deliberately NOT in the strip list).
- `struct BranchSpec { parent_id: Option<String>, title, kind, workspace, origin_dir, base, origin_id, origin_message_id }` — optional fields present-only-when-set on the stored row.
- `fn open_branch(ctx: &BranchCtx, spec) -> Seeder` — creates session, publishes
  `session.created` with what STORAGE kept (read-back, not the argument), returns seeder.
- `struct Seeder { session }` with `add(role, parts) -> Message` (fresh uuid, `pending:
  false`, `created_at = now()` read once per message; indexes into messages_fts quietly —
  an index failure logs and degrades search, never aborts the seed; publishes
  `message.started`) and `copy(m) -> Message` (new id, deep-copied parts via JSON
  round-trip). Constructed directly (without `open_branch`) by move-into.

### compact.ts
- `fn render_span(messages: &[Message]) -> String` — one line per part; exhaustive match
  over the Part enum (a new part kind must be a compile error). tool_call/tool_result
  payloads clipped at `PART_CLIP = 2000` chars; `tool_result (error)`/`(interrupted)`
  suffixes; ask parts render Q → answer; empty-parts message renders as `role:`.
- `fn runs_of(picked: &[ResolvedPick]) -> Vec<Run { start, end, span }>` — maximal runs of
  ADJACENT thread indexes; each run → ONE summary; unselected messages copied verbatim
  between summaries.
- `async fn summarize_span(ctx, model, span, instructions) -> Result<String>` — exported
  for fork's abandoned-path summary.
- `async fn compact(ctx, session_id, args: CompactBody) -> Result<Session>` — errors:
  404 unknown session; 400 empty picks / no own messages / pick in ancestor history
  (names the ancestor: "Compact <ancestor> instead") / foreign pick / out-of-range part;
  502 empty summary ("nothing was written; retry, or narrow the selection").
- `async fn compact_h(...)` — `POST /sessions/:id/compact` → 201 `{session, thread}`
  (thread = `thread_for`, not just seeded messages).
- Summary message role is `supervisor` (replays as assistant — a continuation, not a
  harness note). Branch inherits workspace + base + originDir + model/effort pins;
  lineage `origin_id = source.id`, `origin_message_id =` last picked message. Title
  `compacted · N turn(s)`; cheap-tier retitle to `<title> · compacted N` fire-and-forget,
  skipped if the user renamed first, all failures swallowed.
- Model resolution: `session.model ?? ctx.model ?? DEFAULT_MODEL` (session pin is a
  provider-routing decision).

### explore.ts
- `DEFAULT_EXPLORE_MODEL = "gpt-5.6-luna"`; `fn explore_model(env) -> String` reads
  `BOUGH_COMPACT_EXPLORE_MODEL`.
- `fn touched_paths(span, workspace) -> Vec<String>` — regex
  `[\w./-]*[\w-]+\.[A-Za-z]\w{0,7}\b` over the RENDERED transcript, dedup, resolve
  against workspace, refuse paths outside it, keep only those that `exists()`. (Paths
  live in `run_steps` program source strings — there is no structured field.)
- `fn touched_dirs(paths) -> Vec<String>` — parent dirs, deduped, order-preserving, ≤6.
- `async fn explore_span(ctx, span) -> Option<String>` — a bash-only scout subagent:
  ≤6 tool rounds then a forced write-up round (tools = [], tool_choice none) so an
  overrun still yields notes; 90s wall clock (AbortSignal); output per command clipped
  at 4000 chars; scout's own bash calls are tagged `compact:explore:scout` and land in
  the tag history. EVERY failure returns None.

### fork.ts
- `struct ForkResult { session, messages, turn_started: bool, done: Option<Future> }`
- `fn fork(ctx, session_id, body: ForkBody, deps) -> Result<ForkResult>` — the four cuts
  (all seed the strict prefix before `at_message_id` first):
  1. `edited_text` (no `at_part`): replaces the at-message (must be role `user`, 400
     otherwise; text trimmed, empty → 400) and starts a real turn via the injected
     starter; unwired starter → branch exists, `turn_started: false`.
  2. plain: also copy the at-message; no turn.
  3. `exclusive: true`: skip the at-message copy; no-op (not an error) when combined with
     `edited_text` or `at_part`.
  4. `at_part: i`: copy the at-message truncated to `parts[0..=i]` (out of range → 400);
     with `edited_text`, the edit is appended AFTER the cut and any role is allowed.
- Fork point must be one of the session's OWN messages; ancestor message → 400 naming
  the ancestor ("fork <ancestor> instead"); unknown session → 404. All validation BEFORE
  `open_branch` (or a bad request leaves an announced empty branch).
- Title: `fork · <first text line of at-message, word-boundary-clipped at 48 with …>`,
  falling back to `base_title(source.title)`. Inherits workspace/base/originDir/pins.
- `fork_session_h` — `POST /sessions/:id/fork` → 201 `{session, thread, turnStarted}`;
  optional `summarizeAbandoned` seeds a best-effort `system` note "Summary of the path
  this branch left behind:\n\n<summary of everything from the fork point to the source's
  end>" (failures logged, never fail the fork).

### extract.ts
- `fn extract(ctx, session_id, args) -> Result<ExtractResult { session, messages }>` —
  picks resolve against `thread_for` (ancestors included — the whole point); new session
  is a root (`parent_id: None`, kind `root`), title `extract · <base_title(source)>`,
  inherits workspace/base/originDir/pins, lineage → source + last picked message.
  Errors: 404 unknown session; 400 empty thread / foreign pick (names where it lives and
  the session to extract from) / unknown message / part out of range.
- `fn inherit_pins(ctx, source, branch) -> Session` — exported; copies model/effort pins,
  re-reads the row, publishes `session.updated`. (fork.ts and compact.ts each have a
  private identical copy; in Rust make it one function.)
- `extract_h` — `POST /sessions/:id/extract` → 201 `{session, thread}`.

### move.ts
- `fn move_into(ctx, target_id, args: MoveBody { source_id, picks }) -> Result<MoveResult>`
  — appends copies of the source's visible-thread picks onto the EXISTING target via a
  directly constructed Seeder. Refusals: 404 unknown target/source; 400 self-move; 400
  target is an ancestor of source (append would land mid-thread of the source's view);
  409 target running a turn (`busy_session_ids`); 400 empty source thread / foreign pick.
  All checks before the first copy.
- `move_into_h` — `POST /sessions/:id/move-into` → **200** (creates no session)
  `{session, thread, appended}` — `appended` because duplicate picks merge, so the count
  written can differ from the count selected.

### handoff.ts
- `async fn handoff(ctx, session_id, args: HandoffBody { goal }) -> Result<Session>` —
  LLM drafts the OPENING PROMPT from the whole visible thread + optional scout notes +
  goal; opens a root (no seeded messages) with `set_session_draft(draft)`; publishes
  `session.updated` with the read-back row. Errors: 404; 400 empty thread ("Start a new
  session directly instead"); 502 empty draft (nothing written). Title
  `handoff · <base_title(source) || clipped goal>` (goal fallback because an untitled
  source produced a forever-"(untitled)" row). `MAX_TOKENS = 8192`. The system prompt's
  two hard-won paragraphs (never reply/ask the user — the draft IS text the user will
  send; scout notes beat the transcript on state, transcript beats notes on
  decisions) must be ported verbatim.
- `handoff_h` — `POST /sessions/:id/handoff` → 201 `{session}` — deliberately NO
  `thread` (empty by construction).

### sections.ts
- `struct Section { start: usize, end: usize /* inclusive, 0-based */, label: String }`
- `SECTIONS_MODEL = "claude-haiku-4-5"` — always the cheap model, never the session's.
- `fn parse_sections(text) -> Option<Vec<RawSection>>` — first `[` to last `]`, JSON
  parse, schema check; tolerates code fences and prose.
- `fn normalize_sections(raw, n) -> Vec<Section>` — force into a clean partition of
  `[0, n)`: drop `start >= n` or `start > end`, clip ends, labels sliced to 60 chars,
  sort, trim overlaps, fill gaps with label `"…"`, tail-fill to n.
- `async fn sectionize(ctx, turns: &[{gist}]) -> Result<Vec<Section>>` — prompt is
  `i. <gist with newlines flattened to spaces>` per line (the numbers ARE the reply
  contract); unparseable reply → 502 "nothing was stored — history is unchanged".
- `sections_h` — `POST /sessions/:id/sections` → `{sections}`; session id validated
  (404) then unused.

### unsend.ts
- `struct UnsendResult { session_id, text /* retracted message's text parts joined+trimmed */, removed: Vec<String>, interrupted: bool }`
- `unsend_message_h` — `POST /sessions/:id/unsend` `{atMessageId}`: refuses (400, each
  naming the operation that DOES work) unless the message is (a) one of the session's own,
  (b) role `user`, (c) the LAST user message. Then: interrupt the running turn FIRST
  (non-blocking), `delete_messages_from(session, target)` second (removes the message and
  everything after — the partial answer it provoked). Late runner writes are UPDATEs
  against absent rows (SQLite no-ops); late events name messages no client holds.

---

## 3. Data structures

### DB tables (db/schema.sql — schema is FROZEN; new columns go at the END)

```sql
command_history (
  id          INTEGER PRIMARY KEY,        -- rowid alias; the join key AND the vec_index rowid
  session_id  TEXT NOT NULL REFERENCES sessions(id),
  ts          INTEGER NOT NULL,           -- epoch ms
  repo        TEXT NOT NULL,              -- scope key: origin URL else path
  cmd         TEXT NOT NULL,
  tags        TEXT NOT NULL,              -- normalized colon-joined; '' = untagged
  exit_code   INTEGER,                    -- NULL = still running when turn moved on
  duration_ms INTEGER,
  output_head TEXT NOT NULL DEFAULT '',   -- first ~2k chars printed, spill marker included
  spill_path  TEXT,                       -- pointer, not a guarantee (file may be cleaned)
  source      TEXT NOT NULL DEFAULT 'live', -- 'live' | 'backfill'
  message_id  TEXT                        -- supervisor message; DELIBERATELY NOT an FK
                                          -- (memory outlives its transcript)
);
CREATE INDEX command_history_repo ON command_history(repo, ts);
command_tags (command_id INTEGER NOT NULL REFERENCES command_history(id), tag TEXT NOT NULL);
  -- indexes: (tag, command_id), (command_id)
command_dirs (command_id ..., rel_dir TEXT NOT NULL);  -- indexes: (rel_dir, command_id), (command_id)
command_history_fts USING fts5(cmd, tags, output_head, command_id UNINDEXED,
                               tokenize = 'unicode61 remove_diacritics 2');
```

`embeddings.db` (separate file, `~/.bough/embeddings.db`):
```sql
embed_meta (key TEXT PRIMARY KEY, value TEXT);      -- key 'model' = "<basename>:<dims>"
vec_index  USING vec0(embedding float[<dims>]);      -- rowid = command_history.id
```
Model change (different `model` value) → `DROP TABLE vec_index` and rebuild from zero
(different models' vectors are not comparable; the store is fully derived).

### Db trait surface consumed by this subsystem (src/types.ts)

```rust
// command-history memory
fn record_command(&self, record: CommandRecord);                       // one transaction
fn command_tag_rows(&self, repo, opts: {dir?, since_ts?}) -> Vec<CommandTagRow>; // dir = dir or descendants
fn tag_spread(&self, since_ts?) -> TagSpread { repos: u32, by_tag: HashMap<String, u32> };
fn tag_diversity_by_day(&self, since_ts, repo?) -> Vec<TagDiversityDay>;
fn commands_for_tag(&self, tag, opts: {repo?, limit?}) -> Vec<TaggedCommand>; // newest first
fn repo_tag_counts(&self, repo, since_ts) -> HashMap<String, u32>;     // COINED tags only (refs excluded)
fn prior_failures(&self, repo, cmd, since_ts, session_id) -> Option<PriorFailures>;
fn recent_failures(&self, repo, since_ts, limit) -> Vec<{cmd, output_head, ts, session_id}>;
fn last_success_like(&self, repo, prefix /* LIKE-escaped */, not_cmd, since_ts) -> Option<String>;
fn program_for_message(&self, message_id) -> Option<String>;
// used by cluster B
fn create_session / get_session / get_session_runtime / ancestor_chain / busy_session_ids
fn create_message / get_message / messages_for /* (created_at, rowid) order */
fn thread_for /* ancestors root→parent, then own */ / delete_messages_from
fn set_session_{title,draft,model,effort} / index_message
```

Key row types: `CommandRecord { session_id, ts, repo, cmd, tags, tag_list, dirs,
exit_code, duration_ms, output_head, spill_path, source, message_id }`;
`CommandTagRow { tag, ts, exit_code }`; `PriorFailures { count, in_session, last_ts,
exit_code, output_head }`; `TagDiversityDay { day /* YYYY-MM-DD LOCAL time */, sessions,
commands, tagged, distinct_tags, distinct_refs, tag_uses, singletons }`;
`TaggedCommand { ts, repo, cmd, tags, exit_code, duration_ms, session_id, message_id }`.

### Wire shapes (exact JSON field names)

- Fork response: `{"session": …, "thread": […], "turnStarted": bool}` (201).
- Compact/extract response: `{"session": …, "thread": […]}` (201).
- Move response: `{"session": …, "thread": […], "appended": n}` (200).
- Handoff response: `{"session": …}` (201) — session carries `draft`.
- Sections: `{"sections": [{"start": 0, "end": 2, "label": "…"}]}`.
- Unsend: `{"sessionId", "text", "removed": [ids], "interrupted": bool}`.
- PartPick request shape: `{"messageId": "…", "parts": [0,2] | absent}`.
- `similar()` rows: `{"cmd","tags","repo","exit_code","ts","distance"}`.
- Bus events: `session.created`, `session.updated`, `message.started` — data is always
  the row READ BACK from storage, never the in-memory argument.

---

## 4. Behaviors & edge cases (mined from tests + comments)

### Tag normalization (record.test.ts, docs/tags.md §2)
- Split on `[:\s]+` FIRST (so a reference is still whole when tested), then per piece:
  if it matches `^[a-z][a-z0-9]*\.[a-z0-9][a-z0-9._/-]*$` (after lowercasing) it is a
  REFERENCE and is kept verbatim — dashes and slashes survive ONLY there
  (`linear.eng-1234`, `branch.claude/tags-history`). Otherwise split on `-+`, strip
  `[^a-z0-9_.]`, and keep only parts containing at least one `[a-z0-9]` (so `...`
  survives the char filter but is dropped — dots are legal tag chars and it would
  otherwise read as a reference).
- `PSQL:Migrate` → `psql:migrate`; `repo-inspect` → `repo:inspect`; `git push` →
  `git:push`; `bun::test:` → `bun:test`; 9+ tags → first 8. `ENG-1234` bare →
  `eng:1234` — **NO bare-number drop rule**: the number half is how a bare-written
  ticket is found again (`bough tags show 1234`).
- `is_ref` is just "contains a dot" on the normalized tag; the full REF regex is only
  a normalization gate.
- `split_tags` dedupes; `""` → `[]`.

### Directory/repo attribution (record.test.ts)
- Token scan capped: 200 tokens split on `[\s;|&<>()]+`, ≤24 stat'd, ≤4 dirs kept.
  Token cleanup: strip surrounding quotes/backticks/trailing comma; take text after
  first `=` (`--output=path`, `FOO=path`); strip `:12`/`:12:3` line refs. Skip tokens
  starting `-` or all-digits, length <2, no `/` and not `name.ext`-shaped, containing
  `://`, or resolving into `/node_modules` or `/.git`.
- File tokens attribute to their `dirname`; directory tokens to themselves.
- The enclosing git checkout containing the MOST touched dirs wins; the row is scoped to
  ITS `repo_identity`, dirs relative to its root (dropping `""`, `.`, `..`-escaping and
  absolute rels). No paths → workspace's own scope (the common cheap path). Absolute
  tokens OUTSIDE the workspace count (a `~`-rooted session working on `~/repos/bough`
  files into bough's memory — the miss this exists to fix).

### Hygiene (hygiene.test.ts)
- Suffix strips, longest first: `ing`, `ed`, `es`, `s`; only when
  `tag.len() > suffix.len() + 2`; a stem is only accepted if it is ALREADY a vocab word.
- SNAP first (so a word novel only by an `s` is known by drop time): a tag not in vocab
  snaps to the canonical spelling of its first matching stem. Works both directions
  (`evaluators`→`evaluator` or `run`→`runs` — the vocabulary decides, not a rule);
  most-used spelling wins, ties alphabetical.
- DROP an echo only when ALL of: vocab has ≥200 distinct words (`MIN_VOCAB_FOR_DROP` —
  the cold-start trap: day-one `git` on `git status` must not be starved out of the
  vocabulary forever), tag not in vocab (post-snap), `tag.len() >= 4`, and
  `command.to_lowercase().contains(tag)`. A vocab word survives echoing (`git` on
  `git status` is that command's best tag).
- References are never snapped, never dropped.
- Output deduped; if empty, keep `tag_list[0]` — never untag outright.
- Vocabulary read once per repo per turn and held STABLE across the turn (a tag coined by
  command 1 must not be "established vocabulary" by command 3). `VOCAB_LOOKBACK_MS` =
  150d, deliberately equal to stats' lookback so hygiene judges against the same
  vocabulary the priming note recommends.

### Ranking (stats.test.ts)
- Power-law decay: doubling age costs √2 (test-pinned). Four old uses can beat one
  recent. Ten failures (0.25 each) lose to two successes.
- `rank_tags` singleton demotion (`uses.get(tag) ?? 2 > 1` — i.e. filter applies only
  when `uses` is provided; per-directory hint lists pass no `uses` and demote nothing).
- References never rank (idf would hand a single-repo ticket the max boost × real
  weight = ticket numbers atop every priming note). They ARE included in dir-hint
  popularity lists (topTags has no ref filter).
- Directory hints: skip non-absolute and already-seen dirs (seen is recorded even when
  no hint emits); dir's repo = its own enclosing checkout (foreign checkout → that
  repo's whole-root profile, labeled `~`-abbreviated; same-repo dir labeled by relative
  path); workspace repo's own root never hints (its profile IS the priming set); tags
  already primed are filtered; empty after filter → no line; hint line format:
  `` [history] tags previously used in <label>/: a, b, c — run `bough tags show <tag>` for the commands behind them ``.
- Note text: `This project's own tag vocabulary — the words it uses that other projects
  do not: <t1, …, t10>. Reuse these when they fit; coin new ones freely when they do
  not, especially for the tool and the intent.`
- Memo cap behavior: at 512 entries the map is CLEARED then re-inserted (wholesale,
  not eviction).

### Echo (echo.test.ts)
- `first_error_line`: first non-empty trimmed line that isn't `[exit code N]`; clipped
  to 220 chars with `…`.
- `ago` vocabulary: `Ns ago` <60s, `Nm ago` <60m, `Nh ago` <48h, else `Nd ago`
  (rounded, not floored).
- `success_prefix`: first 2 whitespace tokens joined; None if <2 chars; `\ % _`
  LIKE-escaped so a command containing `%` cannot widen its own success lookup.
- Guard resets on ANY byte edit; ignores failures older than 2min and other sessions'
  (uses `prior.in_session`); fires exactly at 3.
- Error-signature grouping: scan ≤400 recent failures in-repo within 24h; skip rows
  whose `cmd` equals the failing command; group by identical first error line; speak
  only at ≥2 DISTINCT other commands. A command that printed nothing has no signature.
  THE REAL INCIDENT test: 100 distinct `gh search prs … --state merged` commands, one
  error — byte-exact matching fires zero times; the signature path must catch it.
- Both matchers can speak at once — exact-command line first, then signature lines,
  then the success line.

### Embed (embed.test.ts, embed_fixture.ts)
- vec0 inserts report shadow-table writes in `changes` — count drained rows by
  `count(*)` DELTA, never by changes (4 rows once reported as 14).
- Drain query: `INSERT INTO vec_index (rowid, embedding) SELECT h.id, lembed('embed',
  h.tags || ' ' || substr(h.cmd, 1, 500)) FROM src.command_history h WHERE h.id NOT IN
  (SELECT rowid FROM vec_index) ORDER BY h.id LIMIT 64`.
- Similar query: KNN subselect `WHERE embedding MATCH lembed('embed', ?) ORDER BY
  distance LIMIT 10`, joined back to `src.command_history`.
- Dimension is PROBED (`length(lembed('embed','probe'))/4`), never hardcoded — an
  env-supplied model of any width just works.
- The embed connection opens embeddings.db and `ATTACH`es bough.db as `src`
  (read-only by discipline, not enforcement).
- Bun trap that shaped the fixture: `setCustomSQLite` must precede the FIRST Database
  open in the process (test uses a subprocess). Rust equivalent: whatever libsqlite3 is
  linked must have extension loading compiled in — decide once at startup.
- Fixture acceptance: 4 seeded rows, query "how do I get into the running container" →
  top hit `docker exec -it myapp-dev-1 bash` (a hit no keyword search could make).

### Branching ops (branch/fork/compact/extract/move/handoff tests)
- Seeder + immediately-following turn order correctly even when EVERYTHING lands in one
  millisecond (rowid tie-break carries it); also pinned on the real clock.
- Every op validates fully before `open_branch` (else an announced, empty, half-seeded
  session leaks into the picker on every bad request).
- Source (and its ancestors) byte-identical after fork/compact/extract/move/handoff —
  tests serialize the rows before and after and compare.
- `resolve_picks` errors flow through the caller-supplied error constructor so each op's
  vocabulary and HTTP status are preserved.
- Compaction: non-contiguous selection → one summary PER RUN with unselected messages
  copied between them; one scout per run (never pointed at the union); a part-narrowed
  pick shrinks what the summarizer sees but the message is still WHOLLY replaced.
- Fork `excerpt_of`: first text line; ≤48 kept whole; else cut at 48, back up to last
  space only if `space > 24`, strip trailing `[,;:.]`, append `…`.
- Handoff prompt order: rendered thread, then scout notes (when any), then
  `Goal for the new conversation: <goal>`; no workspace → no scout.
- Sections partition property: output always covers exactly `[0, n)` with no gaps or
  overlaps (gap-filler label `…`).
- Unsend: retracted message stops answering keyword search (delete must clean
  messages_fts); siblings sharing an ancestor untouched; `delete_messages_from` cuts a
  contiguous tail with ties broken by insertion order.
- TS-only hazard (drop in Rust): several HTTP handlers are `function` declarations to
  survive an import cycle with `server/app.ts`; Rust's module system makes this moot.

---

## 5. Dependencies

Imports (cluster A): `../types` (Db, CommandRecord, CommandTagRow, PriorFailures),
`../db/extensions` (embed only), `../paths` (embed), Node `child_process`/`fs`/`path`/`os`.
Imports (cluster B): `../errors` (typed error enums with HTTP statuses), `../llm/client`
(`clientFor`, `completeText`), `../schema/parts` + `../schema/requests` (frozen request
bodies: ForkBody, CompactBody, ExtractBody, MoveBody, HandoffBody, SectionsBody,
UnsendBody, PartPick), `../turn/runner` (DEFAULT_MODEL, interruptTurn), `../turn/queue`,
`../server/http` (json/parseBody), `../hostfn/shell` (explore's bash).

Imported by: `turn/runner.ts` (createCommandRecorder, createCommandEcho, tagsNoteFor,
dirTagHints — the wiring: note into the volatile prompt tier, hints appended to the
round's RESULT), `hostfn/shell.ts` (the boundary that REQUIRES tags on `bash(cmd, tags)`
— a missing/empty tag throws a catchable ProgramError; `sh()` legs may be untagged and
record `tags = ''`; there is no untagged door: execSync/Bun.$/spawn-sh throw),
`server/app.ts` (route table: all seven handlers), `server/sessions.ts` (primedTagsFor on
the snapshot; first post clears the handoff draft), `server/main.ts` + `cli/tags.ts`
(createEmbedLayer; drain is pumped by the server), `tui/api.ts` (Section, UnsendResult
types).

`bough tags` CLI contract (cli/tags.ts, reachable surface for the memory): verbs
`list | show TAG | stats | sql "SELECT…" | similar "text"`; bare word = `show`; `--all`
beats `--repo`; exit codes 0 answered / 1 no memory (or no vector layer) / 2 usage.
`sql` guarantees, structural: `{readonly: true}` open + `PRAGMA query_only = ON` (covers
ATTACH tricks) + SELECT/WITH keyword gate + `busy_timeout = 2000` + 200-row cap.

---

## 6. External deps → Rust equivalents

| TS/Bun | Used for | Rust |
|---|---|---|
| `bun:sqlite` | all storage | `rusqlite` (bundled feature for main db; see extensions note) |
| `sqlite-vec` npm (loadable ext) | vec0 KNN table | `sqlite-vec` crate (bundles the C extension; register via `sqlite3_vec_init`) or load the dylib via `rusqlite::LoadExtension` |
| `sqlite-lembed` (loadable ext) | GGUF embedding as SQL fn | no maintained crate — either load the compiled `sqlite-lembed` dylib via rusqlite `load_extension`, or replace with in-process embedding: `fastembed` / `llama-cpp-2` / `candle` computing the vector in Rust and inserting bytes directly (recommended; kills the extension-loading fragility and the ATTACH dance) |
| `Database.setCustomSQLite` (Homebrew swap on macOS) | extension-capable sqlite | moot with `rusqlite` `bundled` + `loadable_extension`/linked-in vec; keep `BOUGH_NO_EMBED` env gate |
| FTS5 (`unicode61 remove_diacritics 2`) | keyword recall | rusqlite bundled sqlite ships FTS5 — keep tokenizer string identical or FTS results drift from the TS db |
| `child_process.spawnSync("git", …)` | repo identity | `std::process::Command` + 2s timeout (`wait_timeout` crate) — or `gix`/libgit2, but shelling out matches behavior incl. hostile checkouts |
| `node:fs` statSync/existsSync/mkdir/rename | attribution, model download | `std::fs` (sync is fine — these are cheap, bounded stats) |
| `fetch` + `Bun.write` | model auto-download | `reqwest` (async) + tmp-file + `fs::rename` |
| `crypto.randomUUID()` | session/message ids | `uuid` v4 |
| `zod` schemas | request bodies, LLM JSON | `serde` + `serde_json`; manual range validation |
| `AbortSignal.timeout(90_000)` | scout wall clock | `tokio::time::timeout` around the round loop |
| JSON deep-copy of parts | Seeder::copy | `serde_json` round-trip or `Clone` on the Part enum (Clone is fine — Parts are fully typed in Rust, unlike TS's "stray non-JSON value" hazard) |
| process-lifetime `Map` memos | stats/repo/git-root caches | `once_cell::sync::Lazy<Mutex<HashMap>>` or a `StatsMemo` struct owned by the server ctx (prefer owned state over globals; keep the 512-cap clear-wholesale semantics) |
| `homedir()` | hint labels | `dirs::home_dir()` |

## 7. Suggested Rust layout

```
crates/history/
  src/
    lib.rs
    tags/
      record.rs      // normalize_tags, is_ref, split_tags, spill_path_from,
                     // attribution (extract_abs_dirs, find_git_root, repo_identity),
                     // CommandRecorder (a struct with record(&self, FinishedCommand))
      hygiene.rs     // stems, canonical_by_stem, clean_tags
      stats.rs       // tag_weights, rank_tags, StatsMemo { notes, primed, hints },
                     // tags_note_for, dir_tag_hints  — pure fns + one memo struct
      echo.rs        // CommandEcho { note, guard }, first_error_line, ago, success_prefix
      embed.rs       // EmbedLayer trait + SqliteVecEmbed impl (or FastembedEmbed)
    ops/
      seed.rs        // merge_picks, pick_parts, resolve_picks, base_title,
                     // BranchSpec, open_branch, Seeder
      compact.rs     // render_span, runs_of, summarize, compact
      explore.rs     // touched_paths, touched_dirs, explore_span (scout loop)
      fork.rs        // fork + excerpt_of
      extract.rs     // extract + inherit_pins (share the ONE copy)
      move_into.rs
      handoff.rs
      sections.rs    // parse_sections, normalize_sections, sectionize
      unsend.rs
```

- **Traits**: `Db` (already the subsystem-wide seam — port as one trait in a shared
  `bough-types` crate; every history fn takes `&dyn Db` or a generic), `Bus`
  (`publish(Event)`), `LlmClient` (async `run`/`complete_text` — the injected-in-tests
  seam every op uses), `EmbedLayer`, `CommandEcho`. The `explore` seam on
  CompactCtx/HandoffCtx becomes `Option<Box<dyn Fn(&[Message], &str) -> BoxFuture<Option<String>>>>`
  or a small `Scout` trait.
- **Clock**: `now: Option<Arc<dyn Fn() -> i64>>` (or a `Clock` trait) — the injected,
  never-advanced clock is what makes the ordering tests writable; keep it.
- **Async boundaries (tokio)**: cluster A is fully synchronous EXCEPT `EmbedLayer`
  (drain is CPU-bound inside SQLite — run it on `spawn_blocking`; keep batch=64 so one
  drain never starves the runtime; model download is async). Cluster B: `compact`,
  `handoff`, `explore_span`, `sectionize`, fork's abandoned-summary are async (LLM
  calls); `fork`, `extract`, `move_into`, `unsend`, all of `seed.rs` are sync. The
  recorder/echo run inline on the turn path — keep them sync and cheap (they stat a few
  paths and run one bounded query each).
- Error enums per op (`ForkError`, `CompactError`, …) carrying `(status: u16, message)`;
  one axum/hyper layer maps them.
- Every error-swallowing contract from §1 becomes an explicit `let _ = …;` /
  `match … { Err(_) => return None/0/() }` — never `?` up through a turn.

## 8. v1 scope cut

Needed for the core loop (agent runs commands, TUI works):
- `record.rs` + `hygiene.rs` in full — the write path is the invariant that has never
  slipped (100% coverage), and hosting it later means a memory hole. The `bash(cmd,
  tags)` REQUIRED-tags boundary lives in hostfn, not here, but depends on
  `normalize_tags`.
- `stats.rs` (note + hints) — small, pure, and the prompt wiring expects it.
- `seed.rs` + `fork.rs` + `unsend.rs` — the TUI's edit-any-turn and take-back keys.

Can be stubbed/dropped initially:
- **embed.rs → stub** returning `None` from `create_embed_layer`. The whole layer is
  graceful-absence by design; tags + FTS carry recall alone (that is the documented
  macOS-without-Homebrew steady state). `bough tags similar` exits 1 naming the FTS
  query. This dodges the riskiest dependency (sqlite-lembed has no Rust story).
- **echo.rs → later** (high value but additive: without it commands simply run; no
  caller breaks if `note`/`guard` return None). Port right after v1 — the guard is a
  real cost-saver.
- **explore.rs → stub** returning `None` — compaction/handoff explicitly summarize
  unenriched when the scout answers None; nothing else changes.
- **sections.rs → later** — a cosmetic labeling pass; TUI degrades to unlabeled history.
- **compact/extract/move/handoff → later** but keep the route stubs answering 501; the
  seeder they share ships in v1 with fork.
- Cheap-tier retitle, `summarizeAbandoned`, `bough tags stats` day-diversity → later.
- `source: "backfill"` writes — nothing in-tree writes them today; keep the column,
  drop the writer.
