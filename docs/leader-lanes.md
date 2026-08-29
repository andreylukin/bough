# The leader and the lanes: create, retire, recommend (2026-08-29)

What Andrey asked (his words): "figure out how to correctly have the leader have the ability to
create/delete/recommend streams and how we want to interact with that. long term I would mostly
be working with the leader but sometimes go to the workers to give specific instructions."
"Streams" are LANES (§4's malleable lanes). This doc maps the ask onto what already exists on
`ux-brief`, names the gaps, and recommends the shape of the missing pieces. Nothing here is
implemented beyond what is marked BUILT.

## What is already built, end to end

The loop is §16's sentence made mechanical: *structure is proposed by the system, made real only
by Andrey.*

1. **The leader recommends.** `sol` is the leader by config (`bundles/bough-tui-app.yml`, the
   `leader` row; moving the set to another agent is a patch). Its persona says exactly this job:
   "propose splits, merges and new lanes as claims. You accept nothing." Its scoped
   `propose_claim` tool accepts the structural kinds — `lane`, `split`, `merge`, `bud` — and
   writes `claim/proposed`, never an op: there is deliberately no path from `tool-leader` to
   `ctx.graph`.
2. **Andrey decides.** Open claims are the ◇ chips on the rail and the status line; the cards sit
   in the conversation. `/claims` lists, `/accept <claim>` takes it as it stands, `/edit` takes
   it with changed text, `/reject <claim> <reason>` refuses with a reason; a click on the card
   drives the SAME `ClaimsHandle::decide` seam, so keyboard and mouse cannot drift.
3. **Acceptance applies the op.** `claims::decide` executes the graph op through `ctx.graph`
   (`graph/bud`, `graph/split`, `graph/merge` — Evidence steps, cited, with `graph/undo` to walk
   one back). A new lane arrives with its `agents` row, its trajectory, its routing refs and wake
   classes from the claim's proposal, so the collectors' mail starts reaching it immediately.
4. **Talking to workers directly.** The composer's `to:` chip (and a click on a rail row) targets
   any lane; the message is delivered to that lane and wakes it. This is the "sometimes go to the
   workers" path, and it needs nothing new.
5. **Parking without deleting.** `dormancy` is mounted: `/sleep <lane>` suppresses wakes while
   ordinary mail still queues; `/resume` drains the backlog in one wake; `/paused` lists. A lane
   you are not sure about sleeps instead of dying.

So "create" and "recommend" are BUILT, and the interaction model Andrey described is the one the
tree already implements: live in `to: sol`, say "look at the lanes and propose what we should
have"; sol answers with claim cards; `/accept` the ones that are right; drop into a worker with
`to:` when an instruction is specific.

## The gaps

- **Retire (the "delete" half) does not exist.** The structural kinds stop at `merge`, which
  ABSORBS a lane into a survivor (right when the work folds into other work), and `dormancy`
  parks one (right when the work may come back). Neither says "this stream is over". There is no
  `retire` claim kind and no graph op that ends a lane.
- **Recommendations arrive only when the leader wakes.** Idle initiative (Phase 7's idle ticks)
  is deferred, so sol proposes structure when mail wakes it or when Andrey asks — never on its
  own clock. Fine for now; worth remembering that "the leader never suggests anything" will be a
  symptom of this, not a bug in the claims path.
- **Small UX debt** (tracked in `docs/tui-brief.md`): clicking the ◇ chip does not yet jump to
  the claim card.

## Recommendation: `retire` as the fifth structural kind

The same propose/decide/apply shape as the other four, nothing bespoke:

- `ClaimKind::Retire { lane }` joins the structural kinds `tool-leader` admits, so sol can
  propose it and only Andrey can make it real.
- On accept, a `graph/retire` op (Evidence, cited, with an `UndoShape`): dispose the lane's
  `agents` row (no more wakes, the rail row leaves), seal a final rollup of the trajectory the
  way `merge` seals its reconciliation, and unlink the routing refs so the router stops
  delivering there. The TRAJECTORY STAYS — the past is append-only and shared (§16); timeline
  and search still reach it, exactly like a fork's chain after the fork is gone.
- `graph/undo` of a retire re-creates the `agents` row against the surviving trajectory, which
  is what makes retire safe to accept casually.
- Until that lands: `/sleep` for "probably done", a `merge` claim into the nearest surviving
  lane for "fold it in".

Second, cheaper recommendation: sol's persona gains one sentence — "when Andrey asks about
structure, always answer with claims, not prose" — so recommendations are always decidable
objects rather than paragraphs. That is a bundle edit, no code.
