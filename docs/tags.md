# bough: the tag system

Every shell command bough runs carries tags the model wrote at the moment it wrote the
command. Those tags are the join key of a cross-session memory: what was tried in this
project, what worked, and what it printed. This document describes the whole loop: how a
tag is written, normalized, stored, ranked, fed back into the prompt, and read out again.

[`spec.md`](./spec.md) is authoritative for behavior; this expands the two lines it spends
on the memory (§6 Shell, §6 Session verbs) into the mechanism.

---

## 1. Why tags exist

A command string is a terrible retrieval key. `bun test src/tui/components/Chat.test.tsx`,
`bun test src/tui`, and `bun test --watch src/tui` are the same intent in three spellings,
and clustering them after the fact is guesswork over text that was never written to be
clustered.

The bet bough makes instead: **the model labels its own intent at generation time.** It
knows why it is running the command (it just decided to) and saying so costs a few tokens
inside a call it was already making. The exit code that arrives moments later is the ground
truth that weights the label, so a tag on ten failing attempts never becomes "popular".

That is the entire design. Everything below is consequences of it.

---

## 2. The grammar

A tag string is **3–5 lowercase tags, colon-separated, naming the tool, the intent, and the
subject**:

```js
await bash("git push origin main",          "git:push:main");
await bash("psql -f migrations/004.sql",    "psql:migrate:demand");
await bash("bun test src/tui",              "bun:test:composer");
```

The subject is the part that pays. A bare tool name is a wasted tag: `wc` says nothing a
future session can search for, `app:linecount` does. The prompt (`src/prompt/shell.md`)
states this to the model directly, along with the rule that follows from one row per
command: **one command, one intent.** A `&&` chain is a single history row under a single
tag set, so `mkdir -p out && bun run build && bun test` records one thing where there were
three, and the two untagged intents are simply gone. Chains survive only where they are one
act: `cd dir && cmd`, a pipeline, a redirect, a guard whose point is the `&&`.

### Normalization

`normalize_tags()` (`bough-core/src/history/tags/record.rs`) is what makes a folksonomy converge: if
`PSQL:Migrate` and `psql:migrate` are different tags, every popularity number fragments.

| Input | Recorded | Rule |
|---|---|---|
| `PSQL:Migrate` | `psql:migrate` | lowercased |
| `repo-inspect` | `repo:inspect` | dashes are **separators**, not tag characters |
| `git push` | `git:push` | so is whitespace |
| `bun::test:` | `bun:test` | empties dropped |
| 9+ tags | first 8 | `MAX_TAGS = 8` |
| `...` | *(dropped)* | a tag must keep at least one letter or digit |

Legal characters after slugging are `[a-z0-9_.]`. The one exception to every rule above is
a reference.

### References: the dot is the whole rule

A tag containing a **dot** is a *reference*: a pointer to something with an identity outside
bough.

```js
await bash("psql -f migrations/004.sql", "psql:migrate:demand:linear.eng-1234");
```

`linear.eng-1234`, `pr.456`, `commit.3c1c78e`, `branch.claude/tags-history`. Dashes and
slashes survive **inside** a reference and nowhere else; the id is whatever the tracker
calls it, and half a branch name is a reference to nothing. Written bare, `ENG-1234` still
becomes the two useless tags `eng` and `1234`, because without a namespace there is nothing
to distinguish an identifier from a hyphenated phrase and `repo-inspect` must keep
splitting.

The two kinds behave oppositely, which is why they are told apart at all:

|  | Tag | Reference |
|---|---|---|
| Nature | a **word** the model coins | a **key** with one referent |
| Converges through reuse | yes: that is the point | never |
| Lives in | many projects | exactly one |
| Ranked into the priming note | yes | **no**: see §5 |
| Recalled by | popularity, hints, search | name (`bough tags show pr.456`) |

Same table, same joins, same graph. Different ranking. Spending a reference therefore costs
the vocabulary nothing, which is why the prompt asks for one whenever the work belongs to a
ticket or a PR.

---

## 3. The write path

### `bash(cmd, tags)`: tags are required

Tags are enforced at the **host-function boundary** (`create_shell_host_fns`,
`bough-core/src/hostfn/shell.rs`), not inside `bash()` itself: internal callers and tests drive `bash()`
directly and owe no tags; the *model* does. A missing or empty-after-normalization tag string
throws a catchable `ProgramError` that restates the format with examples, so a model that
forgot self-repairs on the next call instead of abandoning the round.

There is no untagged door. `child_process.execSync`, `Bun.$` and `Bun.spawn(["sh", "-c", …])`
throw, because a command that does not pass through here is never recorded, and an
unrecorded command is one the project can never recall.

### `sh()` legs tag individually

```js
await sh([{cmd: "bun test", tag: "bun:test:composer"},
          {cmd: "bun run check", tag: "tsc:typecheck:tui"}]);
```

Strings and objects mix freely in one array, and **a bare-string leg is recorded with no
tags at all**: it lands in `command_history` with `tags = ''`, findable by FTS or by SQL but
never by tag. That gap is what `bough tags stats` measures as coverage (§8).

### When the row is written

A command is recorded **when it finishes**, with the output the program actually saw:

- Normal exit → recorded immediately with `exit_code`, `duration_ms`, and the first
  `OUTPUT_HEAD_CHARS` (2 000) of the combined output, spill marker included.
- Auto-backgrounded past ~60s → the row waits for the **real** exit. A promoted build that
  fails ten minutes later must not be remembered as a success.
- Still running when the turn moved on → `exit_code` is `NULL`, which the ranking treats as
  half-credit (§5).

### What is NOT recorded

**A command that only reads or maintains the memory.** `bough tags …` and
`bough notes …` are skipped by `is_memory_command` before anything is attributed,
in every spelling: `PATH=… bough notes show x`, `./scripts/bough tags`,
`~/.local/bin/bough notes stale`.

This closes a loop that was measured on a real install: of 748 commands recorded in a
few days, 271 were bough talking to itself, and per tag it was worse. **53 of the 54
commands tagged `notion` were `bough notes show notion`**, 39 of 40 for `slack`, 69 of
72 for `history`. Recall was recorded as work, which lifted the tag's weight, which
raised it in the priming note and the hints, which prompted another recall. Every turn
of that loop carries zero information about the project, and the "this repo has worked
on that before" hint had become a tour of bough's own CLI.

Writes are skipped too, not only reads. `bough notes write` was recorded at first on the
argument that "the memory records its own maintenance", a nicer sentence than it was a
rule. Bookkeeping about the work is not the work.

Deliberately narrow: `bough patterns` reads a real log and `bough mcp` changes real
configuration, so both stay recorded. Only the two verbs whose entire subject is the
memory are skipped. Rows written before the gate existed are still there, so the
exemplar picker in §6 filters them at read time as well, because otherwise an existing install
would keep reciting itself until the 30-day half-life worked them out.

### Failure contract

Recording is best-effort and **must never surface a failure into a turn**. `createCommandRecorder`
swallows everything: a broken git checkout, a locked database, a hostile command string
loses one memory row, not the round. Same contract as search indexing.

---

## 4. What a tagged command records

```
command_history ──┬── command_tags(command_id, tag)      one row per tag
                  ├── command_dirs(command_id, rel_dir)  directories it was about
                  └── command_history_fts(cmd, tags, output_head)
```

`command_history` columns (`src/db/schema.sql`):

| Column | Meaning |
|---|---|
| `id` | `INTEGER PRIMARY KEY`: a high-volume append-only log joined through two junctions; the rowid alias is the natural join key |
| `session_id`, `ts` | who ran it, when (epoch ms) |
| `repo` | the **scope key**: git origin URL, else a path (below) |
| `cmd`, `tags` | the command, and the normalized colon-separated string (`''` for an untagged leg) |
| `exit_code` | `NULL` = unknown (still running when the turn moved on) |
| `duration_ms` | |
| `output_head` | first ~2k chars the command **printed**: recall over results, not just invocations |
| `spill_path` | the file holding the full output when it was too big to inline; may have been cleaned since |
| `source` | `live` \| `backfill` |
| `message_id` | the supervisor message whose `run_steps` program ran it: nullable, because a memory row outlives its transcript |

Two of these carry more design than their names suggest.

**`repo` is resolved from what the command *touched*, not from where the session sits.**
`attribute_command()` mines path-looking tokens out of the command string (bounded: 24 tokens
checked, 4 directories kept), resolves each to its enclosing git checkout, and scopes the row
to the checkout containing the most of them. A session rooted at `~` running
`cd ~/repos/bough && cargo test -p bough-tui` therefore files into *bough's* memory, where sessions
rooted in that repo will find it, which is the miss this rule exists to fix. The identity itself is
the **git remote origin URL** where there is one, else the workspace root path, so a project
that is moved or re-cloned keeps its tag profile.

**`command_dirs` is not the cwd.** A bough program runs at the workspace root and never cds,
so cwd carries no signal. `bun test src/tui/x.test.ts` is attributed to `src/tui`, relative
to the winning checkout's root, which is what makes per-directory tag profiles (§6)
possible at all.

**`message_id` is the other half of recall.** On anything but a one-liner, the *program* is
the reusable part and the command is a line inside it, so `bough tags show --program` walks
this link back to the round the command ran in.

The whole insert (history row, tag rows, dir rows, FTS row) is **one transaction**
(`Db.recordCommand`). A half-recorded command would silently skew every popularity query
that joins them.

---

## 5. Ranking

Two numbers, computed in `bough-core/src/history/tags/stats.rs`.

### Weight: success × recency

```
weight(tag) = Σ over its commands of  successFactor(exit_code) × 0.5 ^ (age / 30 days)
```

| `exit_code` | factor | why |
|---|---|---|
| `0` | 1 | it worked |
| `NULL` | 0.5 | unknown: still running when the turn moved on |
| anything else | 0.25 | a tag on ten failures must not read as popular |

The half-life is 30 days and the query looks back 5 half-lives (150 days); beyond that a row
carries under 3% weight and is not worth reading. Memory that is never deprecated erodes
performance; a repo's profile should track what the user does *now*. The decay runs in JS
rather than SQL so nothing depends on the SQLite build carrying math functions.

### Score: weight × idf over repos

Raw popularity is the wrong ranking for the priming note. The grammar is
`tool:intent:subject`, and popularity is dominated by the first two: `git`, `bun`, `rg`,
`test` recur in *every* project, while `composer` or `retention` recur only in the one they
belong to. A top-ten by weight spends most of its slots anchoring the model on the dimension
where reuse was never in doubt.

And that anchoring is not free: suggested tags drive convergence, so whatever the note lists
is what gets reused. Listing tool names buys a narrower vocabulary for nothing.

The correction is inverse document frequency over repos:

```
score(tag) = weight(tag) × ln(1 + N_repos / repos_using(tag))
```

A tag every project uses is damped; a tag only this project uses is lifted. With one repo in
the memory every idf is `ln 2` and the order collapses to exactly the popularity order,
the honest answer when there is nothing to contrast against, and the reason this needs no
special case for a fresh install.

**References never rank.** `linear.eng-1234` lives in exactly one repo, so idf hands it the
maximum boost, *and* it accumulates real weight because a ticket is worked over many
commands. The two multiply, and without the filter the note would open every session by
reciting last week's ticket numbers instead of this project's words.

---

## 6. How tags reach the model

### The session-start priming note

`tags_note_for()` produces one volatile-tier prompt note:

> This project's own tag vocabulary: the words it uses that other projects do not:
> `bun, tui, composer, retention, …`. Reuse these when they fit; coin new ones freely when
> they do not, especially for the tool and the intent.

Top 10 by score, for the workspace's repo. `null` for a project with no history yet, and
then simply omitted; the static examples in `prompt/shell.md` are the cold-start fallback.

**It is frozen per session.** The volatile prompt tier is cached per session with a 1h TTL,
so a note whose text drifted mid-session would bust that cache every turn. `tagsNoteFor` is
memoized by session id for the process lifetime (bounded at 512 entries), and the same memo
records the primed set so every other surface agrees with it.

Every session kind that runs a program gets the note, subagents and workflow agents included
because they run commands in a real checkout and benefit from the same vocabulary.

### Per-directory hints, mid-turn

When a round newly touches a directory (by a `view()` read or by a path its shell commands
named) and that directory's own tag profile **diverges** from what the session was primed
with, one dim line is appended:

```
[history] tags previously used in src/logs/: drain, anomaly, sketch; run `bough tags show <tag>` for the commands behind them
```

Rules (`dirTagHints`):

- **Divergence only.** Tags already in the priming set are filtered out; nothing left means
  no line, and no context bloat.
- **Once per directory**, at most **4 per session**.
- **Directory profiles use plain popularity**, not idf, because the question is "what has been done
  in here", where the tool name is part of the answer and the set is already narrow.
- **Absolute paths, resolved per directory.** A directory in a *foreign* checkout surfaces
  that repo's profile, labelled by its home-abbreviated path; the workspace repo's own root
  is skipped, because its profile *is* the priming set.

These are appended to the round's **result**, never to the prompt, because a mid-session prompt edit
would bust the volatile-tier cache the note depends on (`with_dir_tag_hint_notes`,
`bough-core/src/turn/runner.rs`).

### The TUI

The session snapshot carries `primedTags`, and the transcript opens with one dim margin row:

```
# this repo remembers: bun · tui · composer · retention …
```

`#` is reserved across the transcript for one meaning, remembered rather than happening now, so
this row and the `[history]` hints share a glyph and share dimness, and neither borrows the
tool grammar or the system amber.

---

## 7. Reading it back with `bough tags`

**The memory has no host function.** Recall is a command, run in the shell, and it is the
same door for the model and for the human: one surface, and no bridge to keep in step with
it. It needs no server, because it only reads.

```
bough tags                  this project's tag vocabulary, ranked, with the arithmetic
bough tags show TAG         the commands under TAG, newest first, exit code first
bough tags stats            tag coverage and vocabulary per day
bough tags sql "SELECT …"   a read-only SELECT over the memory and the transcripts
bough tags similar "text"   semantic recall, where the local vector layer exists
```

| Option | Effect |
|---|---|
| `--repo R` | scope to a repo identity (origin URL or path); default is this checkout's |
| `--all` | no repo scope: answer across every project the memory knows |
| `--program` | `show`: print the whole program each command ran in, not just its line count |
| `--limit N` | rows (default 20) |
| `--days N` | `stats`: how far back to look (default 30) |
| `--json` | machine-readable output |

Exit codes: `0` answered · `1` no command memory yet (or no vector layer, for `similar`) ·
`2` usage problem, including a rejected query.

A bare word is treated as `show TAG`, the commonest thing to type and the likeliest
to be a tag, and guessing beats a usage error that names three verbs. `--all` beats an
explicit `--repo`: asking for everything after naming one is a correction, not a
contradiction. The default `list` view has no meaningful cross-project form (there is no
project to be distinctive *against*), so it stays scoped to the checkout regardless.

### `sql` is read-only by construction

This is the reason the memory is reached through a command rather than through advice to run
`sqlite3`: the guarantee is **structural**, against a file a live server is writing to.

1. The handle is opened `{readonly: true}`.
2. `PRAGMA query_only = ON`, which also covers anything a clever statement `ATTACH`es.
3. A keyword check refuses anything not starting with `SELECT` or `WITH`, so a write attempt
   is answered with a sentence naming the queryable tables instead of a bare
   `SQLITE_READONLY`.
4. `PRAGMA busy_timeout = 2000`, so a concurrent writer is a brief wait rather than a
   spurious "database is locked".
5. Results are capped at 200 rows, so one greedy `SELECT` cannot flood a terminal, or a
   tool result.

Queryable surface: `command_history`, `command_tags`, `command_dirs`,
`command_history_fts`, `messages`, `messages_fts`, `sessions`, `turns`. It is the same
`~/.bough/bough.db` that holds the transcripts, which is why the bundled `history` skill
answers questions about conversations through this same command.

The commands worth copying are the ones that **worked**:

```bash
# what worked for docker here before
bough tags sql "SELECT cmd FROM command_history JOIN command_tags t ON t.command_id = id
  WHERE t.tag = 'docker' AND exit_code = 0 ORDER BY ts DESC LIMIT 5"

# keyword search when you do not know the tag; FTS covers cmd, tags AND output
bough tags sql "SELECT h.cmd, h.exit_code FROM command_history_fts f
  JOIN command_history h ON h.id = f.command_id
  WHERE f.cmd MATCH 'migrate' ORDER BY h.ts DESC LIMIT 10"

# this conversation; $BOUGH_SESSION is set in every shell bough runs
bough tags sql "SELECT cmd FROM command_history
  WHERE session_id = '$BOUGH_SESSION' ORDER BY ts DESC LIMIT 10"
```

**Never open the database file directly.** Not with `sqlite3`, not with a library. A stray
write or a long lock on the file a live server holds is a broken turn for everyone.

### `similar` and the optional vector layer

`bough tags similar "text"` is KNN recall over the memory, where the layer exists. Two
loadable SQLite extensions do all of it: `sqlite-vec` (the `vec0` KNN table) and
`sqlite-lembed` (a local MiniLM as a SQL function), so there is no native Node module, no
subprocess, and no API call:

```
drain:    INSERT INTO vec_index SELECT id, lembed('embed', tags || cmd) …
similar:  … WHERE embedding MATCH lembed('embed', ?) ORDER BY distance
```

Vectors live in their **own** file, `~/.bough/embeddings.db`, never in `bough.db`: every
other connection in the system (the migrator, `bough tags sql`'s readonly handle, the
drain's own reader) lacks the `vec0` module, and a virtual table they cannot parse must not
sit in a file they walk. Embeddings are fully derived state and can be deleted freely.

Where the SQLite build cannot load extensions, the layer simply is not there: the command
exits 1 and says so, naming the FTS query to use instead. Tags plus FTS carry recall alone.

---

## 8. Measuring with `bough tags stats`

The measurement the tag system otherwise lacks: whether a prompt change made the model name
*more things* or just repeat itself. One row per local day.

| Column | Meaning |
|---|---|
| `sessions` | distinct sessions that ran commands |
| `cmds` | commands recorded |
| `tagged` | share of them carrying at least one tag: the number a bare `sh` leg moves |
| `vocab` | distinct **coined** tags that day (references excluded) |
| `refs` | distinct references that day, counted apart so a busy ticket week does not read as a richer vocabulary |
| `uses` | total tag applications |

How to read it: **vocab rising with uses flat** is the model naming more things, which is the
point. **Uses rising with vocab flat** is it repeating itself. Days are grouped in SQLite's
*local* time, because the question is "what did I do on Tuesday" and a UTC boundary answers
a different one.

---

## 9. Invariants

For anyone changing this code:

- **Recording never fails a turn.** Every path in `record.ts` and `stats.ts` swallows its
  own errors. A lost memory row is strictly better than a broken round.
- **The insert is atomic.** History row, tag rows, dir rows and FTS row go in one
  transaction, or the popularity joins skew silently.
- **The priming note is frozen per session.** Anything that makes its text drift mid-session
  busts the volatile-tier prompt cache. Mid-turn information goes into the round's *result*,
  not the prompt.
- **The CLI and the prompt share one ranking.** `bough tags`'s default view *is* the priming
  note's ranking, from the same functions in `history/stats.ts`, so a surprising tag is
  traceable to the commands behind it rather than taken on faith.
- **All SQL lives in `db/`.** No raw SQL outside it, except the user-supplied string
  `bough tags sql` runs against a read-only handle.
- **Normalization is pure and total.** `parseTagsArgs` and `normalizeTags` never throw, and
  `runTags` returns an exit code without touching a real process, so the whole command is
  testable against an in-memory database and two collectors.

---

## 10. Where the code lives

| File | Owns |
|---|---|
| `bough-core/src/prompt/sections/shell.md` | the grammar as the model is taught it: `bash(cmd, tags)`, references, one-command-one-intent |
| `bough-core/src/prompt/sections/history.md` | how the model is taught to *recall*: the CLI, the tables, the `exit_code = 0` habit |
| `bough-core/src/hostfn/shell.rs` | the boundary that requires tags; recording on exit, including after auto-backgrounding |
| `bough-core/src/history/tags/record.rs` | normalization, the reference rule, repo identity, directory attribution, the recorder |
| `bough-core/src/history/tags/stats.rs` | weighting, decay, idf ranking, the priming note, the per-directory hints |
| `bough-core/src/history/tags/embed.rs` | the optional local vector layer behind `similar` |
| `bough-core/src/db/schema.sql` | `command_history` and its three junction/index tables |
| `bough-core/src/db/sqlite_db.rs` | every query: `record_command`, `command_tag_rows`, `tag_spread`, `tag_diversity_by_day`, `commands_for_tag`, `program_for_message` (the `Db` trait declaring them is `bough-core/src/types.rs`) |
| `bough/src/tags.rs` | the `bough tags` command: parsing, the read-only `sql` handle, rendering |
| `bough-core/src/turn/runner.rs` | wiring: the note into the prompt, the hints onto the round's result |
| `bough-server/src/sessions.rs` · `bough-tui/src/lines.rs` | `primedTags` on the snapshot, and the `#` margin row |
| `bough-core/skills/history/SKILL.md` | the bundled skill that queries transcripts through the same command |

---

## Appendix: one command, end to end

```js
await bash("bun test src/logs/drain.test.ts", "bun:test:drain:linear.eng-1204");
```

1. **Boundary.** `normalizeTags` returns `bun:test:drain:linear.eng-1204`: four tags, the
   last one a reference (it has a dot, so its dash survives).
2. **Run.** The command exits 0 in 4.2s, printing 300 chars.
3. **Attribution.** `src/logs/drain.test.ts` resolves inside this checkout → `repo` is the
   origin URL, `rel_dir` is `src/logs`.
4. **Record.** One transaction: the history row (with `output_head` and the supervisor
   `message_id`), four `command_tags` rows, one `command_dirs` row, one FTS row.
5. **Rank.** `drain` gains weight 1.0, decaying by half every 30 days; it is a word few
   other repos use, so idf lifts it. `bun` and `test` gain the same weight but are damped
   toward the bottom of the note. `linear.eng-1204` gains weight and is excluded from the
   ranking entirely.
6. **Feed back.** Tomorrow's session opens with `drain` in its priming note. A round that
   reads `src/logs/` for the first time and finds tags *not* already primed gets one
   `[history]` line.
7. **Recall.** `bough tags show drain` lists this command, `✓`, `1d ago`, with
   `--program` to see the round it ran in. `bough tags show linear.eng-1204` lists every
   command run for that ticket, across every session.
