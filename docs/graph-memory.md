# Graph memory

Long-term memory for bough as a first-class graph core. Replaces the graphiti
plugin (Graphiti MCP server + Neo4j/FalkorDB + gpt-5-mini extraction) with a
SQLite-native layer that connects work across Linear, Slack, GitHub, and Notion.

## Why not Graphiti

Graphiti earns its keep when entities must be LLM-extracted from unstructured
chat. Our data is not unstructured: tickets, PRs, branches, emails, and note
slugs are deterministic keys. Everything else Graphiti provides we either
already have or is a schema property, not a dependency:

- Storage: Graphiti needs Neo4j or FalkorDB; bough already runs on SQLite.
- Hybrid retrieval (vector + BM25 + traversal): we have sqlite-vec and FTS5;
  traversal is a recursive CTE.
- Episodes and provenance: `section_citations` and the `author` column already
  do this, more carefully than Graphiti does.
- Extraction: the gpt-5-mini extraction was the part producing mush.
  Deterministic linking replaces it for ~90% of edges.

The engine landscape also collapsed: Kuzu is archived (team acqui-hired by
Apple), CozoDB is abandoned, DuckPGQ is a research extension pinned to old
DuckDB, Neo4j embedded is JVM-only. SQLite with edge tables is the survivor's
choice and it is already our stack.

## Invariants

Four properties carried over from Graphiti's design, kept as bough invariants
rather than a plugin:

1. **Bi-temporal edges.** `valid_from`/`valid_to` record when a fact was true
   in the world; `observed_at`/`recorded_at` record when bough learned it.
   Point-in-time query = filter edges whose validity window contains the date.
2. **Invalidate, never delete.** Contradiction closes a validity window. The
   notes schema already committed to this ("frozen, never deleted").
3. **Every claim has provenance.** An edge points at an episode (a session, a
   command, a PR fetch, a Slack thread). No orphan facts.
4. **No LLM at retrieval.** Query time is vec + FTS + CTE hop expansion only.
   LLM calls happen at write time, and only for concept-level claims.

## Schema

Four tables. Either a new `graph.db` or folded into `bough.db`; same file
conventions as the rest of `~/.bough`.

```sql
CREATE TABLE entities (
  id         INTEGER PRIMARY KEY,
  kind       TEXT NOT NULL,      -- ticket | pr | person | repo | slack_thread
                                 -- | notion_page | concept | session | command
  key        TEXT NOT NULL,      -- canonical key within kind (see Keys below)
  title      TEXT NOT NULL DEFAULT '',
  attrs      TEXT NOT NULL DEFAULT '{}',  -- JSON, source-specific fields
  UNIQUE (kind, key)
);

CREATE TABLE aliases (
  entity_id  INTEGER NOT NULL REFERENCES entities(id),
  source     TEXT NOT NULL,      -- linear | slack | github | notion | bough
  foreign_id TEXT NOT NULL,      -- the source's own id
  url        TEXT,               -- canonical link when the source has one
  PRIMARY KEY (source, foreign_id)
);

CREATE TABLE episodes (
  id          INTEGER PRIMARY KEY,
  source      TEXT NOT NULL,     -- collector name, session fold, human edit
  ref         TEXT NOT NULL,     -- what was ingested (session id, PR url, ...)
  ingested_at INTEGER NOT NULL
);

CREATE TABLE edges (
  id          INTEGER PRIMARY KEY,
  src         INTEGER NOT NULL REFERENCES entities(id),
  rel         TEXT NOT NULL,
  dst         INTEGER NOT NULL REFERENCES entities(id),
  -- bi-temporal: when the fact held in the world ...
  valid_from  INTEGER NOT NULL,
  valid_to    INTEGER,           -- NULL = still true
  -- ... and when bough learned it
  observed_at INTEGER NOT NULL,  -- when the source stated it
  recorded_at INTEGER NOT NULL,  -- ingestion time
  episode_id  INTEGER NOT NULL REFERENCES episodes(id),
  -- human | session | cheap | collector. Same rule as note_sections.author:
  -- who wrote this claim, which no staleness query can recover.
  author      TEXT NOT NULL,
  weight      REAL NOT NULL DEFAULT 1.0
);
CREATE INDEX edges_src ON edges(src, rel) WHERE valid_to IS NULL;
CREATE INDEX edges_dst ON edges(dst, rel) WHERE valid_to IS NULL;
```

Embeddings and FTS follow the existing pattern: a `vec0` index in
`embeddings.db` over entity titles and claim text, an FTS5 table beside the
graph tables. Nothing new to build; the notes index is the template.

### Keys

Canonical keys are deterministic, one per kind:

- `ticket`: the Linear identifier (`NME-1673`). This is the hub key: branch
  names embed it (the branch convention mandates Linear branch names), PR
  bodies reference it, Slack unfurls carry the URL.
- `pr`: `repo#number` (`uni-nas-event-log#50`).
- `person`: email. Slack user IDs, GitHub logins, Linear user IDs, and Notion
  user IDs are aliases rows, not separate entities.
- `repo`: the origin URL normalized (already the scope key in
  `command_history.repo`).
- `slack_thread`: `channel:thread_ts`.
- `notion_page`: the page id.
- `concept`: the note slug (`nased`, `fmds`, `gitops:promotion`).
- `session`, `command`: existing bough ids.

### Relations

Initial vocabulary, from what the data actually contains:

| rel         | src → dst                          |
|-------------|------------------------------------|
| `implements`| pr → ticket                        |
| `reviews`   | person → pr                        |
| `authored`  | person → pr \| ticket \| notion_page |
| `discusses` | slack_thread → pr \| ticket \| concept |
| `documents` | notion_page → concept \| repo      |
| `touches`   | session → repo \| pr \| ticket     |
| `cites`     | concept → command \| url           |
| `relates`   | concept → concept                  |

`cites` absorbs `section_citations`. New rels are added when a collector
produces them, not speculatively.

## Ingest

Sync-then-resolve. Collectors write raw records with cursor watermarks (the
`collect-{github,linear,slack}.db` watermark tables are already this pattern);
a batch linking pass extracts edges. Linking is deterministic string
extraction, not fuzzy matching:

- ticket IDs from branch names, PR titles/bodies, commit messages, Slack text
- PR URLs from Slack unfurls and Linear attachments
- emails from directory data to fold user IDs into person entities

Slack is a hybrid source: index selectively (threads that link to our tickets,
PRs, or channels we care about), fetch live for recency. Do not mirror
everything.

LLM extraction survives only for concept-level edges (`relates`, `documents`
judgments), runs in the fold (same seam as note folding), and writes
`author = 'cheap'` so its claims are never mistaken for observed facts.

## LLM interaction

Three layers, in order of value:

1. **Passive injection.** At session start, resolve the workspace to its
   entities (repo, branch → ticket, open PRs) and inject the 1-2 hop
   neighborhood as a system-prompt section. The graphiti plugin was already
   "prompt section only"; this keeps that shape. Memory arrives without being
   asked for.

2. **Narrow read API via codemode.** Not raw SQL. Verbs on `bough.graph`:

   - `search(query)`: vec + FTS + RRF merge, returns entities and claims with refs
   - `neighbors(key, hops = 1, rel?)`: CTE expansion, open validity windows only
   - `timeline(key)`: edge history including closed windows, for "what changed"
   - `resolve(ref)`: URL / ticket ID / branch → entity

   Small verbs compose in codemode (loop, filter, join in JS). A read-only
   `sql()` escape hatch may exist for the human, not for the prompt.

3. **Two write verbs.** `assert(src, rel, dst, evidence)` and
   `invalidate(edge, reason)`. Both stamp `author` and `episode_id`
   automatically. The model never writes rows directly, so provenance cannot
   be skipped. Everything else is written by collectors and folds.

The failure mode to avoid is the Graphiti one inverted: hand the model raw SQL
and it writes clever queries that pin the whole graph into context. Narrow
verbs plus passive injection keep interaction cheap and the graph
authoritative.

## Retrieval

The Graphiti recipe without the server: sqlite-vec KNN over entity and claim
embeddings, FTS5 BM25 over the same text, reciprocal-rank-fusion merge, then
1-2 hop expansion via recursive CTE filtered to open validity windows.
Retrieval never calls a model.

## Migration and teardown

1. Land the schema and the `graph` core (this doc's tables, the codemode
   verbs, the injection section).
2. Backfill from what exists: notes → concept entities, `section_citations` →
   `cites` edges, `command_history` → command/repo entities and `touches`
   edges, session log → session entities.
3. Point collectors at the linking pass; start with GitHub (richest
   deterministic signal), then Linear, then Slack, then Notion.
4. Remove the graphiti row from `bough.yml`; retire `bough graphiti`
   (launchd job, serve.py, the vendored mcp_server tree, the Neo4j brew
   service and minted password, the FalkorDB fallback).

Notes stay. They become one episode source among several; the graph is where
their citations and cross-references live.
