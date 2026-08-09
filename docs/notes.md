# bough — the note memory

The command memory records what ran and whether it worked. It cannot record what it
*meant*. `bough notes` is the layer that does: prose keyed on the tags that already exist,
written by hand when you have something to say and by the cheap model when you do not.

[`tags.md`](./tags.md) is the memory this sits beside; nothing here works without it.

---

## 1. Why it exists

The measurement that produced this feature, from one real install (11,028 commands, 1,971
tags, 143 references):

* one PR rollout had **nine `session_state` keys** — `pr7134_rollout`, `nased_pr7134`,
  `rollout_7134`, `pr7134_monitor`, `rollout-pr-7134`, `nased_pr7134_rollout`, `pr7134`,
  `nased_pr7134_monitor`, `monitor` — each written by a different lineage root, none
  visible to the next session, none readable by a human;
* the work itself spanned repos: `linear.nme-1673` is **1,405 commands across 7 repos and
  37 sessions**;
* only **13.6%** of commands carry a directory attribution, so anything triggered by
  directories would almost never fire.

Three conclusions, and they are the whole design. The unit is the **reference**, not the
repo. The trigger is the **tag**, not the directory. And the thing being written down is a
*conclusion*, which no amount of command logging produces on its own.

---

## 2. The division of labour

|  | command memory | note memory |
|---|---|---|
| Written by | the harness, on every command exit | you, or the cheap model |
| Holds | what ran, where, exit code, output | what it meant, what was decided |
| Unit | one command | one page |
| Key | tags + repo + dir | **the same tags** |
| Truth from | the exit code | authorship |
| Lives in | `~/.bough/bough.db` | `~/.bough/notes/` |

**The invariant: a note contains no command strings and no output.** Every "how" is a
citation into `bough tags show TAG`. Break it and the two stores become two copies of the
same facts that age independently — which is the drift the split exists to prevent.
`notes::tests::a_note_never_stores_a_command` and
`worker::notes::tests::the_model_is_never_shown_a_command_string` pin it from both ends.

The payoff is a safety property: a stale note can give you wrong *context*, never a wrong
*incantation*. The commands sit one line away and cannot go stale by construction. A wiki
page that copied the deploy command rots silently and hands you a broken command with full
confidence; this cannot.

---

## 3. The page

```
~/.bough/notes/
  .llmwiki.yaml
  wiki/
    index.md                        ← llmwiki maintains
    refs/linear.nme-1673.md         ← a reference: has a lifecycle
    refs/pr.7134.md
    tags/nased.md                   ← ordinary vocabulary: does not
```

References (a tag containing a dot) file under `refs/`, everything else under `tags/`. A
slash inside a reference is folded to `-` in the filename — `branch.claude/tags-history` is
one id, and half a branch name is a path to nothing.

```markdown
---
title: NASED executor removal
key: linear.nme-1673
synced:
  ASI-K05CYVWP4W: 1786286717159
---

DAG removal must land before the executor swap, or the historical backfill
re-creates the nodes. Incantations: `bough tags show linear.nme-1673`.
Related: [[pr.7134]]

> [!WARNING] the cutover merged green on 2026-08-09; check this claim.

## Log
* 2026-08-08  cutover blocked on the backfill window
+ 2026-08-09  executor swap needs the DAG removal first
~ 2026-08-09  prod otel rollout green on the second attempt
```

### Two zones, two authorities

| Zone | Author | Write mode |
|---|---|---|
| body | you, or the session model | whole-page replace |
| `> [!WARNING]` | cheap model | insert only — never resolve |
| `## Log` | cheap model, session model, you | append only, capped |

The body is **canonical**: original authorship, not derived from anything. The Log is
**derived** from `command_history` and rebuildable. That is why the page is zoned rather
than free-form — one file, two authorities, and the machine only ever writes in the derived
zone.

`bough notes rebuild TAG` drops the Log and reopens every host frontier, so a re-fold can
reproduce it. If it cannot, something canonical was living in the derived zone; the rebuild
verb is that separation test, executable.

### Provenance glyphs

`*` you · `+` the session model · `~` the cheap model.

One character, and the only defense against the failure no query can detect: a note that
was **wrong when written**. Nothing later contradicts it, no timestamp helps, and without
the glyph a machine's inference and your own standing instruction arrive as the same
paragraph of confident text.

`bough notes append` picks the glyph from `$BOUGH_SESSION`, which is set in every shell
bough runs and in none of yours — so the model and the human are told apart without either
having to declare itself.

### Caps

| | |
|---|---|
| `MAX_LOG_LINES` | 40 — past this an append is **refused**, never rotated |
| `MAX_LINE_CHARS` | 120 |
| `MAX_NOTE_BYTES` | 16 KB — the page is refused, never truncated |

A full log is a message that says folding it into prose is a judgment only you make.
Rotation would make the derived zone a lossy copy of a log that is not lossy.

---

## 4. The four write paths

|  | path | who | model |
|---|---|---|---|
| M1 | `bough notes write TAG` | you, at a terminal | none |
| M2 | `bash("bough notes append …")` | the model, mid-turn | the session's |
| A1 | post-round Log line | automatic | **cheap tier** |
| A2 | `bough notes check` | on demand or scheduled | **cheap tier** |

**There is no host function.** The model writes by running `bough notes append`, exactly as
it recalls by running `bough tags` — one door, no bridge to keep in step, and the write
itself lands in `command_history` under its own tag, so the memory records its own
maintenance.

### A1 — the automatic line

At the end of every round, `fold_round_into_notes` (`turn/runner.rs`) walks the references
that round's commands carried:

1. **Does the reference have a page, or has it earned one?** Auto-creation needs
   `AUTO_CREATE_MIN_COMMANDS` (20) across `AUTO_CREATE_MIN_SESSIONS` (2). On a real memory
   that is roughly six references, not 143.
2. **Is the log full?** Then nothing, quietly.
3. **The cheap model is shown** the last ten Log lines, the reference as
   `tag → worked/failed/still running`, and the note's own claim. It is shown **no command
   string, no exit code, and no output** — `round_gist` has no parameter one could arrive
   through.
4. **It answers `SKIP` or one line**, and the prompt says SKIP is the usual answer twice.
   Most rounds are inspection and status checks and are worth nothing to a future session.
5. **The line is appended** if it is not a repeat of the last one — deduplication at write
   time, which is what makes a consolidation rewrite unnecessary.
6. **The frontier advances to the newest row actually folded**, never to `now`.

Every one of those gates is a non-event. So is a missing cheap tier, a provider error, a
refusal, an empty answer and the deadline: a bough built without a cheap tier is a working
bough that writes no notes.

### A2 — `bough notes check`

Asks the cheap model one question per note: does any Log line make the body's claim false?
If so it **inserts a `> [!WARNING]`** naming what changed. It never edits the claim, never
compacts the Log, and never clears a warning.

There is deliberately **no rewriting consolidation pass**. Consolidation solves one
failure — a flat scratchpad accumulating twenty phrasings of one fact — and this Log does
not have it: one line per round, reference-keyed, timestamped, deduped at write time. For
memory that already has structure and provenance, a rewrite pass is at best a no-op and at
worst destructive.

Run it from a schedule if you want it nightly; the schedule mechanism already exists and
this needs nothing new from it.

---

## 5. The escalation rule

> **The cheap model may append, and may raise a warning. It may never resolve one.**

Detection is cheap and reversible. Judgment is not. Arbitration performed by the same kind
of process that writes wrong notes does not remove the failure — it moves it one layer
down, where the loss is silent, because the old claim is gone and nothing records that a
judgment was made.

So resolving a warning means rewriting the body, which means `bough notes write`, which
means you or the session model. It is the only edge in the state machine the cheap tier
cannot traverse.

---

## 6. Staleness is computed

A personal wiki has no fact stream to check a page against, so its only trigger for
revision is "a new source arrived", and its lint is structural — a page a year out of date
lints clean. Bough has `command_history`, so drift is a count:

```sql
SELECT COUNT(*), MAX(ts) FROM command_history h
  JOIN command_tags t ON t.command_id = h.id
 WHERE t.tag = 'linear.nme-1673' AND h.ts > :frontier
```

```
$ bough notes stale
linear.nme-1673   ⚠ warning · 412 behind          4d
pr.7134           142 commands since sync         2d
nased             fresh                           1h
```

Warnings sort above volume. A row clears only when the frontier advances, and the frontier
advances only over rows actually folded — a skipped or failed fold leaves the note
**visibly behind** rather than falsely fresh.

### The frontier is per host

`synced` is a map, not a scalar, and this is not a detail. The notes directory git-syncs
between installs; `command_history` does **not** — every machine has its own database. A
shared scalar would let one machine advance the frontier past another machine's unfolded
rows, and those rows would never be seen again. `$BOUGH_HOST` overrides the hostname.

---

## 7. llmwiki

`~/.bough/notes` is a directory of markdown with YAML frontmatter, which is exactly what
[llmwiki-cli](https://github.com/doum1004/llmwiki-cli) reads. Registered as wiki id
`bough`, it brings `index.md`, `[[wikilinks]]`, `backlinks`, `orphans`, `lint`, `search`
and the graph view for the cost of one shell-out.

Ownership is split, and the split is not arbitrary:

* **page creation** goes through `wiki write`, because that is what upserts `index.md`;
* **appends** are a direct file write, because llmwiki has no append verb and an append
  changes nothing the index records.

**Ordering matters exactly once.** `wiki write` writes its *own* frontmatter around the
content it receives, so it must run first and bough's authoritative render must land on top
of it. Reversed, llmwiki's copy wins and the per-host sync frontier is silently swallowed
into the body — which is a bug that shipped for about ten minutes and is now pinned by
`a_rewrite_keeps_the_derived_log`.

**Every path degrades.** llmwiki is not a dependency, it is an enhancement. Without it,
notes are still written, read, appended, listed and searched; only `index.md` goes
unmaintained, and `wiki lint` fixes that whenever the CLI reappears. Nothing in the bridge
can fail a turn.

---

## 8. Reading it back

```
bough notes                 every note, most out of date first
bough notes show TAG        the prose, then its log
bough notes write TAG       replace the prose; body on stdin
bough notes append TAG "…"  one line onto the log
bough notes search WORDS    notes mentioning every word
bough notes stale           how far behind each note is
bough notes check [TAG]     raise warnings where a log contradicts a claim
bough notes rebuild TAG     drop the derived log so it can be re-folded
bough notes lint            llmwiki's structural check
bough notes path TAG        where the file is
```

Exit `0` answered · `1` nothing there · `2` usage. `--repo` / `--all` scope drift,
`--limit` and `--json` do the obvious.

And the surface that matters most, because it needs no new habit:

```
$ bough tags show linear.nme-1673
note · NASED executor removal
  DAG removal must land before the executor swap.
  ~ 2026-08-09  prod otel rollout green on the second attempt
  (bough notes show linear.nme-1673)

  ✓  4h   kubectl -n nased rollout status deploy/executor
  …
```

---

## 9. Invariants

For anyone changing this code:

- **A note never holds a command or its output.** Two tests pin it, one at the store and
  one at the prompt.
- **The machine writes only in the derived zone.** The body is replaced by `write` and by
  nothing else; a warning is *inserted*, never applied.
- **The cheap model cannot resolve a warning.** If a future change lets it, the trust model
  is gone.
- **The frontier advances only over folded rows**, and never backwards.
- **Every automatic path is a non-event on failure.** A lost note line is strictly better
  than a broken round — the same contract the command recorder holds.
- **llmwiki is optional at every call site.**

---

## 10. Where the code lives

| File | Owns |
|---|---|
| `crates/bough-core/src/notes/mod.rs` | the page: parse, render, append, caps, provenance, per-host frontier |
| `crates/bough-core/src/notes/drift.rs` | staleness as a query, and the auto-create threshold |
| `crates/bough-core/src/notes/llmwiki.rs` | the optional bridge, and the write ordering it forces |
| `crates/bough-core/src/worker/notes.rs` | the cheap tier's prompts, and what may reach them |
| `crates/bough-core/src/turn/runner.rs` | `fold_round_into_notes`, `with_note_hint_notes` |
| `crates/bough-core/src/hostfn/shell.rs` | the reference trail a round leaves |
| `crates/bough-core/src/prompt/sections/history.md` | how the model is taught to write and read notes |
| `crates/bough/src/notes.rs` | the `bough notes` command |
| `crates/bough/src/tags.rs` | the note header on `bough tags show` |
