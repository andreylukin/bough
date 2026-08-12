# Configuration

## Environment

Read from `~/.bough/env` (and the process environment).

**Keys.** At least one is required. See [install.md](install.md).

```
ANTHROPIC_API_KEY  OPENAI_API_KEY  OPENROUTER_API_KEY  CLOUDFLARE_API_TOKEN
```

**The two that matter most**

| | |
|---|---|
| `BOUGH_HOME` | Relocates the entire data root (default `~/.bough`) |
| `BOUGH_PORT` | Moves the listener (default `4321`) |

Together they are how a development instance never touches a live install. `BOUGH_PORT`
is an environment variable and deliberately not a flag: the API client is bound before a
flag could be read, so a `--port` that parsed and did nothing would be the bug.

**Models**

| | |
|---|---|
| `BOUGH_MODEL` | The frontier model that runs turns |
| `BOUGH_CHEAP_MODEL` | Titles, ghost text, activity blurbs, automatic note lines |
| `BOUGH_EFFORT` | Default thinking depth |

All three are also settable in the model picker (`^o`), which persists to
`~/.bough/model.json`.

**Endpoints.** Every provider's base URL is overridable, and both the picker and the
turn read the same variable, so a custom base moves both.

| | |
|---|---|
| `ANTHROPIC_API_BASE` | default `https://api.anthropic.com` |
| `OPENAI_API_BASE` | default `https://api.openai.com` |
| `OPENROUTER_API_BASE` | default `https://openrouter.ai/api` |
| `CLOUDFLARE_API_BASE` | default `https://api.cloudflare.com/client/v4` |

This is how you reach something that is not the vendor: a gateway, a proxy, or a model
running on your own machine. Use the **OpenRouter** slot for anything OpenAI-compatible:
it speaks `/v1/chat/completions`, which is the dialect Ollama, vLLM, LM Studio and
LiteLLM serve. Point `OPENROUTER_API_BASE` at the server, put any non-empty string in
`OPENROUTER_API_KEY` for the servers that ignore it, and address the model by its
`vendor/model` id.

The `openai:` slot is *not* the one to use for a local runtime: OpenAI rides
`/v1/responses`, and most local servers do not implement that endpoint.

**Tuning**

| | |
|---|---|
| `BOUGH_BASH_BG_AFTER_MS` | When a foreground `bash` auto-backgrounds (default ~60s) |
| `BOUGH_SUBAGENT_TIMEOUT_MS` | Subagent wall clock |
| `BOUGH_WORKFLOW_CONCURRENCY` | Agents at once inside a workflow (default up to 16) |
| `BOUGH_WORKFLOW_TIMEOUT_MS`, `BOUGH_WORKFLOW_SIZE`, `BOUGH_WORKFLOW_CONFIRM`, `BOUGH_WORKFLOW_TOKEN_WARN` | Workflow limits and prompts |
| `BOUGH_SCHEMA_ATTEMPTS` | Retries when a schema-validated agent result does not validate |

**The embedding layer** (optional, derived, and safe to disable)

| | |
|---|---|
| `BOUGH_NO_EMBED` | Turn it off |
| `BOUGH_EMBED_MODEL`, `BOUGH_LEMBED_PATH` | Model and `sqlite-lembed` extension location |

**Development**

| | |
|---|---|
| `BOUGH_DB` | Override the database path |
| `BOUGH_TRACE_DIR` | Write turn traces |
| `BOUGH_DIR` | Where `install.sh` clones to |

## What lives under `~/.bough`

```
env                  API keys and environment
bough.db             sessions, messages, turns, workflows, command memory, notes
embeddings.db        the optional vector index (derived; deletable)
model.json           model picker state
theme.json           theme picker state
mcp.json             MCP registry
mcp-auth.json        MCP credentials
skills/              your skills
hooks/               your Lua hooks
extensions/          your JavaScript extensions
workflows/           saved workflow scripts
artifacts/           published artifacts, per session
artifact-versions/   artifact history
comments/            artifact comment batches
attachments/         images pasted into the composer
scratch/             per-session scratch, including spilled command output
maps/                wayfinder maps
logs/                server logs
```

Two of these are **derived and safe to delete**: `embeddings.db` rebuilds, and `scratch/`
is per-session working space.


## The database

One SQLite file, and **the table set is closed**: every column is created in a single
block at open. There is no migration ladder, deliberately: the previous implementation
accumulated one column at a time, and the result was a schema you could only learn by
reading its migration history. A change that needs a new column stops and asks.

```
sessions  messages  turns  workflows  workflow_agents  session_state  schedules
command_history  command_tags  command_dirs
messages_fts  command_history_fts
```

Sessions form a forest by `parent_id`, and a session's thread is its ancestors' messages
plus its own, which is what makes fork and compaction cheap. Message ordering is
`(created_at, rowid)` everywhere, never `created_at` alone, because branch seeding and a
turn started immediately after can land in the same millisecond.

Notably absent, each for a reason: no archive/delete columns (visibility is derived from
`kind` and `origin_id`), no jobs table (a background shell dies with the server, so a
persisted row would always be a lie after a restart), no message embeddings (cross-session
message search is keyword FTS), and no artifacts or skills tables (the filesystem is the
source of truth, and both survive a database reset).

`bough tags sql` opens this file read-only; see [tags.md](tags.md).
