---
name: history
description: Read bough's own conversation history — list sessions, dump transcripts, grep past turns
---

# bough conversation history

bough stores everything in a SQLite db at `~/.bough/bough.db`:
`sessions` (id, parent_id, title, kind, workspace), `messages` (role, parts JSON,
pending), `turns` (per-turn status/checkpoint), `net_events` (gated network
requests). Messages within a session are linear; branching is done by creating a
child session (kind `fork` / `subagent` / `compaction`) pointing at its parent.

Run the helper (no deps beyond python3; opens the db read-only):

```bash
python3 ${SKILL_DIR}/bough_history.py <command>
```

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
python3 ${SKILL_DIR}/bough_history.py show <id> --net       # append gated net requests
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
- `# net events` (with `--net`) — host, verdict, reason per gated request.
  Caveat: bough currently records `net_events.session_id` as NULL, so this is
  empty until that attribution bug is fixed; query the table directly and
  correlate by timestamp instead.

## Tips

- Ad-hoc queries: `sqlite3 ~/.bough/bough.db` — e.g. grep all conversations with
  `SELECT session_id, id FROM messages WHERE parts LIKE '%some text%';`
- Timestamps are epoch milliseconds.
- Override the db path with `BOUGH_DB` if needed.
- Session snapshots (shadow-git/clonefile refs) live in `snapshots`; workspaces under
  `~/.bough/workspaces/<id>`.
