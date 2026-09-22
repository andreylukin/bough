---
name: multi-model-plan
description: "Plan a task with three models from different providers: drafts, cross-critiques, an interview, then a merged plan. Use as /multi-model-plan <task>."
manual: true
---

# multi-model-plan

Install once: `ln -s "$PWD/go/skills/multi-model-plan" ~/.bough/skills/multi-model-plan`
(from the bough checkout). `preflight.sh` lives next to this file, at
`~/.bough/skills/multi-model-plan/preflight.sh`.

`$BOUGH` below means `"${BOUGH_BIN:-bough}"`. Every child run sets
`BOUGH_WEB_ADDR=127.0.0.1:0` so it never binds the web port.

## 0. Pick three candidates

Three `plugin/model` pairs from **different providers** (plugins). Defaults:
the `llm` row in `bough.yml` (see `$BOUGH rows`) plus other `llm-*` plugins
whose keys are set, e.g. `llm-anthropic/claude-sonnet-5`,
`llm-openai/gpt-6`, `llm-openrouter/deepseek/deepseek-chat`. The user may name
others in the task; use theirs.

## 1. Preflight

```sh
~/.bough/skills/multi-model-plan/preflight.sh P1/M1 P2/M2 P3/M3
```

It exits 1 if fewer than 3 distinct models answer `OK`. If it does, report
which models failed and **stop**. Never fall back to fewer models.

## 2. Drafts

Slug the task (`fix-flaky-tests`). Work in `$BOUGH_SCRATCH/plans/<slug>/`.
Write the brief to `brief.md` (the task, the relevant paths, constraints).
`<model>` in file names is the model with `/` replaced by `-`.

Run the three drafts in parallel as background shell jobs (`bash` with
`background: true`, or `tools.bash` in a js block), one per model, each a
fresh process:

```sh
cd "$BOUGH_SCRATCH/plans/<slug>" &&
  { cat brief.md; printf '\n\nWrite an implementation plan for this task. Reply with the plan only, as markdown.'; } |
  BOUGH_WEB_ADDR=127.0.0.1:0 "${BOUGH_BIN:-bough}" --headless --set llm.plugin=P --set llm.model=M \
  > draft-<model>.md
```

Wait for all three jobs to finish.

## 3. Critiques

Three more fresh runs, in parallel. Each model critiques only the two drafts it
did not write, into `critique-<model>.md`: pipe `brief.md` plus the other two
drafts (label them `Draft A`/`Draft B`, not by model) with "Critique both
plans: wrong assumptions, missing steps, risks. Name the draft for each point."

## 4. Interview

Read the drafts and critiques. Find 2-4 real disagreements between drafts
(approach, scope, order, tooling). For each, ask the user
(the `ask` tool with `question` and `options`, or
`tools.ask(question, option1, option2, ...)` in a js block), with options
taken from the drafts. One question per disagreement; no questions about
things all drafts agree on.

## 5. Merge

Write, in the same directory:

- `plan.md`: the merged plan.
- `merge-notes.md`: every decision, the draft it came from, and the critique
  point or interview answer it rests on.

## Gate

Before reporting, verify with `ls` and `wc`:

```sh
cd "$BOUGH_SCRATCH/plans/<slug>" && ls -l draft-*.md critique-*.md plan.md merge-notes.md && wc -l draft-*.md critique-*.md
```

There must be 3 non-empty drafts and 3 non-empty critiques that together cover
all 6 (critic, draft) pairings. If any file is missing or empty, rerun that
step for that model; do not merge around a gap. Report the paths of
`plan.md` and `merge-notes.md`.
