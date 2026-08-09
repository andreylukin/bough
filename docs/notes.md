# bough — the note memory

The command memory records what ran and whether it worked. It cannot record what it
*meant*. `bough notes` is the layer that does: prose keyed on the tags that already exist,
written by hand when you have something to say and by the cheap model when you do not.

[`tags.md`](./tags.md) is the memory this sits beside; nothing here works without it.

---

## 1. Why it exists

From one real install (11,028 commands, 1,971 tags, 143 references):

* one PR rollout had **nine `session_state` keys** — `pr7134_rollout`, `nased_pr7134`,
  `rollout_7134`, … — each written by a different lineage root, none visible to the next
  session, none readable by a human;
* the work spanned repos: `linear.nme-1673` is **1,405 commands across 7 repos and 37
  sessions**;
* only **13.6%** of commands carry a directory attribution, so anything triggered by
  directories would almost never fire.

Three conclusions. The unit is the **tag**, not the repo. The trigger is the **tag**, not
the directory. And the thing written down is a *conclusion*, which no amount of command
logging produces on its own.

---

## 2. The division of labour

|  | command memory | note memory |
|---|---|---|
| Written by | the harness, on every command exit | you, or the cheap model |
| Holds | what ran, where, exit code, output | what it meant, what was decided |
| Unit | one command | one section |
| Key | tags + repo + dir | **the same tags** |
| Truth from | the exit code | authorship, and citations |

**The invariant: a note holds no command strings and no output.** Every "how" is a
citation — a POINTER, never a copy. Two tests pin it, one at the store and one at the
prompt: `round_gist` has no parameter a command could arrive through.

The payoff is a safety property: a stale note gives wrong *context*, never a wrong
*incantation*. The commands sit one line away and cannot go stale by construction.

---

## 3. Placement is not attachment

Conflating these is the trap the whole design is built to avoid.

**Placement** is `notes.path` — a colon path in the tag grammar's own order. Depth 1 is a
top-level note about a word; deeper paths are notes about a combination.

```
nased                    a subsystem
kubectl:rollout          how rollouts are done here
kubectl:rollout:nased    this particular operation
linear.nme-1673          a ticket
```

Intermediate nodes nothing was written at are **stubs** — computed when `bough notes tree`
renders, never stored. An empty row would be a note that says nothing, and needing to
create the container before you have anything to put in it is exactly why folder
hierarchies ossify.

**Attachment** is `note_tags` / `section_tags` — order-free set membership. The grammar is
faceted, not a containment tree: `nased` appears under `kubectl:rollout:nased` *and*
`helm:upgrade:nased`, so prefix matching would miss the half that carries the meaning. A
note attached to `{kubectl, nased}` covers every command carrying both, in any order, at
any position. That is what makes a tag *group* free.

---

## 4. The section is the atom

A lesson learned while working on `nased:rollout:prod` is often a truth about `nased`. With
the note as the atom it would be stuck where it was written, so the addressable unit is the
**section**: one home, many appearances, resolved at read time and never copied — one fix
repairs every appearance.

```markdown
## Backfill window
Only true of prod: the window closes at 02:00 UTC.

## Executor ordering
tags: nased
DAG removal must land before the executor swap. [cmd:8812]
```

A section **inherits its note's tags by default**, so authoring is unchanged and it appears
only where it was written. A `tags:` line under the heading NARROWS it — and that is
**promotion**, the deliberate act that says "this is general". A section surfaces wherever
its tags are a **subset** of the reader's context. Subset, not overlap: overlap would put
every `git`-tagged section on every page.

Reading `nased:backfill:dev` then shows its own prose *and* `Executor ordering`, labelled
with where it is authored — because a transcluded section that looked native would turn one
home / many appearances into an invisible copy.

`write` **upserts by heading and never removes**: a section has its own tags, citations and
history, and other notes may resolve it, so dropping one is an explicit
`bough notes rm PATH HEADING`.

### Why promotion needs no policy

There is no cap on promotion and no quota. Resolution ranks by `idf` over repos — the same
correction the tag priming note uses:

```
score = Σ over shared tags of  ln(1 + N_repos / repos_using(tag))
```

`git` is in 26 repos, `rg` 28, `inspect` 36; `nased` is in 6. **A section promoted to a
word every repo uses scores at the floor and never wins a slot**, however many pages it
becomes eligible for. The incentive to over-promote disappears because the payoff does.

Two filters from `rank_tags` are deliberately NOT reused here: it drops references, and
`linear.nme-1673` is the most specific match a section can have; and it demotes single-use
tags, but a section deliberately promoted to one is still a valid narrow match.

---

## 5. Citations

What a claim rests on, as rows rather than markdown — so a citation can be **validated**.

```
[cmd:1234]  [msg:<id>]  [file:src/x.rs@3c1c78e]  [url:https://…]  [sec:12]
```

Authored in the prose, parsed into `section_citations` at write time. A `command` citation
must name a row that **exists** AND that **carries one of the section's tags**: existence
stops an invented id, and the tag check stops a real id with nothing to do with the claim,
which is the shape a plausible-but-wrong citation actually takes. One that fails is refused
and **named**, never silently dropped.

`bough notes cites PATH` reports the citations — and, more usefully, marks the sections
that have none. An uncited claim is the interesting one: it is the only signal separating a
claim that rests on evidence from one resting on somebody's memory.

---

## 6. History

Every superseded section body is kept in `section_revisions`. Full copies, never pruned —
a section is small, so the column-mask tricks a large-blob history needs do not apply.

This is what makes resolving a contradiction **auditable**. Without it, a warning cleared
by a rewrite loses the claim it replaced and records that no judgment was made — the exact
silent loss that makes model-arbitrated memory untrustworthy.

```
$ bough notes history nased:rollout:prod
## Backfill window
  * now      The window moved to 04:00 UTC after the executor swap. [cmd:5]
  * rev 1    Only true of prod: the window closes at 02:00 UTC.
```

---

## 7. The four write paths

|  | path | who | model |
|---|---|---|---|
| M1 | `bough notes write PATH` | you, at a terminal | none |
| M2 | `bash("bough notes append …")` | the model, mid-turn | the session's |
| A1 | post-round log line | automatic | **cheap tier** |
| A2 | `bough notes check` | on demand or scheduled | **cheap tier** |

**There is no host function.** The model writes by running `bough notes append`, exactly as
it recalls by running `bough tags` — one door, and the write lands in `command_history`
under its own tag, so the memory records its own maintenance.

Provenance is a column: `*` you · `+` the session model · `~` the cheap model.
`bough notes append` picks it from `$BOUGH_SESSION`, which is set in every shell bough runs
and in none of yours — so the model and the human are told apart without either declaring
itself. It is the only defence against a claim that was **wrong when written**: nothing
later contradicts it, and no timestamp helps.

### A1 — the automatic line

At the end of a round, `fold_round_into_notes` walks the references the round's commands
carried. A page is created only for one that has EARNED it (20 commands across 2 sessions —
about six references on a real memory, not 143). The cheap model sees the last ten log
lines, the note's own claim, and the reference as `tag → worked/failed/still running`; it
sees **no command string, no exit code, no output**. It answers `SKIP` or one line, and the
prompt says SKIP is the usual answer twice.

Every gate is a non-event, as is a missing cheap tier, a provider error, a refusal, or the
deadline: **a bough built without a cheap tier is a working bough that writes no notes**.

### A2 — `bough notes check`

Asks whether any log line makes a section's claim false, and if so **inserts a
`> [!WARNING]`**. It never edits the claim, never compacts the log, never clears a warning.

There is deliberately **no rewriting consolidation pass**. Consolidation solves one failure
— a flat scratchpad accumulating twenty phrasings of one fact — and this log does not have
it: one line per round, deduplicated at write time. For memory that already has structure
and provenance, a rewrite pass is at best a no-op and at worst destructive.

---

## 8. The escalation rule

> **The cheap model may append, and may raise a warning. It may never resolve one.**

Detection is cheap and reversible; judgment is not. Arbitration by the same kind of process
that writes wrong notes does not remove the failure — it moves it one layer down, where the
loss is silent. Resolving a warning means rewriting the body, which means
`bough notes write`, which means you or the session model. It is the only edge in the state
machine the cheap tier cannot traverse.

---

## 9. Staleness is computed

A wiki has no fact stream to check a page against, so its only trigger for revision is "a
new source arrived" and its lint is structural — a page a year out of date lints clean.
Here drift is a COUNT, and no model is involved:

```
$ bough notes stale
linear.nme-1673   ⚠ warning · 412 behind          4d
pr.7134           142 commands since sync         2d
nased             fresh                           1h
```

Warnings sort above volume. `notes.synced_ts` advances **only to a row actually folded,
never to `now`**, so a skipped or failed fold leaves the note visibly behind rather than
falsely fresh. A command carrying two of a note's tags counts once — a per-tag sum would
report a two-tag note as twice as stale.

The frontier is a plain column and not the per-host map the file store needed: **the
database is per machine**, so "this host's frontier" and "this database's frontier" are the
same number.

---

## 10. Context bloat is bounded by change

The mid-turn hint would be pure cost if it re-said what the session was already told, so
`resolve::hint_line` consults an injection ledger keyed by session and section:

| State | What the round carries |
|---|---|
| unchanged | **nothing** — it is in the context above |
| lines added | only the new lines, `+2` |
| >50% churn | the whole section, labelled `rewritten` |
| lines only removed | one line saying so |

The rewrite threshold matters: an added-lines diff would show a corrected claim as an
*addition*, leaving the superseded claim standing in the context above it.

A stable note therefore costs **one injection per session** however many rounds touch it.
The ledger is memory-only and bounded at 512 sessions — a restart that re-injects one
section once is harmless, and a table would make a cosmetic feature a durability problem.

---

## 11. Reading it back

```
bough notes                 every note, most out of date first
bough notes tree            the hierarchy, stubs included
bough notes show PATH       its sections, then what resolves into it (--own to suppress)
bough notes write PATH      add or update sections; markdown on stdin
bough notes rm PATH H       remove one section
bough notes append PATH "…" one line onto the log
bough notes search WORDS    FTS over sections
bough notes stale           how far behind each note is
bough notes check [PATH]    raise warnings where a log contradicts a claim
bough notes history PATH    every superseded version
bough notes cites PATH      what each claim rests on, and which rest on nothing
```

Exit `0` answered · `1` nothing there · `2` usage.

And the surface that matters most, because it needs no new habit — `bough tags show TAG`
prints the resolved sections above the commands, each labelled with where it is authored.

---

## 12. Invariants

- **A note never holds a command or its output.** Two tests pin it.
- **The machine writes only in the derived zone.** A warning is *inserted*, never applied.
- **The cheap model cannot resolve a warning.**
- **A citation is validated or refused**, and a refusal is named.
- **Every superseded body survives** in `section_revisions`.
- **The frontier advances only over folded rows**, and never backwards.
- **Every automatic path is a non-event on failure.** A lost note line is strictly better
  than a broken round.
- **A note's path segments are legal tags.** `canonical_key` refuses anything a command
  could not carry, because an unreachable key is a page that looks filed and is orphaned.

---

## 13. Where the code lives

| File | Owns |
|---|---|
| `crates/bough-core/src/db/schema.sql` | the six tables |
| `crates/bough-core/src/db/notes_sql.rs` | every query, including subset matching in SQL |
| `crates/bough-core/src/notes/mod.rs` | keys, paths, stubs, citations, sections, drift, idf |
| `crates/bough-core/src/notes/resolve.rs` | ranking and the injection ledger |
| `crates/bough-core/src/worker/notes.rs` | the cheap tier's prompts, and what may reach them |
| `crates/bough-core/src/turn/runner.rs` | `fold_round_into_notes`, `with_note_hint_notes` |
| `crates/bough-core/src/hostfn/shell.rs` | the reference trail a round leaves |
| `crates/bough-core/src/prompt/sections/history.md` | how the model is taught to use it |
| `crates/bough-core/skills/prepopulate-tags/` | seeding a cold topic's tags AND its notes |
| `crates/bough/src/notes.rs` | the `bough notes` command |
| `crates/bough/src/tags.rs` | the note header on `bough tags show` |
