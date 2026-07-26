---
name: history
description: Read and search bough's own conversation history — semantic recall over past sessions, keyword search, list sessions, dump transcripts
---

# bough conversation history

bough stores everything in a SQLite db at `~/.bough/bough.db`:
`sessions` (id, parent_id, title, kind, workspace), `messages` (role, parts JSON,
pending), `turns` (per-turn status/checkpoint). Messages within a session are linear; branching is done by creating a
child session (kind `fork` / `subagent` / `compaction`) pointing at its parent.

Run the helper (no deps beyond python3; opens the db read-only):

```bash
python3 ${SKILL_DIR}/bough_history.py <command>
```

## Find past work: semantic first, keyword for identifiers

For meaning-level questions ("did I solve this before?", "that websocket
reconnect fix from last week") use the **`recall` host function** — it is a
pre-injected global in your run_steps program, backed by local embeddings
(nothing leaves the machine):

```js
const { hits, indexed } = await recall("flaky websocket reconnect fix", 10);
for (const h of hits) {
  console.log(h.score.toFixed(2), h.sessionId.slice(0, 8), h.title, "—", h.snippet);
}
```

- Indexing is lazy: `indexed > 0` means this call spent budget embedding new
  messages and coverage is still growing — call it once or twice more on a cold
  index before trusting an empty result.
- Hits are pointers (session + snippet + cosine score), not transcripts — feed
  `h.sessionId` to `show` below to read the conversation.
- It searches prose (user/assistant text + reasoning), not tool output.

For exact strings — identifiers, error messages, file names — keyword search
beats embeddings and also covers tool calls/results:

```bash
python3 ${SKILL_DIR}/bough_history.py search "resolveInWorkspace"
python3 ${SKILL_DIR}/bough_history.py search "exit code 143" -p bough -n 10
```

Matches print grouped by session (newest first) with a snippet around the hit.

## List sessions (newest first)

```bash
python3 ${SKILL_DIR}/bough_history.py list          # last 30
python3 ${SKILL_DIR}/bough_history.py list -p bough # filter by workspace substring
python3 ${SKILL_DIR}/bough_history.py list -n 5 --json
python3 ${SKILL_DIR}/bough_history.py list --archived --no-empty
```

Each row shows id, last-updated, turn count (user prompts), workspace basename,
kind tag for non-root sessions, and the session title.

## Show a transcript

```bash
python3 ${SKILL_DIR}/bough_history.py show <id>
python3 ${SKILL_DIR}/bough_history.py show <id> -q          # hide system msgs + reasoning
python3 ${SKILL_DIR}/bough_history.py show <id> --full      # don't truncate
python3 ${SKILL_DIR}/bough_history.py show <id> --maxlen 200
```

`<id>` may be a full session id or any unambiguous prefix. The header lists the
session's children (forks/subagents/compactions) — `show` those ids to descend
into subagent transcripts.

Line legend:
- `USER:` / `ASSISTANT:` — the human prompt and the supervisor's reply
  (`(pending: <status> @ <step>)` marks a turn still running or orphaned)
- `~ thinking:` — a reasoning part (hidden by `-q`)
- `▶ <tool>: …` — a tool call (`run_steps` shows the JS code + `[check]`/`[done]`;
  `bash` shows the command; file tools show the path)
- `← …` / `✗ …` — the tool result (✗ = isError)

## Tips

- Ad-hoc queries: `sqlite3 ~/.bough/bough.db` — e.g. grep all conversations with
  `SELECT session_id, id FROM messages WHERE parts LIKE '%some text%';`
- Timestamps are epoch milliseconds.
- Override the db path with `BOUGH_DB` if needed.
- Clonefile snapshots (non-repo workspaces) live in `snapshots`. Repo sessions have
  no snapshots — their history is the workspace's own git.
