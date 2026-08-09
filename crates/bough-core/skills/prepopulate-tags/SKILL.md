---
name: prepopulate-tags
description: Seed the command-tag memory AND the note memory for a topic — survey it with real read-only commands under a small deliberate vocabulary, then write down what the survey concluded, so later sessions start primed instead of cold.
---

# Prepopulate tags

The message names a **topic** — `payments`, `the composer`, `the release
pipeline`. The memory has nothing under it yet, so every session that touches it
starts cold: no priming note, `bough tags show` misses, and the same three
commands get rediscovered every week.

This skill fixes that the only way the memory can be written: by **running the
commands for real**. There is no insert path — `bough tags sql` is read-only and
enforced, and a row exists because a `bash()` finished. So the work is a genuine
read-only survey of the topic, executed under a vocabulary chosen on purpose
instead of improvised per command.

The payoff is three things at once: the tags become this project's words, the
commands become `bough tags show` answers with exit codes attached, and their
output lands in `output_head` and the FTS index — so a later session can find the
survey by what it *printed*, not only by what it was called.

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
good word**. Getting the vocabulary right before running anything is therefore
the whole job; the commands are just how it gets written down.

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

**1. Read the vocabulary that already exists.** Before coining anything:

```bash
bough tags            # this project's ranked words — what sessions are primed with
bough tags stats      # coverage and vocabulary per day
bough tags show <w>   # what a candidate word already means here
```

Snapping onto an existing word beats coining a synonym every time; `checkout`
and `checkouts` split a ranking that `checkout` alone would win. Controlled
suggestion earns its keep through *consistency*, not novelty — coin only what
the topic genuinely adds.

**2. Explore the topic, and mine it for words.** Find what the topic actually is
here — the files, the entry points, the tooling, what the Makefile and CI
already run for it. Take the candidate vocabulary from **the repo's own
language**: the words in the Makefile targets, the module names, the CI job
names. That is the cold-start trick the semi-supervised tag-recommendation work
uses (Lipczak; Krestel) — untagged material already carries the terms, so derive
them rather than invent them, and a future session reaching for the obvious word
hits the memory.

**3. Design the vocabulary, then show it.** Six to twelve tags, grammar
`tool:intent:subject` — the tool that ran, what it was for, what it was about.
Lowercase slugs, colon-separated in the call, 3–5 per command.

- **Plan at least two uses for every coined word**, across genuinely different
  commands. One use is demoted on read; it is a typo with a row attached.
- **Do not coin a word that is a substring of its own command.** Write-time
  hygiene drops those once the repo's vocabulary is large enough, because FTS
  already finds them — the tag was duplicating the keyword index.
- **Dotted tags are references** (`pr.456`, `linear.eng-1234`). They join, they
  never rank. Use one when the topic really is a ticket; do not build the seed
  out of them.
- Tell the user the planned vocabulary in one short block before running
  anything. Given how strongly a suggestion anchors what comes after, this is the
  one part of the run worth a moment of their attention.

**4. Run the survey.** Every command through `bash(cmd, tags)`, tagged from the
planned vocabulary — no improvised words at the call site, that is the whole
point. Ten to twenty-five commands is the useful range: enough that each word
lands twice, small enough that every command earned its place.

Cover the shapes a later session actually asks for — *where does this live*,
*how do I run it*, *how do I know it works*, *what does healthy look like*. A
failing command is worth keeping when the failure is the truth about the topic;
the exit code is recorded and read back first, so a red row teaches too. Do not
retry it into a lie.

**5. Write down what you learned.** The survey leaves commands, exit codes and
output behind. It does not leave the UNDERSTANDING behind, and that is the half
a future session cannot reconstruct — so write it, one top-level note per coined
word:

```bash
bough notes write nased --title "NASED" <<'EOF'
The scheduler's evaluator. Runs as a Deployment per environment; the
executor and the DAG builder are separate images. [cmd:8812]

## Where it lives
tags: nased
Config is in the gitops repo, not this one. [file:apps/nased/values.yaml]
EOF
```

Three rules, and the first is the same one that governs the commands:

- **Claim only what the survey showed.** The skill forbids faking a command
  because a zero exit code is supposed to mean something. A note asserting what
  no command demonstrated is the same defect one level up, and worse, because
  prose carries no exit code to check it against. If you inferred something,
  say you inferred it.
- **Cite it.** `bough tags sql "SELECT id, cmd FROM command_history WHERE
  session_id = '$BOUGH_SESSION'"` gives you the ids your own survey just wrote;
  `[cmd:<id>]` attaches a claim to the command that showed it. A citation that
  does not resolve is refused and named, so this cannot be faked either.
- **No commands in the prose.** The command memory already holds them, and
  `bough tags show <tag>` is the citation. A note that copies an incantation
  goes stale silently; a note that points at one cannot.

This is also the only way a new topic gets a note at all: automation creates
one only after 20 commands across 2 sessions, which a topic nobody has worked
on yet will never reach.

**6. Verify and report.** Read the memory back the way a future session will:

```bash
bough tags
bough tags show <each-coined-tag>   # the note now prints above the commands
bough notes cites <each-coined-tag> # and what each claim rests on
```

Then report: the vocabulary, what each word means, how many rows carry it;
which claims you had to leave uncited;
anything that failed and why it was kept; anything you had to survey around
because it would have mutated something. If a word came back with a single row,
say so — it will not survive into the priming note.
