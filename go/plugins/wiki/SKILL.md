---
name: llm-wiki
description: "The LLM wiki at ~/.bough/wiki, compiled from bough's session history. Use for /llm-wiki ingest, /llm-wiki query <question>, /llm-wiki lint."
manual: true
---

# llm-wiki

bough keeps two levels of record:

1. **History** — `~/.bough/history/<session>.jsonl`, the append-only log of
   every session. The source of truth. Never edit it.
2. **The wiki** — `~/.bough/wiki/`, markdown pages *you* compile from that
   history: what was decided and why, how things work, where things live,
   what failed and why, what the user prefers. Knowledge, not transcripts.

Nothing from the wiki is ever put into a prompt automatically. It is read on
demand — by this skill, or by an agent that greps it like any other file.

## Running `bough wiki`

Always call it as `"${BOUGH_BIN:-bough}" wiki …`: the scheduler sets
`BOUGH_BIN` to the binary that launched this ingest, which may not be the
`bough` first on your shell's PATH.

## Layout

```
~/.bough/wiki/
  index.md                    one line per page, grouped by topic
  log.md                      append-only: one heading per ingested session
  topics/<topic>/<page>.md    the pages; one level of topic directories
```

The wiki is a git repo; the scheduler commits after every ingest. Topics are
short kebab-case nouns (`bough`, `go-testing`, `infra`, `people`). Reuse an
existing topic before creating one.

## Page format

```markdown
# <Title>

<1-3 sentence summary.>

## <sections as the subject needs>

Claims, each followed by its citation: the ghost-mode PTY test is flaky
under -race `01a078c3-02b1-7274-b778-49c5496a8d5d#214`.

## See also
- [Other page](../other-topic/other-page.md)

Updated: YYYY-MM-DD · Sessions: `<session>#<seq>`, ...
```

**The grounding rule.** Every load-bearing claim — a decision, a number, a
path, a command, a cause — carries a citation `` `<session>#<seq>` `` (in
backticks) pointing at the history entry it came from. Find the seq in the
digest *before* you write the claim. No citation, no claim. Inference is
labeled as inference. `"${BOUGH_BIN:-bough}" wiki check` verifies every citation resolves.

When a newer session contradicts a page, keep the old claim and mark it:
`**Outdated** (superseded by `<session>#<seq>`): ...` — do not silently
rewrite history.

## Ingest

Run from the scheduler (`"${BOUGH_BIN:-bough}" wiki run`) or by hand. Sessions are named in
the command (`/llm-wiki ingest <id> <id>`); with none named, run
`"${BOUGH_BIN:-bough}" wiki pending` and take the first three.

For each session, one at a time (index.md and log.md are shared state):

1. **Read.** `"${BOUGH_BIN:-bough}" wiki digest <id> --from <N>`, where N is the last seq this
   session was ingested to (the `#<seq>` in its latest log.md heading; 0 if
   none; `"${BOUGH_BIN:-bough}" wiki pending` prints the range).
2. **Triage.** Search the wiki (`rg -i` over `~/.bough/wiki/topics`, and
   index.md) for the session's subjects. Decide the disposition:
   - **No material** — a greeting, a one-off lookup, a test run, nothing a
     future session would want. Log it and move on. Most sessions are this;
     do not force a page out of a thin session.
   - **New** — create page(s).
   - **Update** — merge into existing page(s).
   - **Disputed** — contradicts a page; mark as above (combines with New or
     Update).
3. **Compile.** Write or update pages in `topics/<topic>/`. Durable
   knowledge only: decisions and their reasons, root causes, gotchas, dead
   ends and why they were dead, conventions, where things live, the user's
   stated preferences. Skip play-by-play.
4. **Cascade.** `rg` the whole wiki for the entities you touched; update every
   page whose claims this session changes.
5. **Index.** Add or update one line per touched page in index.md under its
   topic heading: `- [Title](topics/<topic>/<page>.md) — one-line summary`.
6. **Log.** Append to log.md, exactly:
   ```
   ## [YYYY-MM-DD] ingest | <session>#<last seq in the digest> | <disposition> | <pages touched, or ->
   ```
   This heading is what marks the session as done — never skip it, even for
   No material.

Then run `"${BOUGH_BIN:-bough}" wiki check` and fix what it reports. Do not commit; the
scheduler does.

## Query

`/llm-wiki query <question>`:

1. Read index.md, then `rg -i` the pages for the question's terms and
   synonyms. Never say the wiki has nothing until both searches came up empty.
2. Read the pages found; answer with links to them and their citations.
3. When a citation matters, verify it: `"${BOUGH_BIN:-bough}" wiki digest <session> --from
   <seq-1>` shows the entry.
4. Do not write files unless asked. If asked to save the answer, write it as
   a new page under the right topic, cite the pages and entries it rests on,
   index it, and log `## [YYYY-MM-DD] query | <title>`.

## Lint

`/llm-wiki lint`:

1. `"${BOUGH_BIN:-bough}" wiki check` — missing citations, broken links, pages not in the
   index. Fix what is mechanical.
2. Read across pages for contradictions, claims newer sessions superseded,
   duplicate pages that should merge, and important subjects mentioned
   without a page. Fix clear cases; list the rest for the user.
3. Log `## [YYYY-MM-DD] lint | <what changed>`.
