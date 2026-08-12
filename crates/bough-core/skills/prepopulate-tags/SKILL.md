---
name: prepopulate-tags
description: Seed the command-tag memory AND the note memory for a topic — research it properly first with free reads, let the vocabulary fall out of what the research found, then run a real read-only survey under it and write down the conclusions, so later sessions start primed instead of cold.
---

# Prepopulate tags

The message names a **topic** — `payments`, `the composer`, `the release
pipeline`. The memory has nothing under it yet, so every session that touches it
starts cold: no priming note, `bough tags show` misses, and the same three
commands get rediscovered every week.

This skill fixes that the only way the memory can be written: by **running the
commands for real**. There is no insert path — `bough tags sql` is read-only and
enforced, and a row exists because a `bash()` finished. So the work is a genuine
read-only survey of the topic, executed under a vocabulary the research earned.

The payoff is three things at once: the tags become this project's words, the
commands become `bough tags show` answers with exit codes attached, and their
output lands in `output_head` and the FTS index — so a later session can find the
survey by what it *printed*, not only by what it was called.

## The one structural constraint, and the way around it

`bash(cmd, tags)` requires its tags **at the call site**, before the command has
printed anything. Taken naively that forces the vocabulary to be invented up
front, from a topic you have not yet studied — which is the worst possible moment
to coin the words that everything downstream will imitate.

The way around it is that **reading is free**. `view()`, file reads, and the
existing memory cost the vocabulary nothing, because they are not shell rows. So
the topic can be researched at length — its files, its entry points, its history,
its prior art — before a single tag is committed. Research first, in the medium
that carries no commitment. Coin only what that research turned up. Then spend
the tagged commands, which are the expensive, permanent part.

Where a survey command must run *before* the vocabulary is settled — you cannot
learn what `make` does here without running it — treat it as a first round under
words mined verbatim from the repo's own surface (a Makefile target, a module
name, a CI job). Those are the low-commitment words: they were already this
project's language, so adopting them coins nothing.

## What the seed is actually doing

Collaborative tagging converges by **imitation**: a resource's tag distribution
settles into a stable power law only after roughly a hundred annotations, and
what stabilizes it is later taggers reusing what earlier ones wrote (Halpin,
Robu & Shepherd, WWW'07 / TOWEB'09). Twenty commands do not produce consensus.
They produce *something to imitate* — which is the entire lever, because
suggestion is powerful out of proportion to its size: about a third of tag
applications in a real system are induced by what was suggested, and up to half
of all tag applications are not meaningful at all (Suchanek, Vojnović &
Gunawardena, CIKM'08). A seeded word will get adopted **whether or not it was a
good word**. That is the whole reason the research comes first: a vocabulary
guessed in the first minute is as durable as one earned in the twentieth, and
nothing downstream will correct it.

Three consequences worth holding onto:

- **Small and reused beats broad.** Vocabulary growth in tagging systems is
  sublinear in Heaps' law fashion — the innovation rate is supposed to fall as
  the corpus grows. A seed that coins thirty words models the wrong behaviour.
  bough enforces the same thing from the read side: a tag with one use is
  demoted out of the hints, so a word used once was never vocabulary.
- **Specific beats generic.** Recommendation in a live folksonomy converges
  vocabulary *and* lifts less-popular, more specific tags (Font et al., ACM TIST
  2015). bough's ranking agrees — it is `weight × idf` over repos, so `git`,
  `test` and `rg` are damped and the word that belongs only to this project
  wins. Seed the words that are specific to the topic; the tool names take care
  of themselves.
- **The seed decays.** Reuse is predicted by frequency *and* recency — an ACT-R
  base-level curve with a power-law forgetting term (Kowald & Lex; Trattner), and
  that is exactly what bough weights tags by. A seed is at its most persuasive in
  the days right after it lands. Re-run this skill on a topic that has gone quiet
  rather than assuming last quarter's survey still primes anything.

## Never fake a command

A command runs here only if it is one you would genuinely run to understand this
topic, and whose output is worth recalling. `true # payments`, an `echo` of a
tag, a command chosen because a word still needed its second use — those poison
the memory permanently, because the memory's whole claim is that a tag with a
zero exit code names something that *worked*.

**Read-only, always.** Inspect, list, query, build, test, dry-run. Never write,
migrate, deploy, push, delete, or start anything long-lived. If a topic's only
interesting commands mutate, survey around them and say so.

## The steps

### 1. Turn the topic into questions

Before reading anything, write down what a future session will actually arrive
wanting to know. Four shapes cover most of it — *where does this live*, *how do I
run it*, *how do I know it works*, *what does healthy look like* — plus whatever
is specific to this topic. Keep the list; it is what the research is answering,
what decides which commands are worth a row, and what step 8 reports back
against. A question the survey never answered is a finding, not a failure.

### 2. Read the vocabulary that already exists

The memory is the first source, not an afterthought — the topic may be half
covered under a word you would never have guessed.

```bash
bough tags                        # this project's ranked words
bough tags stats                  # coverage and vocabulary per day
bough tags show <candidate>       # what a word already means here
bough tags similar "<the topic>"  # semantic recall, where the vector layer exists
bough tags sql "SELECT h.cmd, h.tags, h.exit_code FROM command_history_fts f
  JOIN command_history h ON h.id = f.command_id
  WHERE f.cmd MATCH '<topic word>' ORDER BY h.ts DESC LIMIT 20"
bough notes search <topic>        # and whether anything is already written down
```

FTS covers `cmd`, `tags` **and** `output_head`, so search for what the topic
would have *printed* as well as what it would have been called, and try more than
one spelling before concluding the memory is empty. `bough tags --all` answers
whether a sibling project already has words for this.

Snapping onto an existing word beats coining a synonym every time; `checkout` and
`checkouts` split a ranking that `checkout` alone would win. Controlled
suggestion earns its keep through *consistency*, not novelty.

### 3. Research the topic with free reads

This is the bulk of the work and it costs the vocabulary nothing, so do it
properly rather than sampling. Read the code, the entry points, the Makefile and
CI config, the docs, the tests, the recent commits that touched it. Follow what
you find: a config key that names a service, a test that names a failure mode, a
CI job that names a stage.

Take the candidate vocabulary from **the repo's own language** — Makefile
targets, module names, CI job names, the nouns the tests use. That is the
cold-start trick the semi-supervised tag-recommendation work uses (Lipczak;
Krestel): untagged material already carries the terms, so derive them rather than
invent them, and a future session reaching for the obvious word hits the memory.

Keep two lists as you go: **candidate words**, each with where you saw it, and
**commands worth recording**, each with the question from step 1 it answers. Stop
when the reading stops changing either list.

### 4. Commit the vocabulary, and keep going

Now — not before — settle on six to twelve tags, grammar `tool:intent:subject`:
the tool that ran, what it was for, what it was about. Lowercase slugs,
colon-separated in the call, 3–5 per command.

- **Every word must trace to something the research found.** If you cannot say
  where you saw it, it is a guess, and a guess seeded here is imitated for
  months.
- **Plan at least two uses for every coined word**, across genuinely different
  commands. One use is demoted on read; it is a typo with a row attached.
- **Do not coin a word that is a substring of its own command.** Write-time
  hygiene drops those once the repo's vocabulary is large enough, because FTS
  already finds them — the tag was duplicating the keyword index.
- **Dotted tags are references** (`pr.456`, `linear.eng-1234`). They join, they
  never rank. Use one when the topic really is a ticket; do not build the seed
  out of them.

Tell the user the vocabulary in one short block, each word with the evidence
behind it — then **continue straight into step 5 without waiting for a reply**.
It is a heads-up, not an approval gate; they can interrupt if a word is wrong.

### 5. Run the survey, in rounds

Every command through `bash(cmd, tags)`, tagged from the settled vocabulary — no
improvised words at the call site, that is the whole point. Ten to twenty-five
commands is the useful range: enough that each word lands twice, small enough
that every command earned its place.

Run them in **rounds, and read what each round printed before choosing the next**
— that is the difference between a survey and a checklist. Output routinely names
something the reading did not: a second service, a failing path, a config the
docs never mention. Follow it. A round that answers a question from step 1 and
raises two more is the run working; go back to free reads to chase them if that
is cheaper than a command.

A failing command is worth keeping when the failure is the truth about the topic;
the exit code is recorded and read back first, so a red row teaches too. Do not
retry it into a lie.

Stop when a round raises nothing new — not when a count is reached.

### 6. Reconcile the vocabulary against what actually happened

The research predicted the words; the survey tested them. Before writing
anything down, check:

```bash
bough tags sql "SELECT t.tag, COUNT(*) FROM command_tags t
  JOIN command_history h ON h.id = t.command_id
  WHERE h.session_id = '$BOUGH_SESSION' GROUP BY 1 ORDER BY 2 DESC"
```

A word with one row will not survive into the priming note. Either it has a
genuine second command still owed — run it — or the research was wrong about it,
in which case say so in step 8 rather than manufacturing a use for it. A word the
survey turned up that you never planned is the better outcome: it came from
evidence. Give it its second use if it deserves one.

### 7. Write down what you learned

The survey leaves commands, exit codes and output behind. It does not leave the
UNDERSTANDING behind, and that is the half a future session cannot reconstruct —
so write it, one top-level note per coined word:

```bash
bough notes write atlas --title "ATLAS" <<'EOF'
The scheduler's evaluator. Runs as a Deployment per environment; the
executor and the DAG builder are separate images. [cmd:8812]

## Where it lives
tags: atlas
Config is in the gitops repo, not this one. [file:apps/atlas/values.yaml]
EOF
```

Answer the step-1 questions in the prose, and say plainly which ones the survey
could not answer — a named gap is worth more to the next session than silence,
because it stops them re-running the research you already found the edge of.

Three rules, and the first is the same one that governs the commands:

- **Claim only what the survey showed.** The skill forbids faking a command
  because a zero exit code is supposed to mean something. A note asserting what
  no command demonstrated is the same defect one level up, and worse, because
  prose carries no exit code to check it against. If you inferred something from
  reading rather than running, say you inferred it.
- **Cite it.** The query in step 6 (selecting `id, cmd`) gives you the ids your
  own survey just wrote; `[cmd:<id>]` attaches a claim to the command that showed
  it, and `[file:path@rev]` attaches one to what you read. A citation that does
  not resolve is refused and named, so this cannot be faked either.
- **No commands in the prose.** The command memory already holds them, and
  `bough tags show <tag>` is the citation. A note that copies an incantation goes
  stale silently; a note that points at one cannot.

This is also the only way a new topic gets a note at all: automation creates one
only after 20 commands across 2 sessions, which a topic nobody has worked on yet
will never reach.

### 8. Verify and report

Read the memory back the way a future session will:

```bash
bough tags
bough tags show <each-coined-tag>   # the note now prints above the commands
bough notes cites <each-coined-tag> # and what each claim rests on
```

Then report: the questions from step 1 and which the survey actually answered;
the vocabulary, what each word means, what evidence it came from, and how many
rows carry it; which words the research proposed and the survey did not support;
which claims you had to leave uncited; anything that failed and why it was kept;
anything you had to survey around because it would have mutated something. If a
word came back with a single row, say so — it will not survive into the priming
note.
