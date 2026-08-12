-- The complete schema, for every milestone.
--
-- Invariant this file holds: **the table set is closed.** Every column any task
-- through M10 needs is created here, in one statement block, applied once at open.
-- There is no migration ladder and no `ALTER TABLE ... ADD COLUMN` swallowing a
-- duplicate error: the old tree accumulated one because columns arrived task by
-- task, and the result was a schema you could only learn by reading the migration
-- history. A later task that needs a column stops and asks (plan §4).
--
-- Second invariant: message ordering is `(created_at, rowid)` everywhere, never
-- `created_at` alone. Branch seeding writes with a real clock rather than an
-- advanced artificial one, so a turn started immediately afterwards lands in the
-- same millisecond — `rowid` is what keeps it after the seed (plan §6.1). The index
-- below is declared in that order for the same reason.
--
-- Conventions: timestamps are epoch ms INTEGER, booleans are 0/1 INTEGER, and
-- anything structured is a JSON TEXT column. Foreign keys are declared and
-- `PRAGMA foreign_keys = ON` is set at open.
--
-- What is deliberately absent:
--   * `archived_at` / `deprecated_at` on sessions — visibility is DERIVED from
--     `kind` + `origin_id`; there is no archive, deprecate, hide or purge action
--     (spec §4, §17).
--   * `message_embeddings` — cross-session MESSAGE search is keyword FTS, not
--     vectors. (The command-history tables below are a separate, additive memory
--     with their own FTS index; a vector index over THEM may exist as an optional
--     runtime layer, but never over messages.)
--   * a jobs table — a background shell dies with the server, so a persisted row
--     would always be a lie after a restart. Jobs are in-memory (spec §9).
--   * artifacts / skills tables — both are filesystem-backed, and the directory is
--     the source of truth. Both survive a database reset (spec §4).

-- One conversation. Sessions form a forest by `parent_id`, and a session's thread
-- is its ancestors' messages ++ its own — which is what makes fork and compaction
-- cheap: a branch parented at the target's parent inherits shared ancestors for
-- free and only seeds the rest (spec §14).
CREATE TABLE IF NOT EXISTS sessions (
  id                TEXT PRIMARY KEY,
  -- Thread inheritance. NULL for a root, and NULL for a subagent too: a subagent
  -- gets a fresh, task-only thread with no inherited context (spec §7).
  parent_id         TEXT REFERENCES sessions(id),
  title             TEXT NOT NULL,
  -- root | fork | compaction | subagent | workflow_agent | schedule_run | shell.
  -- `subagent`, `workflow_agent` and `schedule_run` collapse under `origin_id` in
  -- listings and surface only on drill-in (`schema/parts.ts` owns that list). This
  -- column IS the visibility rule. `shell` is the per-workspace conversation a `!`
  -- command runs in when none is open — listed like any root, and reused rather
  -- than re-created, so the habit costs the switcher one row and not one a launch.
  kind              TEXT NOT NULL,
  created_at        INTEGER NOT NULL,
  -- The checkout the session operates on, edited in place. NULL = the process
  -- default. There is no per-agent worktree: subagents share this directory.
  workspace         TEXT,
  -- The project directory at creation. Mirrors `workspace` and is never
  -- rewritten, so it stays the stable record of WHICH project this is for.
  origin_dir        TEXT,
  -- The git sha the session started from. `git diff <base>` plus untracked files
  -- is the change set. NULL for a non-git workspace, which therefore has no
  -- change set and no revert (spec §13).
  base              TEXT,
  -- Lineage edge for the tree view: what this branched from, at which message.
  origin_id         TEXT,
  origin_message_id TEXT,
  -- Per-session pins; NULL = the global default (spec §12).
  model             TEXT,
  effort            TEXT,
  -- Prefilled composer text set by handoff; cleared by the first posted message.
  draft             TEXT,
  -- Status-bar display. `context_tokens` is the LAST round's prompt size and
  -- `cached_tokens` the share of it served from / written to the provider cache;
  -- `last_llm_at` is when that round finished, so the client can derive cache
  -- warmth (a time-decaying property, never a stored boolean).
  context_tokens    INTEGER,
  cached_tokens     INTEGER,
  last_llm_at       INTEGER,
  -- Cumulative across the session, for the cost meter. Reasoning and cache
  -- totals are tracked apart from input/output (spec §5 Usage).
  input_tokens      INTEGER,
  output_tokens     INTEGER,
  reasoning_tokens  INTEGER,
  cache_read_total  INTEGER,
  cache_write_total INTEGER,
  cost_usd          REAL,
  -- Delegation outcome, stamped when a subagent's turn finishes, so the tree can
  -- render a failed branch. Records whether the TURN errored — there is no
  -- acceptance gate and nothing here reflects one (spec §17).
  outcome_ok        INTEGER
);
CREATE INDEX IF NOT EXISTS sessions_parent ON sessions(parent_id);
CREATE INDEX IF NOT EXISTS sessions_origin ON sessions(origin_id);

-- One message. `parts` is the JSON Part[] from schema/parts.ts; image bytes are
-- NOT in it — an image part stores a path under ~/.bough/attachments/, so rows
-- stay small and replay survives a moved file (spec §4).
CREATE TABLE IF NOT EXISTS messages (
  id         TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id),
  -- user | supervisor | system. `system` messages are harness-injected notes
  -- (a detached subagent's report, a job exit, artifact comments); they render
  -- distinctly and replay to the model as user-side text.
  role       TEXT NOT NULL,
  parts      TEXT NOT NULL,
  -- 0/1. Created pending while the supervisor streams; flipped when finished.
  pending    INTEGER NOT NULL,
  created_at INTEGER NOT NULL
);
-- Ordering index. Reads sort by (created_at, rowid) — see the header note.
CREATE INDEX IF NOT EXISTS messages_session ON messages(session_id, created_at);

-- The turn state machine: one row per turn, checkpointed as it progresses. On
-- boot, any row still `running` becomes `orphaned` and its session unblocks —
-- that is the whole reason `step` is persisted rather than held in memory
-- (spec §4, plan T2.3).
CREATE TABLE IF NOT EXISTS turns (
  id               TEXT PRIMARY KEY,
  session_id       TEXT NOT NULL REFERENCES sessions(id),
  -- The pending supervisor message this turn produces.
  message_id       TEXT NOT NULL REFERENCES messages(id),
  -- running | done | error | interrupted | orphaned.
  status           TEXT NOT NULL,
  -- Last checkpoint, human-readable; written after each API round and each tool
  -- result so recovery can say how far the turn got.
  step             TEXT NOT NULL,
  created_at       INTEGER NOT NULL,
  updated_at       INTEGER NOT NULL,
  -- Present when status is `error`; names the limit or the failure (spec §5).
  error            TEXT,
  -- Per-turn usage from the provider, aggregated up to the session.
  input_tokens     INTEGER,
  output_tokens    INTEGER,
  reasoning_tokens INTEGER,
  cache_read_tokens  INTEGER,
  cache_write_tokens INTEGER,
  cost_usd         REAL
);
CREATE INDEX IF NOT EXISTS turns_status ON turns(status);
CREATE INDEX IF NOT EXISTS turns_session ON turns(session_id, updated_at);

-- One detached orchestration run (spec §8). The script text is persisted verbatim
-- — and mirrored to ~/.bough/workflows/<id>.js for out-of-band editing — so a
-- rerun can diff the edited source against what actually ran.
CREATE TABLE IF NOT EXISTS workflows (
  id            TEXT PRIMARY KEY,
  session_id    TEXT NOT NULL REFERENCES sessions(id),
  -- From the script's `meta`, which must be a pure literal and is extracted
  -- host-side by a balanced-brace scan BEFORE the body runs.
  name          TEXT NOT NULL,
  description   TEXT NOT NULL,
  script        TEXT NOT NULL,
  -- JSON [{title, detail?}] from meta.phases.
  phases        TEXT NOT NULL,
  -- running | paused | done | error | stopped | orphaned.
  status        TEXT NOT NULL,
  current_phase TEXT,
  -- JSON: the script's return value (status `done`).
  result        TEXT,
  error         TEXT,
  -- JSON: the run's input, handed to the script as `args` verbatim.
  args          TEXT,
  -- The run this rerun replays its journal from.
  resume_of     TEXT REFERENCES workflows(id),
  created_at    INTEGER NOT NULL,
  finished_at   INTEGER
);
CREATE INDEX IF NOT EXISTS workflows_session ON workflows(session_id, created_at);

-- The journal: one row per `agent()` call. `key` is hash(prompt + opts), and a
-- rerun replays every hit instantly and re-runs only calls whose key changed.
-- That is why workflow scripts are forbidden `Date.now()` and `Math.random()` —
-- without determinism, "replays unchanged calls" is a lie the first time a script
-- stamps a timestamp into a prompt (plan §6.15).
CREATE TABLE IF NOT EXISTS workflow_agents (
  id          TEXT PRIMARY KEY,
  run_id      TEXT NOT NULL REFERENCES workflows(id),
  -- Call order within the run.
  idx         INTEGER NOT NULL,
  key         TEXT NOT NULL,
  label       TEXT NOT NULL,
  phase       TEXT,
  prompt      TEXT NOT NULL,
  model       TEXT,
  -- JSON Schema when the call was made with {schema}; NULL otherwise. Part of
  -- what `key` hashes, so editing a schema re-runs exactly the calls using it.
  schema      TEXT,
  -- queued | running | done | error | stopped | cached.
  status      TEXT NOT NULL,
  -- The agent's report text, or the JSON of a {schema} call.
  result      TEXT,
  error       TEXT,
  -- The subagent session backing this call (TUI drill-in). NULL for cached replays.
  session_id  TEXT REFERENCES sessions(id),
  started_at  INTEGER NOT NULL,
  finished_at INTEGER
);
CREATE INDEX IF NOT EXISTS workflow_agents_run ON workflow_agents(run_id, idx);
-- Journal lookup on rerun: find the source run's row for a given call key.
CREATE INDEX IF NOT EXISTS workflow_agents_key ON workflow_agents(run_id, key);

-- Durable KV the program writes for itself (spec §6). Scoped to the LINEAGE ROOT,
-- not the session, so forks, compactions and subagents of one piece of work share
-- one store — the point is surviving a context the turn loop will eventually
-- compact or truncate away. Notes, not storage: 16KB per key, payloads go in files.
CREATE TABLE IF NOT EXISTS session_state (
  root_id    TEXT NOT NULL,
  key        TEXT NOT NULL,
  -- JSON: whatever the program stored.
  value      TEXT NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (root_id, key)
);

-- Recurring runs (spec §9). A ~30s ticker fires each enabled schedule whose
-- `next_run_at` has passed, by opening a fresh ROOT session and running `prompt`
-- there. `next_run_at` advances FROM NOW at fire time, never from the stale stored
-- value, so a server down through N missed slots fires once and then resumes
-- cadence rather than bursting N make-up runs (plan §6.8).
CREATE TABLE IF NOT EXISTS schedules (
  id          TEXT PRIMARY KEY,
  title       TEXT NOT NULL,
  prompt      TEXT NOT NULL,
  -- NULL = no workspace; the session runs chat-only.
  workspace   TEXT,
  -- "every:<N><m|h|d>" (N >= 1) or "daily@HH:MM" (local wall clock). Stored
  -- verbatim; the schedules module owns parsing and `nextRun(spec, now)`.
  spec        TEXT NOT NULL,
  enabled     INTEGER NOT NULL,
  created_at  INTEGER NOT NULL,
  last_run_at INTEGER,
  next_run_at INTEGER NOT NULL,
  -- The conversation that created it: each firing's outcome is posted back there
  -- as a system note (schedules.ts). NULL = created outside any conversation.
  -- Deliberately no FK: the creator may be deleted, and the note then simply
  -- drops rather than the schedule refusing to exist. LAST because ALTER TABLE
  -- appends — a migrated file and a fresh one must agree on column order.
  session_id  TEXT
);
CREATE INDEX IF NOT EXISTS schedules_due ON schedules(enabled, next_run_at);

-- Command-history memory: one row per finished shell command, tagged by the model
-- at write time (`bash(cmd, tags)`). The tags are the retrieval key — the command
-- string alone is too varied to match across sessions — and the exit code is the
-- ground truth that weights them (a tag on ten failing attempts must not become
-- "popular"). Scoped by `repo` (git origin URL, else the workspace root path) so
-- profiles survive a moved or re-cloned checkout.
--
-- INTEGER PRIMARY KEY rather than the TEXT ids used elsewhere: these rows are a
-- high-volume append-only log joined through two junction tables, and the rowid
-- alias is the natural join key. Nothing outside this table group references it.
CREATE TABLE IF NOT EXISTS command_history (
  id          INTEGER PRIMARY KEY,
  session_id  TEXT NOT NULL REFERENCES sessions(id),
  ts          INTEGER NOT NULL,
  -- git remote origin URL when the workspace has one, else the workspace root
  -- path. The scope key for every popularity/profile query.
  repo        TEXT NOT NULL,
  cmd         TEXT NOT NULL,
  -- The normalized colon-separated string as recorded ('' for verbs that carry
  -- no tags, e.g. `sh` legs). Split into command_tags for querying.
  tags        TEXT NOT NULL,
  -- NULL = unknown (still running when the turn moved on).
  exit_code   INTEGER,
  duration_ms INTEGER,
  -- The first ~2k chars of what the command PRINTED (as the program saw it,
  -- spill marker included). Recall over results, not just invocations: "what
  -- did the migration say" is answerable without re-running it. '' = silent.
  output_head TEXT NOT NULL DEFAULT '',
  -- Where the full output was spilled when it was over the bound
  -- (hostfn/spill.ts). NULL = it fit. The file may since have been cleaned;
  -- the path is a pointer, not a guarantee.
  spill_path  TEXT,
  -- live | backfill. Backfilled labels are model-inferred after the fact and
  -- weigh less than generation-time intent.
  source      TEXT NOT NULL DEFAULT 'live',
  -- The supervisor message whose `run_steps` program ran this command, so a
  -- recalled command reaches the PROGRAM around it: `messages.parts` holds the
  -- tool_call whose `input.code` is what the round actually did. Recall answers
  -- "here is the incantation" without it and "here is the shape of the round that
  -- used it" with it, which is the more useful half on anything but a one-liner.
  --
  -- NULL for rows written before the column existed, and for any writer that has
  -- no message (a backfill, a test). Deliberately NOT a foreign key: the memory
  -- outlives its transcript — a compaction or a fresh root can leave a command
  -- whose message is gone, and losing the row would be worse than losing the link.
  message_id  TEXT
);
CREATE INDEX IF NOT EXISTS command_history_repo ON command_history(repo, ts);

-- One row per tag per command. `command_history.tags` split at record time.
CREATE TABLE IF NOT EXISTS command_tags (
  command_id INTEGER NOT NULL REFERENCES command_history(id),
  tag        TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS command_tags_tag ON command_tags(tag, command_id);
CREATE INDEX IF NOT EXISTS command_tags_command ON command_tags(command_id);

-- Directories a command was ABOUT, extracted from path-looking tokens that
-- resolve inside the workspace (`history/record.ts`). Not the cwd: a bough
-- program runs at the workspace root, so cwd carries no signal — `bun test
-- src/tui/x.test.ts` is attributed to `src/tui`, which is what makes
-- per-directory tag profiles possible at all. Workspace-relative.
CREATE TABLE IF NOT EXISTS command_dirs (
  command_id INTEGER NOT NULL REFERENCES command_history(id),
  rel_dir    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS command_dirs_dir ON command_dirs(rel_dir, command_id);
CREATE INDEX IF NOT EXISTS command_dirs_command ON command_dirs(command_id);

-- Keyword search over recorded commands — invocation AND result — for
-- `history.sql()` recall. Same standalone shape and tokenizer as messages_fts.
CREATE VIRTUAL TABLE IF NOT EXISTS command_history_fts USING fts5(
  cmd,
  tags,
  output_head,
  command_id UNINDEXED,
  tokenize = 'unicode61 remove_diacritics 2'
);

-- Keyword search over transcripts (spec §17: FTS, no embeddings, no vector index).
-- Standalone rather than an external-content table: message `parts` is JSON, so
-- the indexed text is a projection of it (the text/reasoning parts), not a column
-- FTS could mirror. `message_id` / `session_id` are UNINDEXED — carried for the
-- join, not matched against. Rebuilding from scratch must produce identical
-- results to incremental indexing (plan T8.9).
CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
  text,
  message_id UNINDEXED,
  session_id UNINDEXED,
  tokenize = 'unicode61 remove_diacritics 2'
);

-- ---------------------------------------------------------------------------
-- The note memory (docs/notes.md). SANCTIONED ADDITION to a closed table set:
-- notes began as markdown files and moved here so a note could attach to a SET
-- of tags rather than be NAMED by one — which files cannot express without a
-- composite filename, and a composite filename is a key no command can carry.
-- ---------------------------------------------------------------------------

-- One note. `path` is PLACEMENT: a colon path in the tag grammar's own order
-- (`atlas`, `kubectl:rollout`, `kubectl:rollout:atlas`), so depth 1 is a
-- top-level note about a word and deeper paths are notes about a combination.
-- Intermediate nodes with no note are STUBS, computed when a tree is rendered
-- and never stored — an empty row would be a note that says nothing.
--
-- Placement is NOT attachment: what a note covers is `note_tags`, because the
-- grammar is faceted, not a containment tree. `atlas` appears under
-- `kubectl:rollout:atlas` and `helm:upgrade:atlas` both, so prefix matching
-- would miss the half that carries the meaning.
CREATE TABLE IF NOT EXISTS notes (
  id         INTEGER PRIMARY KEY,
  path       TEXT NOT NULL UNIQUE,
  title      TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  -- The newest `command_history.ts` folded into the log. A plain column, not
  -- the per-host map the file store needed: the database IS per machine, so
  -- "this host's frontier" and "this database's frontier" are the same number.
  -- Advanced only to a row actually folded, never to `now`, so a fold that
  -- skipped rows cannot mark them accounted for.
  synced_ts  INTEGER NOT NULL DEFAULT 0,
  -- Set when a reference stops seeing commands. Frozen, never deleted: bough
  -- has no purge, and a closed ticket's conclusions outlive the ticket.
  closed_at  INTEGER
);
CREATE INDEX IF NOT EXISTS notes_path ON notes(path);

-- ATTACHMENT: which commands a note covers. Many-to-many, order-free, and the
-- reason a "tag group" costs nothing — a note on {kubectl, atlas} matches every
-- command carrying both, at any position. `tag` joins `command_tags.tag`, so a
-- key that is not a legal tag is unreachable by construction (`canonical_key`).
CREATE TABLE IF NOT EXISTS note_tags (
  note_id INTEGER NOT NULL REFERENCES notes(id),
  tag     TEXT NOT NULL,
  PRIMARY KEY (note_id, tag)
);
CREATE INDEX IF NOT EXISTS note_tags_tag ON note_tags(tag, note_id);

-- A note's body is its ordered sections. The section is the unit that is
-- addressed, tagged, revised, cited and transcluded: a lesson learned while
-- working on `atlas:rollout:prod` is often a truth about `atlas`, and with the
-- note as the atom it would be stuck where it was written.
CREATE TABLE IF NOT EXISTS note_sections (
  id         INTEGER PRIMARY KEY,
  note_id    INTEGER NOT NULL REFERENCES notes(id),
  ord        INTEGER NOT NULL,
  heading    TEXT NOT NULL,
  body       TEXT NOT NULL,
  -- human | session | cheap. WHO WROTE THIS CLAIM, which no staleness query
  -- can recover: a line the cheap model inferred and a sentence you typed
  -- arrive as the same confident prose unless the row says otherwise.
  author     TEXT NOT NULL DEFAULT 'human',
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS note_sections_note ON note_sections(note_id, ord);

-- Where a section surfaces. Defaults to its note's tags on write, so authoring
-- is unchanged and a section appears only where it was written. NARROWING the
-- set is PROMOTION — the deliberate act that says "this is a general truth" —
-- and a section surfaces wherever its tags are a SUBSET of the reader's
-- context. Subset, not overlap: overlap would put every `git`-tagged section on
-- every page.
--
-- There is no cap on promotion and no policy against it. Resolution ranks by
-- the same `weight x idf` the priming note uses, so a section promoted to a tag
-- every repo uses scores at the floor and never wins a slot. The incentive
-- disappears because the payoff does.
CREATE TABLE IF NOT EXISTS section_tags (
  section_id INTEGER NOT NULL REFERENCES note_sections(id),
  tag        TEXT NOT NULL,
  PRIMARY KEY (section_id, tag)
);
CREATE INDEX IF NOT EXISTS section_tags_tag ON section_tags(tag, section_id);

-- WHAT A CLAIM RESTS ON. Rows rather than markdown, so a citation can be
-- VALIDATED — an id that does not exist, or that carries none of the section's
-- tags, refuses the write instead of rotting silently in prose.
--
-- This is what lets the cheap tier write at all without being taken on faith:
-- the fold cites the very commands of the round it is summarizing, it is never
-- shown any other id so it cannot invent one, and a line with no citation is by
-- construction a human's unsourced claim.
--
-- `kind`: command (command_history.id) | message (messages.id) | file
-- (path@sha) | url | section (note_sections.id). Deliberately no foreign key —
-- the memory outlives its transcript, and losing the note would be worse than
-- losing the link.
CREATE TABLE IF NOT EXISTS section_citations (
  section_id INTEGER NOT NULL REFERENCES note_sections(id),
  kind       TEXT NOT NULL,
  ref        TEXT NOT NULL,
  at         INTEGER NOT NULL,
  PRIMARY KEY (section_id, kind, ref)
);
CREATE INDEX IF NOT EXISTS section_citations_ref ON section_citations(kind, ref);

-- HISTORICAL NOTES: one row per superseded section body, full copies (a section
-- is small, so the column-mask tricks a large-blob history needs do not apply).
-- Never pruned.
--
-- This is what makes resolving a contradiction auditable. Without it, a warning
-- cleared by a rewrite loses the claim that was there and records that no
-- judgment was made — the exact silent loss that makes model-arbitrated memory
-- untrustworthy.
CREATE TABLE IF NOT EXISTS section_revisions (
  section_id INTEGER NOT NULL REFERENCES note_sections(id),
  rev        INTEGER NOT NULL,
  heading    TEXT NOT NULL,
  body       TEXT NOT NULL,
  -- human | session | cheap — who wrote the version being superseded.
  author     TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (section_id, rev)
);

-- The derived zone, as rows. Provenance is a column rather than a glyph to
-- parse, and the frontier is a MAX(ts) rather than a frontmatter map.
-- Append-only: the machine writes here and nowhere else.
CREATE TABLE IF NOT EXISTS note_log (
  id      INTEGER PRIMARY KEY,
  note_id INTEGER NOT NULL REFERENCES notes(id),
  ts      INTEGER NOT NULL,
  -- human | session | cheap.
  source  TEXT NOT NULL,
  text    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS note_log_note ON note_log(note_id, ts);

-- Keyword search over notes, at SECTION grain so a hit points at the paragraph
-- rather than the page. Same standalone shape and tokenizer as the other two.
CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
  heading,
  body,
  path,
  section_id UNINDEXED,
  note_id UNINDEXED,
  tokenize = 'unicode61 remove_diacritics 2'
);
