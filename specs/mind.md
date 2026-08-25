# Spec: the mind — persistent agency above the turn loop

A **mind** is a session that keeps thinking between external interactions. Nobody
has to message it: a ticker wakes it, it does one thing, it goes back to sleep, and
the interval stretches while nothing is happening. A message from a person does not
start anything special — it lands in the same session, is answered by an ordinary
turn, and resets the wake cadence.

This is an outer loop layered ON the turn loop, not a change to it. Every wakeup is
one ordinary turn through `turn/runner.rs`, with every turn invariant intact except
the single one this spec relaxes (§3). The design is adapted from Headlong
(laude-institute/headlong): its monolith thinker, idle backoff, trajectory steps,
and tiered life summary, mapped onto bough's sessions, turns, and prompt assembly.

Module: `crates/bough-core/src/mind/`. Consumes: `agents/notes.rs` (the wake),
`turn/queue.rs` (the busy check), `session_state` (durable loop state),
`CheapTier::summary` (rollups). Nothing in `turn/` consumes `mind/`.

---

## 1. The session kind

`SessionKind::Mind` (`"mind"` on the wire and in `sessions.kind`). A mind is a
root: no parent, always listed, never collapsed. Created over HTTP like any root
(`POST /sessions` with `kind: "mind"`); the composer, fork, and every history
operation work on it unchanged.

## 2. The trajectory: steps

The transcript records everything; the **trajectory** records the narrative. It is
a typed, append-only layer over the same session:

```
mind_steps(id INTEGER PK, session_id, turn_id NULL, ts, type, source, content)
```

- `type` ∈ `thought` | `observation` | `idle` | `goal` | `learning` | `message`.
- `source` names the process that wrote the step: `self` (the model, via
  `step()`), `user`, `system` (mirrored by the driver).
- Append-only. No edit, no delete. Content is clipped at 4,000 chars.

**`step(type, content)`** is a host function granted only to mind sessions. It
appends one step stamped with the current turn, publishes `mind.step`, and returns
`"ok"`. An unknown `type` or empty `content` is a 400 naming the valid set —
error text is a product surface. `message` is reserved for the mirror (§5): the
model reports what it did as `observation`, it does not forge inbound messages.

## 3. The one relaxed invariant

`turn/runner.rs` holds "every turn must produce user-visible text" with a report
nudge and then a forced text-only round. **For `kind: mind` both are skipped**: a
wakeup that appended an `idle` step and called `stop` is complete. Everything else
holds — a turn still always ends, ends exactly once, still requires the explicit
`stop`, still gets stop-nudges, and the pending message is still closed on every
path. Non-mind kinds are untouched; the existing mute-turn tests keep their
guarantees and mind-scoped twins pin the exemption.

## 4. Context is a projection

A mind lives for thousands of steps; its context must not grow with its life.

- **Windowed replay.** For mind sessions the thread replays only its last
  `MIND_REPLAY_WINDOW` (default 40) messages, cut at a user/system message
  boundary so no tool pair is split. Older rows stay in the database untouched —
  nothing is compacted away in place, the projection just does not show it. The
  context-overflow error is unchanged and remains reachable; the window exists so
  it is not reached in normal operation.
- **The recent stream.** The last `MIND_STREAM_TAIL` (default 30) steps, rendered
  as a volatile prompt note, newest last, each clipped to 500 chars.
- **The life summary.** Tiered rollups (§7), oldest tier first, rendered as a
  volatile prompt note above the stream.
- **The menu.** A stable section (`sections/mind.md`) gated on
  `kind == mind && granted(step)`: pick ONE function per wakeup — act / think /
  observe / goal / learn / idle — write steps with `step()`, then stop. The
  persona (§6) rides the volatile tier since it is per-session.

## 5. The driver

A ~30s ticker (same shape, seams, and stop semantics as the schedule ticker).
Per enabled mind session, per tick, in order:

1. **Settle.** If a wake is pending (`mind.pending_turn`) and that turn has
   finished: on `error`, `fail_streak += 1`; on `done`, `fail_streak = 0` and the
   wakeup is *idle* iff it produced no steps or only `idle` steps
   (`idle_streak += 1`, else `= 0`). Then `next_wake_at = now + backoff` (§6),
   clear pending, mint rollups (§7). A turn that ended `interrupted` disables the
   mind (`mind.enabled = false`) and records — never wakes — a note saying so:
   a stop stays stopped, and here it stays stopped until `mind start`.
2. **Mirror.** Messages that arrived since `mind.last_mirrored_id` (walked in
   thread order) become steps: `user` rows → `message`/`user`, system notes →
   `observation`/`system`, skipping the driver's own wake notes. Any new user
   message resets `idle_streak` to 0 and pulls `next_wake_at` down to
   `now + base` — the person talking is what the backoff decays *from*.
3. **Guard.** `fail_streak >= MIND_MAX_CONSECUTIVE_FAILURES` (10) disables the
   mind and records a note naming the count and the re-enable command. The
   ceiling is the *driver's* guard; the turn's own retry bounds still apply
   inside each wakeup.
4. **Wake.** If idle in the registry, not stopped, and `now >= next_wake_at`:
   post the wake note through `agents/notes.rs` (`post_system_note`), which
   already holds every wake invariant — starts a turn on an idle session, queues
   behind a running one, never races, never double-wakes. Record
   `mind.pending_turn`. The wake note is one short stable line; the instructions
   live in the prompt, not the note.

The driver never calls the LLM on the tick path and never blocks on a turn. It is
a state machine over database facts — every read re-derivable after a restart, so
a server crash mid-wake recovers to a consistent loop (the orphaned turn settles
as `error` did).

## 6. Loop state and backoff

Durable state in `session_state`, root-scoped to the mind session itself:

| key | meaning |
|---|---|
| `mind.enabled` | `"true"`/`"false"`; absent = false |
| `mind.persona` | free prose, rendered into the volatile prompt |
| `mind.idle_streak` | consecutive idle wakeups |
| `mind.fail_streak` | consecutive errored wakeups |
| `mind.next_wake_at` | ms epoch; absent = due now |
| `mind.pending_turn` | turn id of the in-flight wakeup |
| `mind.last_mirrored_id` | message id watermark for §5.2 |

Backoff: `base * 2^streak`, capped. Idle: base `MIND_WAKE_BASE_MS` (120s), cap
`MIND_WAKE_MAX_MS` (1h), streak = `idle_streak`. Failure: base
`MIND_FAIL_BASE_MS` (60s), same cap, streak = `fail_streak`; when both apply the
later of the two wins. The arithmetic is pure and threaded `now`, like
`schedules::next_run`.

## 7. Tiered rollups

The whole life in bounded space, per design/tiered_memory.md upstream: fanout
`F = 10`, forward-only, minted after settle on the cheap tier
(`CheapTier::summary`), silently skipped when the tier is absent or declines —
absence of the cheap tier is a working system.

```
mind_rollups(id INTEGER PK, session_id, tier, first_step_id, last_step_id,
             summary, created_at)
```

- Tier 1 covers `F` raw steps; tier `k` covers `F` tier-(k−1) rollups' spans.
- A span is minted once and never re-minted; coverage is tracked by
  `last_step_id` per tier, so minting is idempotent across restarts.
- Rendered oldest→newest, coarsest tier first, with step-id ranges kept in the
  text: the tiers are an index into the raw log, not testimony. The raw steps
  stay one `bough tags sql` away.

## 8. Surfaces

- `GET /sessions/:id/mind` → enabled, persona, streaks, next wake, step count.
- `POST /sessions/:id/mind` `{enabled?, persona?}` → 400 on a non-mind session;
  enabling stamps `next_wake_at = now`.
- CLI: `bough mind new [--workspace DIR] [--persona TEXT] [--title T]` ·
  `list` · `start ID` · `stop ID` · `status ID`. All through the server.
- TUI: a mind session renders as an ordinary conversation; no dedicated surface
  in v1.

## 9. Non-goals (v1)

- No multi-thinker dispatcher: one mind loop per session (Headlong's monolith);
  the responder role is the ordinary message-answers-turn path.
- No chat bridges, no identity interview, no self-forking.
- No sandbox added: a mind edits its workspace with the session's full authority,
  like every other session. Point it at a checkout you are willing to review, or
  one you do not mind it owning. The warning belongs in `bough mind new` output.
- No spend metering beyond the backoff and failure ceilings.
