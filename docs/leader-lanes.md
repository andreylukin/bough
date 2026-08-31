# The leader and the lanes: the open hand (2026-08-30)

Supersedes the 2026-08-29 version of this file, which mapped lane creation onto the claims seam
(propose → Andrey accepts). Andrey's call, after the Headlong research below: **"lets allow as
many threads as the agent wants"**, and then further: **"remove the claims setup altogether"**.
Both are built. This file records what replaced the claims flow and why.

## What the leader has now

- **`create_lane`** (tool-leader, leader-scoped): buds a trajectory off the leader's, writes the
  `agents` row with the asked-for routing refs and wake classes, and brings the resident up — a
  row AND a live agent, or neither (the §2.4 birth dance, moved verbatim from the old
  `claims::decide`). Applied IMMEDIATELY: no card, no waiting. Attributed
  `Attribution::Agent{sol}`, cited (at minimum `call:<tool-call-id>`), and reversible with
  `/undo` — the op is Evidence in the ledger like every other graph op.
- **`merge_lanes`**: the cleanup half. Folds a finished or quiet lane into a survivor; the
  absorbed trajectory stays readable forever (§16, the past is append-only). Self-absorption is
  refused: the leader cannot fold away the lane its own set is mounted on.
- **`curate`**: unchanged (unsorted adoption + timeline).
- **The Lanes roster, every wake** (`leader.lanes`, a projection section at Tail/Before —
  volatile tier, so its churn never touches the stable prompt cache): every lane, its routing,
  and a coarse quiet-age (`active this hour` / `quiet 7h` / `quiet 3d`). Cleanup is something
  the leader SEES, not something it must remember to wonder about; the persona names unremarked
  roster clutter as its failure mode.

## Why open, and what Headlong says

Researched 2026-08-30 (parallel-cli over laude-institute/headlong's design docs — §18's reference
project). Headlong splits the question:

- **Ephemeral streams** (trajectories): unbounded, agent-created, no gate. `traj fork` nests to
  any depth and merges back. bough already matched this (worker forks).
- **Persistent streams** (thinkers — the true analog of a lane): no runtime creation path at
  all; a human installs them and restarts the dispatcher.

So bough's claims gate was already MORE permissive than Headlong's static roster, and the open
hand goes past both. The price of a lane is standing (it wakes, it costs tokens); the
counterweights chosen instead of a gate are visibility (the roster section), duty (the persona's
cleanup sentence), attribution + `/undo`, and Andrey's ability to `/sleep` any lane.

## What the demolition removed, and what replaced each piece

| Was | Is |
| --- | --- |
| `claims` crate (propose/decide, §16's gate) | gone; structure is `create_lane`/`merge_lanes`, direct |
| `propose_claim` (global + leader-shadowed) | gone, including the codemode `claim` alias |
| `/accept`, `/edit`, `/reject`, `/claims` | gone |
| accepted requirement → `pin/set` | **`/pin <agent> <text…>`** and `/unpin <pin-step> <reason…>` (the `pins` plugin, the only pin writers now); first line of the text is the title |
| claim cards, rail `◇N`, status `◇ n claims` | gone (the `?` pending-question chip stays) |
| drift-watch claim-rejection signal | gone (it measured decisions that no longer happen) |
| bench bank task `10-propose-a-claim` | deleted; `Coverage::Claim` left the surface table |

## Residuals, on purpose

- The LEDGER still declares `claim/proposed` / `claim/accepted` / `claim/rejected`: old ledgers
  replay, and two writers still exist that append `claim/proposed` BY NAME — the wards' `mark`
  action (`runtime-actions`) and reconsolidation's contradiction claims (§8). With no decide
  surface these are now cited NOTES, rendered as plain `· claim/proposed` rows; nothing waits on
  them. If they should become mail to the leader instead, that is a follow-up, not part of this
  demolition.
- `retire` (the true "this stream is over") still does not exist; `merge_lanes` absorbs and
  `/sleep` parks, same as before.
- Idle initiative is still deferred: the leader tends the roster when mail wakes it or when
  Andrey asks, never on its own clock.
