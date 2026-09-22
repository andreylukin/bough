---
name: llm-wiki
description: "The LLM wiki at ~/.bough/wiki, compiled from bough's session history. Use for /llm-wiki ingest, /llm-wiki query <question>, /llm-wiki lint, /llm-wiki brief."
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

## Headless

Ingests and briefs run with nobody at the keyboard. Never call
`tools.ask`: it waits minutes for an answer that cannot come. Decide,
and write the assumption into the page as `*Inference:*`.

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
backticks) pointing at the history entry it came from. A claim that rests
on something read outside history (the brief does this) cites it as
`` `<source>:<ref>` ``, with the source one of `gh`, `slack`, `linear`,
`notion`, `git`, `circle`, `url`: `` `gh:owner/repo#7801` ``,
`` `slack:C06TKRHR7J9/1758123.456` ``, `` `linear:NME-1462` ``,
`` `notion:<page id>` ``, `` `git:uni-git-ai-cas@a1b2c3d` ``,
`` `url:https://…` ``. The control room links these; nothing resolves them,
so cite exactly what you read. Find the seq in the
digest *before* you write the claim. No citation, no claim. Inference is
labeled as inference: start the sentence (or bullet) with `*Inference:*`,
so the control room can tell it from a claim that lost its citation.
`"${BOUGH_BIN:-bough}" wiki check` verifies every citation resolves.

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

## Brief

`/llm-wiki brief`: what the person is doing today, across everything you
can read, as `topics/me/briefs/<YYYY-MM-DD>.md` (today's date, local
time) plus `topics/me/signals.json`. The scheduler runs this during
working hours, every half hour; the control room's Me page renders it and
its Refresh runs it now. It is the standup reply they would write, with
evidence.

Read `topics/me/profile.md` first: who they are, their teams, channels,
Slack and Linear ids, the repos they own, and what they want left out. No
profile, no brief — say so and stop. If a profile exists but is thin, use
what is there; do not invent ids.

**Gather, in parallel, each source that is available** (a source that is
not connected or fails is reported in `signals.json` under `sources`, not
guessed at):

1. **Threads** — `"${BOUGH_BIN:-bough}" wiki pending --all` and the recent
   sessions by title: what ran today, what is waiting on an answer, what
   failed. Cite `` `<session>#<seq>` ``.
2. **Git** — `git -C ~/repos/<repo> log --since=yesterday --author=<their email>`
   over the repos the profile names (and any under `~/repos` with commits
   today). Cite `` `git:<repo>@<short sha>` ``.
3. **GitHub** — `gh pr list --author=@me --state=open` and
   `gh search prs --review-requested=@me --state=open`; review comments
   waiting on them. Cite `` `gh:owner/repo#N` ``.
4. **Linear** — issues assigned to them updated in the last two days, and
   status transitions. Cite `` `linear:<KEY>` ``.
5. **Slack** — messages from them in the last day (outreach is work), and
   mentions of them that have no reply. Cite `` `slack:<channel>/<ts>` ``.
6. **Notion** — pages they created or edited since yesterday. Cite
   `` `notion:<page id>` ``.

**Write `topics/me/briefs/<date>.md`:**

```markdown
# Brief, <Weekday> <Month> <D>

<One sentence: the one thing today is about.> `<cite>`

## Since yesterday

- <one casual first-person line per piece of work> `<cite>`

## Today

- <what they are doing or about to do> `<cite>`

## Waiting on

- <person or thing> — <what for> `<cite>`

Updated: <YYYY-MM-DD HH:MM>
```

Voice: casual, first person, chat tone; a bullet is a pointer, not a
report; plain English, no ticket ids or PR numbers in the prose (they are
in the citation); honest about state. Four to eight bullets in all. Open
PRs are not automatically yesterday's work: something is "since
yesterday" only if a commit, comment, message or thread from that window
says so. Every bullet cites. The lede may be `*Inference:*` when it is
your reading of the day; nothing else may.

Rewrite the whole file each run while its date is today. Never touch an
earlier day's brief: those are frozen.

**Write `topics/me/signals.json`** — the rows the Me page lists under the
brief, machine-readable:

```json
{
  "asOf": "<RFC 3339>",
  "items": [
    {"kind": "needs-you", "source": "gh", "title": "Review comment on the demand-settlement fix",
     "note": "Priya, on the timeout default", "project": "smart-scheduler",
     "at": "<RFC 3339>", "cite": "gh:asi/uni-nes#7801", "url": "https://github.com/…"},
    {"kind": "moving", "source": "thread", "title": "fix the broken ci on main",
     "project": "git-ai-enrichment", "at": "…", "cite": "<session>#<seq>", "session": "<session>"}
  ],
  "sources": [
    {"name": "gh", "ok": true, "at": "<RFC 3339>"},
    {"name": "slack", "ok": false, "error": "not connected"}
  ]
}
```

`kind` is one of `needs-you` (a person is waited on: a review, a question,
a mention with no reply), `moving` (running or in review by others),
`waiting` (they wait on someone), `done` (finished since yesterday).
`project` is a bough project slug when the item belongs to one, else the
repo name, else omitted. Keep it under forty items; the page is a glance.

Then `"${BOUGH_BIN:-bough}" wiki check`; fix what it reports. Do not
touch index.md or log.md for a brief, and do not commit; the scheduler does.
