---
name: domain-modeling
description: Build and sharpen a project's domain model — pin down its ubiquitous language, challenge fuzzy terms, and record architectural decisions as ADRs. Use when designing, or when another skill needs the domain model maintained.
---

# Domain Modeling

Actively build and sharpen the project's domain model as you design. This is the
*active* discipline — challenging terms, inventing edge-case scenarios, and
writing the glossary and the decisions down the moment they crystallise. Merely
*reading* `CONTEXT.md` for vocabulary is not this skill; that is a habit any turn
can have. This skill is for when you are **changing** the model.

Unlike a wayfinder map, these artifacts belong to the repo: `CONTEXT.md` and
`docs/adr/` are committed alongside the code they describe.

## File structure

Most repos have a single context:

```
/
├── CONTEXT.md
├── docs/adr/
│   ├── 0001-event-sourced-orders.md
│   └── 0002-postgres-for-write-model.md
└── src/
```

If a `CONTEXT-MAP.md` exists at the root, the repo has multiple contexts and the
map points at where each one lives (`src/ordering/CONTEXT.md`, plus a
context-local `docs/adr/`). Infer which context the current topic belongs to; if
it is unclear, ask.

Create files **lazily** — only when you have something to write. No `CONTEXT.md`
until the first term is resolved; no `docs/adr/` until the first ADR is needed.

## During the session

**Challenge against the glossary.** When the user uses a term that conflicts with
the existing language in `CONTEXT.md`, call it out immediately: "your glossary
defines 'cancellation' as X, but you seem to mean Y — which is it?"

**Sharpen fuzzy language.** When a term is vague or overloaded, propose a precise
canonical one. "You're saying 'account' — do you mean the Customer or the User?
Those are different things."

**Discuss concrete scenarios.** Stress-test relationships with specific,
invented edge cases that force precision about the boundaries between concepts.

**Cross-reference with code.** When the user states how something works, check
whether the code agrees, and surface any contradiction: "your code cancels whole
Orders, but you just said partial cancellation is possible — which is right?"

**Update `CONTEXT.md` inline.** Capture a term the moment it is resolved; do not
batch. Format: [CONTEXT-FORMAT.md](${SKILL_DIR}/CONTEXT-FORMAT.md).

`CONTEXT.md` is a glossary and nothing else — totally devoid of implementation
detail. It is not a spec, not a scratchpad, not a home for decisions.

**Offer ADRs sparingly.** Only when all three hold: hard to reverse, surprising
without context, and the result of a real trade-off. If any is missing, skip it.
Format: [ADR-FORMAT.md](${SKILL_DIR}/ADR-FORMAT.md).

_Adapted from `mattpocock/skills` (MIT)._
