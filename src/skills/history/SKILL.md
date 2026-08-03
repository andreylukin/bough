---
name: history
description: Search and read bough's own conversation and command history by querying its SQLite database directly
---

# bough's own history

bough keeps everything in one SQLite database — `~/.bough/bough.db`, or
`$BOUGH_DB` / `$BOUGH_HOME/bough.db` when those are set. Transcripts, costs, the
session tree and the command memory are all in that one file.

**Query it through `bough tags`, never by opening the file yourself.** Not with
`sqlite3`, not with a library, not read-only-by-convention:

```bash
bough tags sql "SELECT id, title FROM sessions ORDER BY created_at DESC LIMIT 5"
bough tags sql --json "SELECT tag, count(*) n FROM command_tags GROUP BY tag"
```

The command opens the file `{readonly: true}` with `PRAGMA query_only = ON` and
refuses anything that is not `SELECT` or `WITH`, so the guarantee holds against a
database the server is writing to right now — where a hand-rolled connection is
one forgotten flag away from a write lock, or a write, on live data. Rows come
back as JSON, capped at 200.

The curated views are worth reaching for first, because they already know the
joins: `bough tags` (this project's vocabulary), `bough tags show TAG`
(what worked, exit code first, `--program` for the round), `bough tags stats`
(coverage and vocabulary per day). `bough tags similar "text"` is semantic
recall where the local vector layer exists — it lives in a SEPARATE file
(`~/.bough/embeddings.db`) that needs the vec0 extension, which is exactly why
it is a command rather than a query you could write here.

Timestamps are **epoch milliseconds** everywhere; `datetime(created_at/1000,
'unixepoch','localtime')` renders one. Booleans are `0`/`1`.

## The tables you need

`sessions` — one conversation.
`id`, `parent_id`, `title`, `kind`, `created_at`, `workspace`, `origin_dir`,
`base`, `origin_id`, `origin_message_id`, `model`, `effort`, `cost_usd`,
`input_tokens` / `output_tokens` / `reasoning_tokens`, `outcome_ok`.

- `kind` is `root` | `fork` | `compaction` | `subagent` | `workflow_agent` |
  `schedule_run` (one firing of a schedule, hung off the conversation that created
  it) | `shell` (the per-workspace conversation `!` commands run in).
- `parent_id` is **thread inheritance**: a session's thread is its ancestors'
  messages followed by its own. A subagent has `parent_id` NULL — it gets a
  fresh, task-only thread.
- `origin_id` is the **lineage edge** for the tree view: what this branched from.
  Subagents, workflow agents and schedule runs collapse under their `origin_id`;
  roots, forks and shells are top-level. Visibility is derived from `kind` + `origin_id` alone — no column
  hides, deprecates or purges a session, because no such operation exists.

`messages` — `id`, `session_id`, `role` (`user` | `supervisor` | `system`),
`parts` (JSON), `pending`, `created_at`. **Order by `(created_at, rowid)`, never
`created_at` alone** — seeded branches and a following turn can share a
millisecond, and `rowid` is what keeps them in order.

`parts` is a JSON array of objects discriminated on `type`: `text`, `reasoning`,
`tool_call` (`{id, name, input}` — `name` is `run_steps`, `input.code` is the
program), `tool_result` (`{callId, output, isError, interrupted?}`), `image`
(a path under `~/.bough/attachments/`, never bytes), `ask` (a settled question).
SQLite's JSON functions read it directly — see the recipes below.

`turns` — one row per turn: `session_id`, `message_id`, `status` (`running` |
`done` | `error` | `interrupted` | `orphaned`), `step`, `error`, per-turn token
counts and `cost_usd`.

`messages_fts` — the FTS5 index: `text`, plus `message_id` and `session_id`
(UNINDEXED, carried for the join). It holds only the **text and reasoning** parts
of each message, joined by newlines — prose, not tool output. A message with no
prose has no row.

For the tag memory specifically there is a command that already knows these joins:
`bough tags` (the project's vocabulary), `bough tags show TAG`, `bough tags stats`,
each with `--json`. Reach for it before hand-writing the SQL below.

`command_history` — the tag memory: one row per finished shell command.
`session_id`, `ts`, `repo` (git origin URL, else a path — the scope key),
`cmd`, `tags` (colon-joined intent tags the model wrote), `exit_code`,
`duration_ms`, `output_head` (first ~2k chars it printed, spill marker
included), `spill_path` (the scratch file holding an oversized output in full —
may have been cleaned since), `message_id` (the supervisor message whose
`run_steps` program ran it — join to `messages` and read `parts` for the code;
NULL on rows written before the column). A tag containing a DOT is a reference
to something outside bough (`linear.eng-1234`, `pr.456`); it joins like any
other tag and is excluded from the popularity ranking. Junctions: `command_tags(command_id, tag)` and
`command_dirs(command_id, rel_dir)` (repo-root-relative dirs the command was
about). `command_history_fts` indexes `cmd`, `tags` AND `output_head`, so a
MATCH finds results as well as invocations.

Also present: `workflows` and `workflow_agents` (the fan-out journal: `key`,
`label`, `phase`, `status`, `result`), `session_state` (durable KV keyed by
`(root_id, key)`), `schedules`.

## What worked here before

```sql
SELECT h.cmd, h.exit_code, h.output_head,
       datetime(h.ts/1000,'unixepoch','localtime') AS ran
  FROM command_history h JOIN command_tags t ON t.command_id = h.id
 WHERE t.tag = 'deploy' AND h.exit_code = 0
 ORDER BY h.ts DESC LIMIT 10;
```

By what a command PRINTED, not what it was called:

```sql
SELECT h.cmd, h.spill_path
  FROM command_history_fts f JOIN command_history h ON h.id = f.command_id
 WHERE command_history_fts MATCH 'output_head:timeout'
 ORDER BY h.ts DESC LIMIT 10;
```

Tag popularity per repo (what the priming row is built from):

```sql
SELECT h.repo, t.tag, count(*) AS uses
  FROM command_history h JOIN command_tags t ON t.command_id = h.id
 GROUP BY h.repo, t.tag ORDER BY uses DESC LIMIT 20;
```

## Find past work

FTS5 syntax: bare words are ANDed, `"a phrase"` requires adjacency, `OR` and
`NOT` are operators, and `*` suffixes a prefix. `"`, `*`, `^`, `:` and `NEAR` are
operators — quote them to search for them literally.

```sql
SELECT s.title,
       datetime(m.created_at/1000,'unixepoch','localtime') AS when_,
       m.session_id,
       snippet(messages_fts, 0, '[', ']', '…', 16) AS hit
  FROM messages_fts
  JOIN messages m ON m.id = messages_fts.message_id
  JOIN sessions s ON s.id = m.session_id
 WHERE messages_fts MATCH 'websocket reconnect'
 ORDER BY rank, m.created_at DESC
 LIMIT 20;
```

Prose only. For an exact identifier that may live in a tool call or its output,
scan the JSON instead — slower, but it sees everything:

```sql
SELECT session_id, id, created_at
  FROM messages
 WHERE parts LIKE '%resolveInWorkspace%'
 ORDER BY created_at DESC LIMIT 20;
```

The running server exposes the same index over HTTP if you would rather not open
the file: `GET http://127.0.0.1:${BOUGH_PORT:-4321}/search?q=…&sessionId=…`.

## Read a transcript

```sql
SELECT role,
       datetime(created_at/1000,'unixepoch','localtime') AS when_,
       (SELECT group_concat(json_extract(p.value,'$.text'), '
')
          FROM json_each(messages.parts) p
         WHERE json_extract(p.value,'$.type') = 'text') AS said
  FROM messages
 WHERE session_id = '<id>'
 ORDER BY created_at, rowid;
```

The programs a session ran:

```sql
SELECT json_extract(p.value,'$.input.code') AS code
  FROM messages, json_each(messages.parts) p
 WHERE session_id = '<id>'
   AND json_extract(p.value,'$.type') = 'tool_call'
 ORDER BY created_at, rowid;
```

## Walk the tree

Recent top-level work (subagents and workflow agents collapse under their origin,
so exclude them here and drill in on demand):

```sql
SELECT id, kind, title, workspace,
       datetime(created_at/1000,'unixepoch','localtime') AS started
  FROM sessions
 WHERE kind IN ('root','fork','compaction')
 ORDER BY created_at DESC LIMIT 30;
```

Its branches — forks, compactions, subagents, workflow agents:

```sql
SELECT id, kind, title, outcome_ok FROM sessions WHERE origin_id = '<id>';
```

A session's full replayable thread is its ancestors by `parent_id`, root first,
then its own messages:

```sql
WITH RECURSIVE chain(id, parent_id, depth) AS (
  SELECT id, parent_id, 0 FROM sessions WHERE id = '<id>'
  UNION ALL
  SELECT s.id, s.parent_id, chain.depth + 1
    FROM sessions s JOIN chain ON s.id = chain.parent_id
)
SELECT m.role, m.created_at, m.parts
  FROM chain JOIN messages m ON m.session_id = chain.id
 ORDER BY chain.depth DESC, m.created_at, m.rowid;
```

## What went wrong, and what it cost

```sql
SELECT t.status, t.step, t.error, t.cost_usd, s.title
  FROM turns t JOIN sessions s ON s.id = t.session_id
 WHERE t.status IN ('error','interrupted','orphaned')
 ORDER BY t.updated_at DESC LIMIT 20;

SELECT date(created_at/1000,'unixepoch','localtime') AS day,
       round(sum(cost_usd),2) AS usd, count(*) AS sessions
  FROM sessions GROUP BY day ORDER BY day DESC LIMIT 14;
```

## Rules of engagement

- **Read-only, and enforced.** `bough tags sql` refuses anything that is not
  SELECT or WITH and cannot write even if it wanted to — so this is a guarantee
  now rather than a rule you have to keep. Use the HTTP API for anything that
  changes state. The SQL blocks below are query bodies: pass one as the quoted
  argument.
- A row can be mid-turn: `messages.pending = 1` and `turns.status = 'running'`
  mean the supervisor is still streaming. `orphaned` means a server restart ended
  it.
- Report findings as sessions the user can open — quote the title and the id, and
  say when it happened. A wall of raw JSON `parts` is not an answer; extract the
  text you actually used.
