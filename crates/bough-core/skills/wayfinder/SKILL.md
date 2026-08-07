---
name: wayfinder
description: Plan a chunk of work too big for one session — chart it as a map of decision tickets under ~/.bough/maps, then resolve them one at a time until the way to the destination is clear.
---

# Wayfinder

A loose idea has arrived — too big for one session, and wrapped in fog: the way
from here to the **destination** isn't visible yet. Wayfinding is about finding
that way, not charging at the destination. This skill charts the way as a **map**
of **decision tickets** — questions whose resolution is a decision, not slices of
a build to execute — and works them one at a time until the route is clear.

The destination varies per effort, and naming it is the first act of charting —
it shapes every ticket. It might be a spec to hand off, a decision to lock before
planning starts, or a change made in place like a data-structure migration. The
map is domain-agnostic; engineering work, writing, whatever fits the shape.

## Plan, don't do

Wayfinder is **planning** by default: each ticket resolves a decision, and the
map is done when the way is clear — nothing left to decide before someone goes
and does the thing. The pull to just do the work is usually the signal you've
reached the edge of the map and it's time to hand off. An effort can override
this in its **Notes**, carrying execution into the map itself; absent that,
produce decisions, not deliverables.

## Refer by name

Every map and ticket has a title. In everything the user reads — narration, the
map's Decisions-so-far — refer to it by that title, never by a bare number or
slug. A wall of `03, 04, 07` is illegible; names read at a glance. The number
doesn't vanish, it rides inside the name.

## Where a map lives

Maps live in bough's own state, **never in the workspace**:

```
~/.bough/maps/<effort-slug>/
├── map.md
└── issues/
    ├── 01-what-does-a-run-own.md
    └── 02-storage-shape.md
```

That location is deliberate. A map is planning state, not a deliverable: in the
checkout it would file every ticket edit into the changes rail and land in
whichever branch happened to be checked out. Here it survives worktrees,
rewinds, `git clean`, and efforts that span more than one repo. Read and write
these files with ordinary filesystem calls; `mkdir -p` the directories.

`<effort-slug>` is lowercase kebab-case, one path segment — no slashes, no `..`.

### `map.md`

The whole map at low resolution, loaded once per session. **Open tickets are not
listed here** — they are files in `issues/`, found by scanning.

```markdown
# <Effort name>

Workspace: <the directory the resulting work lands in>

## Destination

<what reaching the end of this map looks like — the spec, decision, or change
this effort is finding its way to. One or two lines; every session orients to it
before choosing a ticket.>

## Notes

<domain; skills every session should consult; standing preferences for this
effort>

## Decisions so far

<!-- the index — one line per resolved ticket: enough to judge relevance, then
open the file for the detail the ticket holds -->

- **04 Storage shape** (`issues/04-storage-shape.md`) — one file per ticket;
  SQLite rejected because the map must survive a rewind.

## Not yet specified

<!-- see "Fog of war": in-scope fog you can't ticket yet; graduates as the
frontier advances -->

## Out of scope

<!-- work ruled beyond the destination; closed, never graduates -->
```

The map is an **index, not a store.** A decision lives in exactly one place —
its ticket — so the map only gists it and points.

### `issues/NN-<slug>.md`

One file per ticket, numbered from `01`, never a combined tickets file.

```markdown
# <Ticket name>

Type: research | prototype | grilling | task
Status: open | claimed | resolved
Blocked by: 02, 05
Claimed by: <session id>

## Question

<the decision or investigation this ticket resolves>

## Answer

<written on resolution — absent while the ticket is open>
```

- **Blocked by** lists ticket numbers. A ticket is **unblocked** when every file
  it names is `resolved`. Omit the line when nothing blocks it.
- The **frontier** is every ticket that is `open`, unblocked and unclaimed —
  the edge of the known. Lowest number first.
- **Claiming** is `Status: claimed` plus `Claimed by: <this session's id>`,
  written **before any work**, so a concurrent session skips it. If a claim
  names a session that is long dead (check with `/history`), it is stale — say
  so and re-claim rather than silently working a ticket someone else holds.
- Assets produced while resolving a ticket are **linked** from the file (a path,
  an artifact URL, a subagent's session id), not pasted into it.

## Ticket types

Every ticket is either **HITL** — human in the loop, worked *with* the user, who
speaks for themselves — or **AFK**, driven alone. A HITL ticket only resolves
through that live exchange; never stand in for the user's side of it. A grilling
that answers its own questions has broken this and the ticket is not resolved.

- **research** (AFK) — reading docs, third-party APIs, or the web to surface a
  fact a decision waits on. Resolve it by spawning a **subagent** with `agent()`;
  record the subagent's session id in the answer so the working is recoverable.
  Use when the knowledge needed lives outside the workspace.
- **prototype** (HITL) — raise the fidelity of the discussion by making
  something cheap, rough and concrete to react to: an outline, a stub, an
  `artifact()`. Link it. Use when "how should it look / behave" is the question.
- **grilling** (HITL) — conversation via `/grilling` and `/domain-modeling`, one
  question at a time. **The default case.**
- **task** (HITL or AFK) — manual work that must happen before a *decision* can
  be made: signing up for a service so its API can be judged, provisioning
  access, moving data so its shape can be seen. The one type that *does* rather
  than decides, and it earns its place by unblocking a decision, not by
  delivering the destination. Drive it alone where you can; otherwise hand the
  user a precise checklist. The answer records what was done and the facts later
  tickets depend on (where a credential lives, a new URL, a row count).

## Fog of war

The map is *deliberately* incomplete: don't chart what you can't yet see. Beyond
the live tickets lies the **fog** — decisions you can tell are coming but can't
pin down, because they hang on questions still open. Resolving a ticket clears
the fog ahead of it, graduating whatever is now specifiable into fresh tickets.

**Not yet specified** is where that dim view is written down: the suspected
question, the area to revisit. Everything there is in scope, just not sharp
enough to ticket. It doubles as a signpost for where the effort is headed.

**Fog or ticket?** The test is whether you can state the question precisely
*now* — not whether you can answer it now.

- **Ticket** when the question is already sharp, even if it's blocked.
- **Fog** when you can't yet phrase it that sharply. Don't pre-slice fog into
  ticket-sized pieces: one patch may graduate into several tickets, or none.

## Out of scope

Work you have consciously ruled out of *this* effort. Scope, not sharpness,
lands it here, and it never graduates — the frontier stops at the destination.
It returns only if the destination is redrawn, and then as a fresh effort.

When a ticket that already exists turns out to sit past the destination, set it
`Status: resolved` with an answer saying it is out of scope, and leave one line
in **Out of scope**: the gist plus why. It stays *out* of **Decisions so far**,
which records the route actually walked — a scope boundary isn't a step on it.

## Invocation

Two modes. Either way, **never resolve more than one ticket per session** —
except research tickets, which are subagents and may run in parallel.

If the user gave no effort, list `~/.bough/maps/` and ask which map they mean,
or whether this is a new one.

### Chart the map

The user invokes with a loose idea.

1. **Name the destination.** Run `/grilling` and `/domain-modeling` to pin down
   what this map is finding its way to. The destination fixes the scope, so it
   is settled first.
2. **Map the frontier.** Grill again, **breadth-first** this time — fan out
   across the whole space rather than deep on one thread, surfacing the open
   decisions and the first steps takeable now. **If this surfaces no fog** — the
   way is already clear and the journey fits one session — you don't need a map.
   Stop and ask the user how they'd like to proceed.
3. **Create the map**: `~/.bough/maps/<effort>/map.md`, Destination and Notes
   filled in, Decisions-so-far empty, the fog sketched into Not yet specified.
4. **Create the tickets you can specify now**, then wire `Blocked by:` in a
   **second pass** — numbers have to exist before they can be referenced.
   Everything you can't yet specify stays in the fog.
5. **Fire the research subagents** for every `research` ticket you just created,
   in parallel.
6. **Stop.** Charting is one session's work; it hand-resolves nothing.

### Work through the map

The user invokes with an effort, and optionally a ticket. Without one, **you**
pick the next decision, not the user.

1. Load `map.md` — the low-res view, not every ticket body.
2. Choose the ticket. If the user named one, use it; otherwise take the first
   frontier ticket. **Claim it before any work.**
3. Resolve it — **zoom as needed**: open the full body of any related or
   resolved ticket on demand; invoke the skills the `## Notes` block names. If
   in doubt, `/grilling` and `/domain-modeling`.
4. Record the resolution: write the `## Answer`, set `Status: resolved`, and
   append a one-line pointer to the map's **Decisions so far**.
5. Add newly-surfaced tickets (create, then wire); graduate any fog the answer
   has made specifiable, **clearing each graduated patch from Not yet
   specified** so it lives only as its ticket. If the answer reveals a ticket
   sits beyond the destination, rule it out of scope rather than resolving it on
   the route. If the decision invalidates other tickets, update or delete them.

The user may work unblocked tickets in parallel sessions, so expect the files to
change under you: re-read a ticket before writing it.

_Adapted from `/wayfinder` in `mattpocock/skills` (MIT), with the issue tracker
replaced by local markdown in bough's own state._
