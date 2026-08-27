# Bough — Requirements

One harness: resident lane-agents in a TUI over a single append-only ledger. Interface cutover at
Phase 3 (one full real workday through the new TUI); the harness completes over Phases 4-8 while the
old feeds bridge the gap. What Phase 3 gates is the interface, not the full harness.

**Architecture stance: everything is a plugin, except the center.** The center is a domain-blind
kernel plus the boot path that composes plugin rows from config. Ledger, projection, agent loop,
models, tools, collectors, wards, scheduler, memory governance, the write-boundary executor, the
TUI: every one of them is a plugin row that a config patch can disable, reconfigure, or replace
without touching another row. This is the DeepSeek Harness (dsh, Aug 2026) shape, ported to Rust
with the compile-time consequences stated plainly in section 0. Sections 1-8 and 10-12 are product
requirements; each names the seams and plugins that carry it.

---

## 0. The center, and the plugin rule

### 0.1 What is NOT a plugin (the center, exhaustively)

1. **`bough-kernel`**: the Cordis-shaped kernel. Contexts, typed service keys, fibers with a
   dependency-gated lifecycle, revertible effects, typed events with four dispatch modes, isolate /
   intercept, per-agent scope, the config-tree loader with id-keyed patch layers, and the runtime
   invariant runner. It has NO domain vocabulary: no ledger, agent, model, tool, or mail type lives
   here. The kernel is the port of the Cordis core library + loader (paper §5) with Rust-native
   typing; the semantics it must reproduce are listed in 0.3.
2. **`bough` (the launcher)**: resolves a profile, stacks bundle patch layers, applies the user
   patch file and `--patch` overlays, mounts the tree, asserts every enabled row loaded AND
   activated (fail loud, naming each unresolved service), watches the user patch file for live
   recomposition, and prints the composed tree on `--dump-config`. It owns no behavior of its own
   beyond composition and teardown-before-exit (the TUI must restore the terminal on a boot failure).
3. **`bough-util`**: branded ids, home paths, timeouts. A library; no `ctx` key.

Everything else is a plugin row in a bundle. The base bundle (`bough-base`, a YAML patch list, not
code) is the composition, and every row in it is individually replaceable.

### 0.2 The plugin rule (the DeepSeek Harness rules we adopt verbatim in spirit)

- **Plugins, not loop changes.** New behavior attaches to a documented extension point (a service
  key or a typed event). Changing the agent loop itself requires updating this document.
- **A capability is a SEAM with three roles**: a **Service Definition** (the trait + vocabulary
  types owning a `ctx.<key>`; never an implementation), one or more **Service Providers**, and one
  or more **Consumers** (what agents and other plugins program against; commonly a model-facing
  tool). One role alone is not a seam. Consumers depend on Service Definitions, never on concrete
  providers. Don't split preemptively: a capability with one conceivable provider and one Consumer
  stays one crate until a second appears.
- **Registrations are effects.** Every contribution (service, listener, tool, prompt section,
  collector, pane, ward) goes through `ctx.effect()` / `ctx.on()` and returns a disposer. Unloading
  a plugin unwinds its effects LIFO and leaves the tree as if it had never mounted. A plugin whose
  disposal does not reach quiescence (kills issued but not awaited) is a bug.
- **Activation is service-driven, not ordered.** A plugin declares `inject: [keys]`; its fiber
  stays PENDING until every key is provided by an ACTIVE fiber, activates when they arrive, unloads
  when one withdraws, and reloads when a key changes provider. Row order in a bundle carries no
  load semantics; it is for readers.
- **Model-visible ⟺ ledgered.** Anything that reaches a model request must be reconstructible from
  the ledger; a runtime invariant asserts it. A new model-visible input requires a new step type,
  never a side channel. (Section 3 already demands this; the kernel now checks it.)
- **Events are the extension points**, in three domains: DURABLE ledger events (facts appended
  and broadcast; use one when the fact must survive restart), LIVE agent events (`agent/*`, carry a
  live agent handle; observe or intercept work in flight), and CAPABILITY events (`tools/*`,
  `actions/*`, `mail/*`, `projection/*`; attach policy to a seam without importing the loop). Each
  event declares its dispatch mode as part of its public contract.
- **No hardcoded tunables in plugins.** Deployment-varying values are validated config fields
  changeable from the bundle patch; a `DEFAULT_*` constant is not configurability. Protocol
  constants and security invariants stay fixed in code.
- **Misconfiguration fails loud** at load when self-contained, else at the earliest resolvable
  point; never silently skip a missing referent. A patch naming an absent row id is a warning; an
  enabled row that never activates is a boot failure.
- **Every plugin crate owns an `invariant` module** that checks an authoritative event stream or
  data relation it owns over time (not service presence, not plugin metadata), or states
  `No runtime invariant:` with a reason. The kernel runs them in dev and test profiles. The
  shape, by example: strict seq growth and wake/step enclosure (ledger); same-step tool
  call/result pairing (tools); non-repeating agent status and terminal disposal (agents);
  carrier presence on every scoped dispatch (scope); the sent request reconstructs from the
  ledger (agent-loop); intent-before-done on every journal row (actions).
- **Explicit at package boundaries.** Defaulting is an explicit `resolve(request) -> Spec` step in
  the owning provider, never a hidden `?? default` inside `run()`. Opaque cross-boundary ids are
  branded types, never bare strings.

### 0.3 Kernel semantics the port must reproduce (from Cordis 4 / the paper)

- `ctx.effect(f)`: `f` runs and yields inverses; the returned disposer fires at most once, halts an
  in-flight effect at its next yield boundary, and is prepended to the owning fiber's accumulator
  (LIFO recovery). Nested plugin mounts are effects of the parent, so unloading a parent cascades.
- Services: `provide(key, value)` is an effect; `get(key)` reads the store; declared injections
  resolve against the fiber's COMMITTED view (bindings captured at activation), so a plugin reads
  the same providers for its whole life, teardown included. Access to an undeclared key is an
  error at the point of use (capability-style: a plugin reaches only what it declared).
- Fiber lifecycle: PENDING → LOADING → ACTIVE → UNLOADING → INACTIVE | FAILED. Transitions are
  INERTIAL: a reload or unload runs to completion before the fiber responds to a new target.
  A provider entering UNLOADING stops providing BEFORE any inverse runs, so dependents recompute and
  tear down first; unload awaits notified dependents before recovering its own effects.
- Targets are identified by provider fiber uid, not value: replacing a provider reloads dependents
  even when the new value is equal; a provider overwriting its own binding in place is not observed
  (withdraw and re-provide to propagate).
- Events: `emit` (fire, no return), `waterfall` (around-middleware; a listener receives `next` and
  MUST call it to delegate, returning without it short-circuits; `prepend: true` only when it must
  run first), `parallel` (awaited fan-out, no return), `serial` (awaited in order, first return
  wins). Listener exceptions are contained in the dispatcher: one bad subscriber never breaks the
  loop.
- `isolate(key, realm)`: a child context resolving `key` to an independent binding; entries sharing
  a realm label share the binding. `intercept(key, metadata)`: per-context metadata a provider
  consults on use (path allow-lists, rate policy) without affecting satisfaction; changeable at
  runtime without reload.
- Loader: the config tree is a list of ENTRIES `{id, plugin, config, disabled, isolate, inject}`
  plus `group` (a list of child entries) and `include` (an external YAML file grafted in). Field
  changes reconcile incrementally: `plugin`/`id` rebuilds, `config` is handed to the plugin (which
  reloads only on a material diff), `disabled` unloads/reloads, `isolate` reassigns realms. The
  quiescent state is a function of the final config alone, whatever order the changes arrived in.
  A failed candidate leaves the last good tree running and broadcasts `config-update-failed`.
- Scope (dsh `core/scope`, not Cordis proper): `create_scope(ctx, key)` mints a tagged context whose
  registrations are scope-visible AND scope-lifetime; `scope_target(base, key)` routes an event to
  untagged listeners plus the subject's own. Keys form an optional parent chain: views inherit
  DOWN (nearest shadows farthest), event admission extends UP. Scopes route trusted in-process
  plugins; they are not sandboxes or authority boundaries.

### 0.4 What "plugin" means in Rust, honestly

- **No code hot-swap.** The plugin CATALOG is compile-time: every plugin crate registers its name
  and constructor through `inventory`. The COMPOSITION is runtime: which rows mount, with what
  config, isolated how, is config, reconciled live when the patch file changes. "Swap a provider"
  = edit a row and save; no rebuild, no restart. Adding a plugin that isn't in the binary IS a
  rebuild; that is the Rust cost, accepted. dsh's HMR of module code has no analogue here and the
  dev loop uses hot-lib-reloader (section 13) for TUI iteration only.
- **Runtime-code plugins are rows too.** Skills, rhai wards, executable hooks, and subprocess MCP
  plugins mount through HOST plugins (`wards-rhai`, `hooks-exec`, `mcp-subprocess`), each of which
  is an ordinary compiled row that grafts a child entry per file/process under its own group. Hot
  reload of a ward is a config-tree reconciliation of one child entry, not a special mechanism.
- **No self-modification by agents.** dsh lets the model inspect and mount plugins into its own
  runtime. Bough does not: structure is proposed by the system and made real only by Andrey
  (section 16). Agents may PROPOSE a ward or a config change as a claim; mounting is his edit to
  the patch file.
- **Not taken from dsh**: web UI, npm-style plugin distribution, telemetry, the Code Mode SDK,
  multi-product subagent providers (Codex/Claude Code children), per-session preset directories
  (section 5 scope covers the need with far fewer moving parts), compaction-by-surface-replace
  (tiers + digests are Bough's answer to context pressure). Where Bough goes further: dsh has no
  idle eviction ("nothing disposes an agent"; an open TODO there); dormancy (section 1) plus the
  per-wake fresh context is the design here, and it is why residents are cheap.

### 0.5 Profiles, bundles, patch layers

A running `bough` is a plugin tree composed from ordered layers over an empty root:
each bundle's patch in the profile's order → the profile's `bough.patch.yml` → the home-level
`~/.bough/bough.patch.yml` → `--patch` overlays. A patch targets a row by id and REPLACES its whole
`config` (restate kept fields; no deep merge) or `insert`s rows. `bough --profile tui --dump-config`
renders exactly what boots, annotated with which layer last wrote each row. The known cost of
layered composition is opacity during incident response (the effective runtime is profile order
+ row replacement + `disabled` expressions + activation); the two mitigations are that dump, and
a COMPOSITION FINGERPRINT (hash of the composed tree) recorded in every `request/header` step,
so any wake in the ledger can be explained by the tree that was live when it ran. Config
validation is synchronous and pure: a check that needs I/O belongs in the plugin's `apply`, not
its validator.

Bundles: `bough-base` (ledger, projection, agent seam + default loop, models, tools, actions,
collectors, memory governance, workers, wards host, scheduler; the shared core of every profile),
`bough-tui-app` (the terminal surface), `bough-headless` (one-shot task runner for tests and
scripts; no TUI). Profiles: `tui` (default), `headless`, `dev` (adds invariants-on, hot-lib
reload, the golden-projection recorder). Config expressions: a row's `config` and `disabled` may
carry `!!expr` (env lookups, `home_path(...)`, platform tests), evaluated at mount; nothing else in
an entry is dynamic.

## 1. What it is

Resident lane-agents (however many the work currently warrants; the count breathes) plus one
leader. An agent is logically continuous (identity, memory, initiative) and physically ephemeral:
every wake is a fresh context projected from its trajectory. Agents can go **dormant**: a dormant
agent costs nothing (no ticks, no wakes), keeps its keep and routing intact, and reactivates only on
Andrey's message or wake-class mail (section 5); ordinary events queue silently. Dormancy is a
state, not an archive; lanes that sleep for weeks are normal.
Talking, delegating, and steering all happen in one chat box per agent. Reactive one-shots, heavy
worker fan-out, and fork branching (section 4) are supported throughout; the residents add
initiative between turns and continuity without re-derivation.

## 2. Agents (seam: `ctx.agents` + `ctx.agent_loop`)

- **Resident lane-agents**, one per lane, named by their lane.
- **Voice: professional colleague.** First person, plain engineer register, zero roleplay.
- **About-line: self-maintained, two halves.** The "state" half is evidence: each refresh cites the
  steps it summarizes. The "intent" half is a self-declared thought, displayed AS intent (labeled),
  never rendered as truth. Refreshed at every wake/turn, stored as a step, shown in the strip.
  Implemented as the `about-line` plugin: a listener on `wake/end` (completed wakes only), not
  loop code.
- **The leader**: the one cross-lane agent. Owns unsorted adoption, requirement drafting from
  Andrey's words, merge/split/bud proposals, the reconsolidation pass, the cross-agent timeline.
  Only the leader proposes structure changes; lane-agents never self-advocate. The leader is an
  ordinary agent row with the `leader` plugin set mounted in its scope (section 5); nothing in
  the loop knows the word "leader".
- Andrey retains direct command over everything; agent proposals ride the claims gate (accept / edit /
  reject), and acceptance is always his act.
- **The Agent seam.** `agents` (Service Definition) owns the `Agent` handle, the live registry,
  the initiator scope, and the `agent/*` event vocabulary: inbox, wake, step, status, request,
  preempt, continuation. It contains zero loop logic. `agent-loop` (Provider) is the default
  driver: the wake loop of section 5, and the ONLY crate with concrete loop code. It registers
  itself through a factory slot (`ctx.agents.set_factory(..)`, which throws if one is already
  set), so consumers call `ctx.agents.create()` / `resume()` and never import the loop. UI,
  wards, tools, and the about-line depend on `agents`, never on `agent-loop`, so the driver is
  swappable: the test profile mounts `agent-loop-scripted` (replays a fixed transcript through the
  same seam) and every consumer must keep working. Substitution is structurally easy and
  semantically demanding: a replacement loop must honor the ledger step protocol (section 5's
  wake flow), the tool pipeline, scope disposal, and the wake-stopping rule; the ledger and
  agents invariants are what hold a replacement to that, so they run against every loop
  provider, not only the default.
- **The handle.** `Agent { id, session, inbox, status: idle | running, ctx (the agent's scope),
  cancel(cause, keep_inbox?), when_idle(), send(msg, target, wake) }`. Two inbox targets,
  `next-wake` and `next-step`, times a wake flag, give three presets: `followup` (next-wake +
  wake: the sole ordinary message of its own wake), `steer` (next-step + wake: an idle agent
  starts a wake, a running one consumes it at its next step boundary), `inject` (next-step, no
  wake: context that waits until something else wakes the agent). Every inbox mutation is a
  durable step (`inbox/spliced`); the message id identifies insertion, claim, and discard facts,
  never a later output. Cancellation causes are typed (`user | parent | hook | disposed`); the
  first cause wins; a cancel with nothing active is a no-op and never arms later work; a
  `disposed` cancel never latches a pending wake. Creation is a transaction: private session,
  concrete agent, scoped context, an optional `setup(agent_ctx)` that composes the agent's scoped
  world while both ids are still unpublished, then registry entry and `agent/created`; a setup
  failure rolls the whole thing back. The returned disposer is a CAPABILITY: only its holder can
  tear the agent down. Teardown order: stop and drain → unwind scope → detach agent → detach
  session. Delegation is NOT a method on `Agent`: worker providers (section 10) create or drive
  ordinary handles through the same factory API, so delegation transports stay outside the core
  interface. The initiating agent rides an ambient task-local scope for attribution (mail
  routing, journal rows); ambient presence is never authorization, and explicit identity stays
  authoritative at service, worker, process, and persistence boundaries.

## 3. The ledger (seam: `ctx.ledger`; one store, append-only)

SQLite master, file views projected on demand. `ledger` is the Service Definition (append API,
read API, step/rollup/edge vocabulary, the two entry classes, invariants); `ledger-sqlite` is the
production Provider; `ledger-memory` is the test Provider (proves the seam and makes projection
golden tests fast). Roughly:

```
steps    (id, traj_id, seq, at, wake_id, type, body, cites)   -- cites: JSON array of {ref, url}
edges    (child_traj, parent_traj, at_seq, kind)        -- ancestor | merge
rollups  (traj_id, kind, tier, from_seq, to_seq, src_trajs, body, notable_refs, prompt_ver, sealed_at)
         -- kind: tier | digest | reconciliation. Digests are NOT contiguous-segment tiers: an
         -- inheritance digest summarizes the parent chain FOR a child (src_trajs names it); a
         -- reconciliation digest spans two parents. Digest "rebuild" = supersede (new row;
         -- superseded_by is the ONE permitted set-once write to a sealed rollup row), never
         -- re-summarizing tiers. "Identity" in the projection is not stored: it renders from the
         -- agents row + digest + the about-line's state half.
actions  (id, wake_id, idem_key, kind, payload, status)   -- the outward-action journal (section 7)
agents   (name, traj_id, routing_refs, wake_classes, model_override, tick_floor, digest_rollup_id)
         -- model choice is by WAKE TYPE (section 12); model_override optionally replaces the terra
         -- default for an agent's unattended wakes. sol-for-answering-Andrey is not overridable.
         -- agents is MUTABLE CONFIG, explicitly exempt from append-only (which governs the ledger
         -- of record: steps, edges, rollups). Merge: Andrey picks the surviving row; routing_refs
         -- union; overrides from the survivor; the losing ROW is deleted, its trajectories remain.
step_refs(step_id, ref)   -- CANONICAL for matching/routing; derived from cites + body refs at append
```

- **Two entry classes**: cited evidence and marked thoughts. Anything rendered as truth (about-line
  state halves, timelines, answers) comes from evidence; thoughts never promote without it. Steps
  carry a wake_id; seq is allocated atomically by the single writer, and the projector de-interleaves
  concurrent wakes by wake_id.
- **Step types are a merge-extensible map.** A plugin that needs a new durable fact declares a new
  step type (with a schema) and renders it from the log; the ledger Definition owns the envelope,
  plugins own their types. A step type unknown to the running binary is refused on read unless it
  carries `ignorable: true`. Only structural envelope changes bump the ledger format version.
- **Durable events.** Every append broadcasts `ledger/step` (emit, post-commit) with the step;
  `wake/start`, `wake/end`, `step/start`, `step/end`, `request/header`, `inbox/spliced`,
  `mail/delivered`, `rollup/sealed`, `pin/*`, `claim/*`, `action/intent`, `action/done` are step
  types, not side channels. Fork, resume, projection, timeline, FTS, and the TUI all derive from
  this stream. Append is a synchronous commit on the single writer; observers never block it and
  observer failures are contained per listener. A fork requires its prefix to end outside an open
  wake and rejects one that does not, instead of clipping silently; the child's first live step
  is an `end-seed` marker so seed history and live work are never byte-identical.
- **Sealed rollups**: a raw segment is summarized exactly once (map over episode windows cut at time
  gaps, reduce to themes), stamped with the prompt version, immutable after. Tiers are an index:
  every block carries refs into the raw beneath it. Fanout ~10; tier k covers ~10^k steps.
- **Pins**: a step type that rides every projection verbatim regardless of age: never demoted into
  tiers, never expired by reconsolidation. The relief valve is SUPERSESSION: a pin may retire an
  earlier pin (appended marker, projector honors it); re-accepting or editing a requirement
  supersedes its old pin. Accepted requirements are pins.
- **Membership is derived, never stamped.** An agent is one head pointer; connected(agent) =
  own_chain ∪ ancestry ∪ ref_matches, computed at need. Linking a ref late includes its history
  retroactively at no cost, because inclusion is never written onto entries. Mail delivery is the one
  eager step: a matching event FANS OUT to every agent whose refs match (consumption is per-agent,
  so multi-delivery is cheap and a true owner is never stranded by a misroute elsewhere); zero
  matches route to the leader's unsorted queue. Misroutes stay recoverable via refs.
- **Ledger invariants** (the crate's `invariant` module): append-only on steps/edges/rollups (the
  schema forbids UPDATE/DELETE; the invariant additionally asserts no row hash changes across a
  session), seal-once, the model-visible ⟺ ledgered check (every projection section cites step or
  rollup ids that exist).

## 4. Graph ops (plugin `graph-ops`; malleable lanes)

A Consumer of `ledger` and `agents`; nothing in the ledger Provider knows about splitting.

- **Split**: append a cited split event, create two heads with ancestor edges, write one inheritance
  digest per child (the only LLM cost), reassign routing refs. The past is never partitioned.
- **Merge**: one new head, two edges of kind `merge` (distinguishing them from birth ancestry), one
  reconciliation digest. Both sides' sealed tiers stay valid. The head ALSO carries an `ancestor`
  edge to each parent, because `connected()` derives membership from ancestry alone and a head
  joined only by merge edges would read neither past; the `merge` edges remain the fact of the
  merge, and any reader that means "birth" must exclude a child that has one.
- **Bud (retroactive birth)**: a split whose point is in the past; the parent never pauses.
- **Fork** (interactive branching): a bud with no agents row and no routing: a headless scratch
  branch for exploration, promotable to an agent later by adding the row.
- Undoing an unused split is pointers; undoing a lived-in one is a merge (reconciliation digest,
  rerouting, divergent heads left behind). Ambiguous routing becomes a leader question, never a
  guess. "Ambiguous" is a ref two children BOTH claim; a ref neither claims is not a tie and stays
  with the parent, whose row survives the split rather than becoming a black hole.

## 5. Scheduling and activation (plugins `mail-router`, `wake-scheduler`, `preemption`, `projection`)

- **Laptop hours only.** No overnight runs; on lid-open each active agent does a catch-up wake over
  queued mail. Dormant agents skip ticks entirely; mail queues silently EXCEPT wake-class items,
  which reactivate them: Andrey's messages always, plus per-agent configured classes (asks, mentions
  of Andrey, review requests). Ordinary events (pushes, CI, state changes) never wake a dormant
  agent; that split is what makes dormancy cheap.
- **Mail consumption is a set, not a high-water mark.** Each wake ends by appending a wake_end
  marker listing exactly the mail seqs it processed (ranges allowed); consumed = the UNION of all
  wake_end sets. Union is order-independent, so concurrent wakes (an answer wake finishing before an
  interrupted thought's jot) can never regress consumption, and an answer wake that reads only the
  triggering message consumes only that: queued ordinary mail stays unconsumed for the coalesced
  drain wake. Consumption is per (agent, seq) and applies only to DELIVERED mail: foreign entries
  read through connected() are query results, not mail, and have no consumption state; a late-added
  routing ref starts mail delivery from link time, with earlier history reachable by query, never
  queued as backlog.
- **Wakes, two urgencies.** IMMEDIATE: Andrey's message and wake-class mail wake the agent now.
  COALESCED: ordinary mail schedules a debounced drain wake (minutes, config) unless some other wake
  happens first and drains it: mail-driven, so draining works from Phase 3 with no tick machinery.
  Plus own scheduled intents and system passes. Idle ticks (Phase 7) add exploration on top;
  agents.tick_floor overrides the config default when set. A jot resumes at the next wake of ANY
  kind, not specifically a tick. Standing invariant, checked at every wake_end: unconsumed ordinary
  mail present implies a drain wake is scheduled: this is also what drains a reactivated dormant
  agent's backlog. An Andrey message ALWAYS gets a fresh sol answer wake, whatever queue it arrived
  through; drain and tick wakes never answer him. One drain wake in flight per agent, and drain
  consumes only ordinary-class seqs while answer wakes consume only their wake-class trigger, so
  concurrent wakes touch disjoint mail.
- **Preemption: checkpoint-and-answer, in parallel.** Andrey's message starts its answer wake
  IMMEDIATELY; the interrupted thought gets one grace step to jot state concurrently, and the jot
  lets the next wake of any kind resume (a preempted wake skips its about-line refresh: refresh
  happens on completed wakes only). His latency never waits on the jot. The answer wake is not blind to the
  in-flight work: thought steps append as they are produced, so the projection includes everything up
  to now; only the not-yet-jotted final state is missing. One answer wake per agent at a time: a
  message arriving during an answer wake joins that wake if it has not started responding, else
  queues as the next wake's first mail; "started responding" means the first reply token has
  streamed. (Scope of the latency promise: a message never waits on a JOT; it may wait on an
  in-progress answer to a previous message.)
- **Idle ticks may**: patrol the lane, explore/connect (output = claims), gather context via MCP,
  and advance the work.
- **Wake flow, and where plugins attach.** A WAKE is a turn: it opens when input is claimed and
  closes when nothing is owed. A STEP is one model request plus the tools it calls.

  ```
  -> agent/wake-request            waterfall over ADMISSION, dispatched by every loop Provider
                                   immediately BEFORE the wake is opened: open | defer{by,reason}.
                                   A deferral means the wake never exists — no wake/start, no
                                   claim, no step — which is what §1's "a dormant agent costs
                                   nothing" requires; agent/pre-step is too late, because it
                                   rejects inside an already-durable wake. Default with no
                                   listener is open. (Added in Phase 5 by `dormancy`.)
  wake/start                       (durable)
    claim trigger + queued mail per the urgency rules above (a pure deletion splice from the
    inbox; between steps only next-step input is claimed)
    -> agent/pre-step              waterfall: reject | enter(messages); a rejected or emptied
                                   first claim still closes a durable wake that spent no step;
                                   claimed messages the decision omits stay removed
    step/start                     (durable)
    -> projection/assemble         waterfall around the assembler: sections may be added, budget
                                   policy may degrade (below); output is the exact request
    request/header                 (durable, only when it changes: prompt version, section ids,
                                   tool schemas, call config: every request is a pure function
                                   of the ledger)
    -> agent/request               waterfall over the CALL CONFIG only (model policy, section 12;
                                   token metering); it cannot mutate messages: model-visible
                                   content must arrive through logged channels
    -> llm/stream                  waterfall: chunks append as thought steps as they are produced
    tool calls -> tools/pre-execute -> tools/execute -> tools/post-execute   (all waterfalls)
    step/end                       (durable)
    on a failed model step: -> agent/request-error   waterfall; a listener that owns recovery
                                   returns retry without next() (llm-retry, overflow repair);
                                   the default leaves the failure terminal for this wake
    tools owe another request, or next-step input arrived -> next step
  -> agent/wake-stopping           serial: a listener that OBJECTS steers (agent.steer(..)) and
                                   the driver re-reads its inbox; fresh steering runs another
                                   step, none closes the wake. Data decides, so listener order
                                   cannot change the outcome. The inverse is data too: a tool
                                   result carrying concludes_wake ends the wake at its step.
  wake/end                         (durable; reason: completed | aborted{cause} | error |
                                   max-tokens | interrupted; carries the consumed-seq set and
                                   the about-line refresh)
  ```

  `agent/pre-step`, `agent/request`, `agent/request-error`, `llm/stream`, and the three `tools/*`
  events are waterfalls whose listeners must call `next()` to delegate. `interrupted` is the one
  reason no loop emits: crash repair synthesizes it when it closes an orphaned trailing wake at
  boot (it also synthesizes `TOOL_OUTCOME_UNKNOWN` results for calls without results, and never
  touches rollups). There is no built-in wake budget: bounding a runaway wake is a plugin
  cancelling from `agent/wake-stopping`. A plugin failure ends the current wake, not the loop.
  `status` is `running` for the driver-wide drain interval and may span consecutive queued
  wakes; it is not proof a wake is open, so automation that owns a run defines its interval from
  its message's durable inbox receipt through the next whole-agent `idle`.
- **Context = projection** (plugin `projection`, a seam: Definition `ctx.projection`, default
  Provider `projection-assembler`, Consumers the loop and the TUI preview pane): a deterministic
  assembler (no LLM in the request path), specified, not just named: fixed section order
  (identity, pins, digest, tiers coarse-to-fine filtered by the agent's refs against notable_refs,
  verbatim tail, unconsumed mail); plugins contribute sections through `ctx.projection.section()`
  (an effect, global or per-agent scope) with a declared position; under budget pressure it
  degrades in fixed reverse order: drop fine tiers first (keep coarse), then shrink the verbatim
  tail to a floor; pins, digest, and mail headers degrade LAST and never silently: pin overflow
  collapses pins to titles+count with an in-context DEGRADED flag (an answer wake must always be
  buildable, leader or no leader; supersession is proposed when the leader exists), and mail-header
  overflow collapses to per-class counts + newest N (safe: the drain wake processes the full queue
  regardless). Token budget applies a 0.6 headroom factor initially (third-party measurements put
  Claude-family tokens at ~1.5-1.7x o200k on code); recalibrate against own trajectories in Phase 1.
- **Per-agent scope.** The loop mints one scope per live agent (`agent.ctx`); the leader plugin
  set, a lane's extra tools, its persona section, and `tools.restrict` (an intersection filter over
  the GLOBAL tool set) register through it and unwind with the agent. Shadowing is most-specific-
  wins: a scoped tool or section replaces its same-named global twin for that agent alone. Two
  levels, flat: scoped registrations never inherit down to workers; worker behavior is data
  (delegation depth), never scope structure. A filtered-away tool is absent from the prompt AND
  refuses execution, indistinguishably from a nonexistent one.

## 6. Context acquisition (plugins `collector-github`, `collector-linear`; seam `ctx.mcp`)

- **Central collectors sweep** (GitHub, Linear; deduped, cited, watermarked) into mailboxes: the
  cheap baseline. Each collector is one plugin row scheduled through `ctx.schedule` (section 9);
  disabling a collector is a row patch. Collectors are Consumers of `ledger` (they append mail) and
  Providers of nothing: there is no "collector seam" until a second GitHub backend exists.
- **Agents may pull ad hoc via MCP during wakes** with no server restrictions (all granted servers,
  all tools; the section 7 messaging rule is instructional, see there). Pull results enter the
  trajectory as cited evidence. `mcp` is a seam: Definition (client vocabulary, tool listing),
  Provider `mcp-rmcp` (stdio/HTTP clients from config rows, one child entry per server), Consumer
  `tool-mcp` (registers every discovered tool on `ctx.tools`).
- `bough mcp call <server> <tool> <json>` CLI stays (a human command on `ctx.commands`, section 11).

## 7. Write boundary (seam `ctx.actions`)

Autonomous, no button:
- Open PRs. Push commits to PRs Andrey authored that are open (never teammates' branches).
- Reply to / resolve / close BOT review threads.
- Linear: status changes and comments on tickets; creating tickets stays Andrey's.

Never autonomous: Slack messages as Andrey, ticket creation, and every other team-visible act not
listed above. Enforcement is split, and every other section defers to this sentence: ward-emitted
actions are CODE-enforced (the harness executes them in one place, section 9); direct agent tool
calls are INSTRUCTIONAL only: agents keep full tool control (all granted MCP servers, all tools) and
the boundary lives in their standing instructions ("never send messages as Andrey; surface a draft
instead"). Chosen deliberately, flagged: a prompt-level rule can be violated under long contexts;
no interception layer exists to catch it. Agents open PRs as Andrey's account,
so any agent-opened PR is immediately "authored + open"; the push rule's real force is only "never
teammates' branches" (accepted as stated).
No sandbox, no spend caps; agents run as Andrey on this machine.
Guardrails that DO exist: modest spawn bounds by default (max workers in flight ~8, chain depth 3,
per-wake spawn cap; all config knobs), per-plugin disable-on-failure (a row whose fiber FAILS is
reported, not retried into a loop; `disabled: true` by patch is the manual off switch), freshness
and dedupe guards.
**The four sanctioned outward acts are HARNESS PRIMITIVES** (open_pr, push_to_pr, bot_thread_op,
linear_write), and the seam is what makes the boundary code: `actions` (Definition) owns the
action vocabulary, the idempotency journal, and the `actions/execute` waterfall; `actions-github`
and `actions-linear` are Providers registering exactly the four kinds; `tool-actions` is the
Consumer that registers them on `ctx.tools`, so the natural path for agents and workers IS the
journaled one. The Definition's executor is the single place the action set is enforced: a kind
not registered by a Provider does not exist, and the section-9 ward executor emits only through it.
Each primitive writes an intent row with an idempotency key BEFORE executing, marks it done after,
and embeds the key in the artifact itself (PR-body marker, commit trailer, comment suffix), so
intent-without-done reconciliation after a crash is a lookup against the world, never a blind
re-execution. idem_key = hash(action kind, canonical target, triggering step id): concurrent wakes
double-processing the same mail collide in the journal instead of duplicating. Raw-tool equivalents
(bare gh, raw MCP writes) remain physically possible and are INSTRUCTIONAL RESIDUE: standing
instructions say use the primitives, and the Phase 8 no-duplicates guarantee is scoped to journaled
actions. Bot-thread classification: GitHub's Bot account type plus a known-bot allowlist; anything
uncertain is treated as human, and human threads are never auto-resolved. Collector-side dedupe does
not cover any of this; the journal does. Enforcement lives in the operation that makes the
decision: schema omission and prompt filtering are not enforcement; the test denies through the
executor.

## 8. Memory governance (plugins `rollups`, `reconsolidation`, `drift-watch`; non-optional)

- **Reconsolidation pass** (a system function; leader-attributed once the leader exists in Phase 5,
  runnable as a command from Phase 4): batched distillation, contradiction detection
  (conflicts surface as claims), stale-evidence expiry with a note. Never silent edits, and never
  edits at all: distillation only ADDS blocks, and expiry is an APPENDED marker the projector honors;
  sealed rows and raw steps are never modified or deleted.
- **Drift watch**: per-agent stability signals (thought-length variance, tool-use distribution;
  claim rejection rate activates with Phase 5's accept/reject surface) and a one-command reset. The reset rebuilds the agent's digest, identity, and
  about-line from raw evidence; sealed tiers are never re-summarized by it. If a tier block itself is
  suspected bad, it is SUPERSEDED: a new block appended, the old marked with an expiry note, seal-once
  preserved. The reset rebuilds the about-line's STATE half from evidence; the intent half is a
  thought and simply starts empty.
- Until the Phase 7 scheduler and lid listener exist, reconsolidation runs by manual command and
  catch-up runs at TUI launch (app start is the lid-open proxy). Governance is available from Phase 4,
  just hand-cranked.
- Fresh execution contexts per wake/worker are the primary defense; tiers-as-index and citations are
  the second; reconsolidation is the third.
- These three rows are in `bough-base`. They are plugins like any other, disable-able by a patch,
  which is Andrey's act; what a RUNTIME SCRIPT (a ward) can do is emit actions, and "disable a row"
  is not an action kind, so no script edit can switch governance off.

## 9. Plugins: catalog, hosts, and runtime-code plugins

This section replaces the old "two tiers". There is one tier (rows in the tree) and two ways a row's
code arrives: compiled into the binary, or loaded by a compiled HOST row from a file or process.

- **Compiled plugin crates** (`bough-plugin-*`): each exports a name and constructor through
  `inventory`, declares `inject`, validates its `Config` (schemars-derived), and contributes only
  through effects. Models, tools, collectors, TUI panes, the agent loop, the ledger: all here.
- **Host rows for runtime code** (no recompile, no restart):
  - **`skills`**: global pool, mention-triggered auto-injection as a projection section, one child
    entry per skill file, hot-reloaded on file change (notify + debouncer → entry reconciliation).
  - **`wards-rhai`**: PURE scripts. A ward receives the event plus a read-only context and RETURNS a
    list of actions (spawn, mark, post [a message into a lane's own chat, an internal surface,
    never outward], hint, schedule); the host executes them THROUGH THE SEAMS (`ctx.workers`,
    `ctx.ledger`, `ctx.actions`, `ctx.schedule`), which is where citations, bounds, and the write
    boundary are enforced. Scripts cannot reach the world: engine limits (max ops, max depth, no
    registered I/O, `eval` disabled) replace sandbox machinery. One child entry per ward file, hot
    reloaded via notify; `bough wards test` dry-fires a ward against past events and prints
    would-do actions.
  - **`hooks-exec`**: executables at named hook points (JSON on stdin), and **`mcp-subprocess`**:
    resident subprocess plugins speaking MCP/JSON-RPC for anything heavier, in any language,
    independently restartable. Their boundary tier, stated plainly: actions they emit THROUGH the
    plugin API are code-enforced and journaled like ward actions; anything they do directly as
    processes running as Andrey is trusted config, outside the boundary's scope: the same standing
    as any script Andrey runs himself. Flagged.
- **Scheduling is a seam** (`ctx.schedule`: Definition; `schedule-cron` Provider on
  tokio-cron-scheduler; Consumers: collectors, reconsolidation, catch-up, idle ticks, wards'
  `schedule` action). Background jobs are wards or schedule registrations; there is no third kind.
  In Phases 3-5 the OLD daemon's collectors are what runs (bridged by the `old-feed-adapter` row);
  Phase 6's collector rows register on `ctx.schedule` directly. Retiring the adapter is
  `disabled: true` on one row.
- This tier REWRITES working jungler machinery, deliberately: jungler already runs mlua wards with a
  dry-fire module, GitHub/Linear collectors, claims, and pending asks. The rhai engine, the
  collectors-as-rows, and `wards test` are ports of proven designs into the new architecture, not
  greenfield inventions; jungler's daemon retires when Phase 6 completes the port.
- Spawning rights: Andrey, agents, and plugins may all spawn workers, within the bounds above.
- Dev-loop only: hot-lib-reloader for live TUI iteration; never a production mechanism.
- **Tools are a seam** (`ctx.tools`: the scoped registry + the guarded execution pipeline).
  A tool declares its schema, its render intent (generic / terminal / diff; decided up front,
  presentation a pure function of args), and registers globally or in an agent scope. The
  pipeline is `tools/pre-execute` (decision `allow | deny{reason} | ask{reason}`; the policy
  layer; the registry's guard is MONOTONIC: a later listener cannot turn a denial back into
  permission; `ask` is serviced by `ctx.approval` when mounted and degrades to deny otherwise;
  input rewrite is deliberately not offered, or logged and rendered args would desync from what
  ran) → `tools/execute` (around-dispatch; wrappers may replace only the cancellation signal;
  deadline enforcement wraps here) → `tools/post-execute` (`accept` may replace content OR value,
  never both, and may attach additional contexts: repeat-call reminders, spill of oversized
  results to a file with a locator inline; `block` turns feedback into a valueless failure) →
  `tools/result` (emit, observe-only, immutable). A tool declares `is_concurrency_safe(args)`;
  exactly `true` permits parallel dispatch, everything else is exclusive and forms a barrier;
  only dispatch overlaps, durable results stay model-ordered. Enforcement is in the executor: a
  tool absent from a scope refuses execution there; `restrict` is visibility composition, not
  an authority boundary.

## 10. Workers (seam `ctx.workers`)

Ephemeral subagents: fresh task-only context, single report back, result lands as cited evidence in
the spawner's chain. `workers` (Definition) owns start requests, results, live runs, bounds, and the
provider registry; Providers `worker-spawn` (fresh child through the agent seam) and `worker-fork`
(child from the parent's history, one-shot, keeps the parent's request prefix); Consumers
`tool-spawn_worker`, `tool-fork`, `tool-ask`, and the ward host's `spawn` action. The SPAWNER
(Provider code, not the parent's prose) prepends the standing write-boundary block into every worker
context at spawn time, from Phase 2 onward (workers run real tasks from Phase 2; Phase 6 proves the
injection). **Worker reports do not launder thoughts into evidence**: the report seal carries
per-claim external cites; claims whose only citation is the worker's own report are recorded as
thoughts. ask() is the worker's structured question primitive: it surfaces as wake-class mail on
the spawner's lane (leader-spawned workers ask in the leader's chat) and blocks or ends the worker
per its mode. Continuable background workers (steer / follow up / collect) arrive when a use case
does; the seam's start/result vocabulary leaves room, the Providers do not implement it yet.

## 11. TUI (bundle `bough-tui-app`)

- **The surface is plugins.** `tui-shell` owns the terminal, the event loop, layout slots, and
  the composer; panes register into slots as effects (`strip`, `focus`, `trajectory`, `search`,
  `preview`, `timeline`, `drift`); each renders from ledger events and drives `ctx.agents`. A
  pane row disabled by patch simply disappears from the layout. Human commands (`/compact`,
  `/goal`-style slash commands, `bough mcp call`) register on `ctx.commands` and dispatch without a
  model turn.
- **Strip + focus pane**: agent rail with state glyphs and about-lines; the focused agent's
  chat/trajectory fills the rest.
- **Full mouse + keyboard parity**: clickable expanding tool calls, click-to-focus, wheel scroll,
  drag-select with OSC52 copy, highlighting.
- **Digging**: per-agent trajectory pane with FTS across all agents; projection preview (exactly what
  the agent would see if it woke now, byte-exact: it calls the same `ctx.projection` the loop
  does); cross-agent chronological timeline with filters.
- **Testing discipline**: exercised continuously during development with shell-use (drive, click,
  scroll, assert on screen text/colors, snapshot).
- Stack: ratatui + crossterm, tui-textarea for the composer.

## 12. Models (seam `ctx.llm`)

- `llm` (Definition): message and stream vocabulary, the adapter registration seam, the
  `agent/request` and `llm/stream` waterfalls. Providers: `llm-anthropic` (bough-llm's existing
  client, wrapped), `llm-replay` (test profile: answers from a recorded transcript), others as
  rows when wanted. `llm-retry` (backon) is a waterfall listener on `agent/request-error`, not
  adapter code. Model failures surface as terminal stream chunks, never as thrown errors, so
  consumers do not guess whether an exception came from the provider, a wrapper, or their own
  assembly; middleware and consumer defects remain thrown.
- **Model choice is policy, a plugin** (`model-policy`, a prepend listener on `agent/request`):
  - sol: any wake answering Andrey directly, whichever agent runs it: lane-agents, and the leader when
    drafting requirements from his words in conversation.
  - terra: all unattended work (idle ticks, patrol, exploration, coding, leader passes, machinery,
    summaries).
  - `agents.model_override` applies to unattended wakes only; sol-for-Andrey is not overridable.

## 13. Crates

Keep: tokio, rusqlite (bundled), rmcp, ratatui/crossterm, bough-llm (dual reqwest 0.12/0.13 stands,
see Phase 0), FTS5, termimad.

**The kernel is hand-rolled** (`bough-kernel`), typed: service keys are types (`ServiceKey { type
Value; NAME }`), events are types, config is serde. Two Rust ports of Cordis appeared the week dsh
shipped: `cordis-rs` (dshbox, 0.6.x, faithful string-keyed port, zero deps, eager sync
reconciliation, "do not use in production yet") and `cordis-core` (0.0.x, typed keys, tokio-native,
staged registrations, cleanup guarantees close to what section 0.3 asks). Both are under two weeks
old at the time of writing. Read both as reference implementations of algorithms 1-7 of the paper;
do not depend on either yet. Revisit at Phase 4: if `cordis-core` has stabilized a 0.x line for
three months, adopting it is a contained swap of one crate's internals behind the same kernel API.

Add, core: inventory (compile-time plugin catalog; linkme is a fine fallback), schemars + jsonschema
(plugin `Config` and seal schemas; compiled validators), serde_yaml (the config tree and patch
layers; the id-keyed replace/insert algorithm is ours, ~200 lines, so figment is DROPPED: its deep
merge is exactly the semantics section 0.5 forbids), notify + debouncer (patch-file, skill, and
ward hot reload → entry reconciliation), tokio-cron-scheduler (the `schedule-cron` provider), rhai
(pure emit-actions ward scripts; engine op/depth limits, no registered I/O, and `eval` DISABLED: it
is on by default), tui-textarea.
kameo is DEMOTED from architecture to an implementation choice inside `agent-loop`: with fibers
owning effects and disposal, the seam does not need an actor framework; the loop provider may still
use kameo for its mailbox and panic isolation if it earns its keep in Phase 2.

Add, supporting: backon (retry with jittered backoff), similar (diff rendering), syntect + two-face
(code highlighting in expanded tool calls), arboard for the OS clipboard plus OSC52 (crossterm's
osc52 feature, present since 0.29), tiktoken-rs `o200k_base` as the token-budget estimator WITH the
0.6 headroom factor from section 5, insta + ratatui TestBackend for TUI snapshots (sparingly; prefer
structural asserts). Composer: the ratatui org's textarea (its 0.8.0 supports ratatui 0.30; the
original tui-textarea is stuck at 0.29). sqlite-vec is kept only if bough's semantic recall ports
over; nothing else in this design uses vectors: drop it until then.
hot-lib-reloader: verify it parses Rust 2024 `unsafe(no_mangle)` before adopting for the dev loop.
Several picks (rmcp, sqlite-vec, both Cordis ports) are pre-1.0: expect churn across an 8-phase
build and pin minor versions.

Hand-roll, deliberately: the kernel (above); the macOS sleep/wake listener (IOKit
IORegisterForSystemPower as the primary source: dark wakes produce no NSWorkspace notifications at
all; NSWorkspace as fallback; either way a small FFI module on its OWN thread with a CFRunLoop, since
crossterm's event loop cannot host one; there is NO lid notification on macOS, and battery firing is
inconsistent, so TUI-launch catch-up remains the reliable baseline; it is the `sleep-listener` row,
a Provider of `ctx.power`); and the agent loop itself.

Avoid: rig, sqlx, actix, octocrab (shelling `gh` reuses existing auth; revisit only if webhooks or
no-CLI distribution become requirements), mlua/piccolo/wasm runtimes and rune (the pure emit-actions
contract makes rhai sufficient; embedded-VM isolation solves a problem this single-user harness does
not have), figment/config-rs (see above), dlopen-style plugin loading (abi_stable, libloading: the
catalog is compile-time by decision, section 0.4).

## 14. Data import (optional, not a priority)

Backwards compatibility is a non-goal, with ONE sanctioned exception: the Phase 3 old-feed adapter
that bridges the old feeds into mail until Phase 6 (throwaway by design; one row, retired by patch).
"The old daemon" is TWO databases: ~/.bough/bough.db (command_history, command_tags, note_sections)
and ~/.jungler/jungler.db (events, lane_story, nodes.summary); the adapter reads both. Beyond that,
agents may start with fresh keeps; both dbs stay on disk, queryable read-only. If an import is done
later, the cheap wins are: command memory (command_history, command_tags) reused as-is for priming,
and jungler's nodes.summary rows plus lane_story sections slotting in as tier-1 blocks. Nothing in
the build waits on any of this.

## 15. Open items

1. Whether and when to import anything old (see §14); decide after the harness is the daily driver.
2. TUI layout specifics: prototype and react, do not debate in prose.
3. Tick floor / backoff curve values: tune in daily use.
4. Reconsolidation cadence and drift-signal thresholds: start conservative, adjust on evidence.
5. Kernel: adopt `cordis-core` or keep the hand-rolled kernel; decide at Phase 4 on the crate's
   track record, not on features.
6. Granularity. dsh pays for its purity with ~100 packages. The rule here is the seam rule (split
   only when roles evolve independently) plus a hard review at the end of every phase: any crate
   with one provider, one consumer, and no second provider on the horizon folds back into its
   neighbor.
7. Event catalog gate: dsh generates an event producer/consumer map and checks declared dispatch
   modes against dispatch sites. Worth a `cargo xtask` from Phase 2 if the catalog passes ~30
   events; not before.

## 16. Standing constraints

Uncertainty never becomes assertion; every claim rendered as truth is cited. Acceptance is the
ground-truth act. The past is append-only and shared; organization is a view. Structure is proposed
by the system, made real only by Andrey. Everything is a plugin except the center (section 0.1);
a behavior that cannot be disabled by a row patch is a kernel bug or a documented exception.

## 17. Build plan, end to end

Each phase ends usable and verified before the next starts. The TUI comes early because the cutover
gate lives there. Idle initiative comes late, after memory and the write boundary are solid. Every
phase's verification includes a SWAP TEST: one row introduced in that phase is replaced or disabled
by a patch, with no compile, and the tree stays consistent (dependents unload/reload, nothing leaks).

**Phase 0 — the center.**
Crate layout: `bough-kernel`, `bough-util`, `bough` (launcher), `bough-llm` kept, plugin crates as
`bough-plugin-*` under `plugins/`; `bough-server` and jungler's HTTP surface are RETIRED: the rebuild
has no HTTP server, the TUI is the only surface. Kernel: contexts, typed services with committed
views, fibers with the inertial lifecycle of 0.3, effects with LIFO disposal, the four dispatch
modes, isolate/intercept, scope, the loader (entries, group, include, per-field reconciliation),
patch layers, invariant runner. Launcher: profiles, bundles, `--dump-config`, fail-loud with
teardown, patch-file watch. `bough-base` exists as a file with one row. The reqwest situation is
already decided by reality: the lockfile carries 0.12 and 0.13 bridged through OAuthHttpClient; the
dual-version arrangement STANDS and Phase 0 merely records it.
Verify: a hello plugin registers, injects a service, activates only when the service appears,
unloads when it withdraws, reloads when the provider is replaced by a different fiber providing an
equal value; effects unwind LIFO; a waterfall listener that skips `next()` short-circuits; a scoped
registration shadows its global twin for one key only; `--dump-config` output equals what boots;
a bad patch leaves the last good tree running; a plugin reading an undeclared key fails at the
point of use; the invariant runner reports one planted violation.

**Phase 1 — the ledger.**
`ledger` Definition + `ledger-sqlite` + `ledger-memory`; schema from section 3, append API with
evidence/thought classes and mandatory citations, pins, the merge-extensible step-type map with
`ignorable`, step_refs index + FTS, the `projection` seam with the deterministic assembler
(identity + digest + tiers + verbatim tail + mailbox, token-budgeted, sections contributed as
effects), file-view projection.
Verify: append-only enforced by schema tests and the ledger invariant, projection golden tests run
against BOTH providers, synthetic 100k-step bench keeps assembly under 50ms on sqlite; swap test:
switch the provider row to `ledger-memory` by patch and the golden suite still passes.

**Phase 2 — one resident agent, end to end.**
`agents` Definition (handle, registry, `agent/*` vocabulary) + `agent-loop` Provider: the wake flow
of section 5, sol/terra tiering as `model-policy`, checkpoint-and-answer preemption, `about-line`
plugin, `llm` seam over bough-llm, `tools` seam with the three-stage pipeline, `workers` seam with
`worker-spawn` (fresh task-only context, boundary block injected by the spawner, seal parsing via
schemars/jsonschema, result lands as cited evidence, ask() primitive), spawn bounds enforced
(workers in flight, chain depth, per-wake cap), and the `actions` seam with the four HARNESS
PRIMITIVES and the idempotency journal (workers run real tasks from here; the journal cannot arrive
later than the capability). `agent-loop-scripted` for tests.
Verify: scripted multi-wake conversation, preemption mid-thought resumes from the jot, a worker
roundtrip on a real small task, the model-visible ⟺ ledgered invariant holds across a wake; swap
test: the test profile mounts `agent-loop-scripted` in place of `agent-loop` and the about-line,
tools, and workers plugins keep working unchanged.

**Phase 3 — the TUI. Cutover gate.**
`bough-tui-app`: `tui-shell`, strip + focus panes, composer (tui-textarea), streaming turns,
clickable expanding tool calls, full mouse (click-to-focus, wheel, drag-select + OSC52), trajectory
scrollback, `ctx.commands`, catch-up at TUI launch, and the OLD-FEED ADAPTER row: a small,
explicitly throwaway reader with collector-style watermarks (a restart must not duplicate mail)
that turns jungler's events into mail and surfaces jungler's nodes.summary / lane_story rows as
interim tier-1 blocks (softening the no-tiers window before Phase 4). command_history is NOT mail:
it stays competence memory, queried for priming, never delivered (no agent should receive every
shell command as an event). The one sanctioned compatibility piece; it dies when Phase 6's
collectors replace it. Also in this phase: a basic FTS search pane (the index exists from Phase 1;
the primary interface should not be searchless for five phases).
Verify: shell-use scripts for every interaction (click expands, scroll, copy; focus switch is
verified in Phase 5 when a second agent exists), developed against the live TUI throughout; swap
test: disable the search pane row by patch while running and the layout reflows; gate: drive one
full real workday through the new TUI. Then INTERFACE CUTOVER: the new TUI becomes how Andrey
works, while the old daemon's collectors and command_history keep writing until Phase 6 replaces
them (the new harness reads them as mail meanwhile). Old bough stays writable for one week past
Phase 6 as the explicit revert path; full retirement after that. (Note: this softens the original
hard-cutover call because Phases 3-5 would otherwise run with no collectors, no memory tiers, and
stopped command history: the exact window with the fewest safeguards. Override back to hard cutover
if wanted.)

**Phase 4 — memory.**
`rollups` (recap-style summarizer: episode windows cut at time gaps, sealed blocks, prompt_ver
stamps; the tier tree; inheritance digests), `reconsolidation`, `drift-watch` + the re-project
reset command; all three as rows in `bough-base`; projection consumes tiers through the seam.
Verify: seal-once invariants, projection consumes tiers within budget, summarizer cost measured per
lived day, reconsolidation records a planted contradiction as a claim step (the accept/reject
surface arrives in Phase 5); swap test: replace `rollups` with a stub provider that seals nothing
and the projection degrades to verbatim tail without error. Kernel decision (open item 5) taken.

**Phase 5 — many agents, the leader, graph ops.**
Multiple residents + dormancy, `mail-router` routing rules, per-agent scope in earnest (the
`leader` plugin set mounted in one agent's scope: unsorted adoption, requirement drafts, claims
flow), `graph-ops` (split/merge/bud with inheritance digests), forks surfaced as a branch picker in
the trajectory pane. (The leader curates timeline DATA from here; the timeline surface arrives Phase
8.)
Verify: bud a real agent from existing content mid-history; a claim accepted in the TUI births a
lane; ambiguous mail becomes a leader question; swap test: move the `leader` set to a different
agent's scope by patch and the old one loses the tools and sections, indistinguishably from never
having had them.

**Phase 6 — context and the write boundary.**
`collector-github`, `collector-linear` rows on `ctx.schedule` (the `schedule-cron` provider arrives
here, ahead of the wards that also use it), `mcp` seam + `tool-mcp` auto-pull inside wakes, mail
routing end to end; `actions-github` and `actions-linear` Providers (open PR, push to authored+open
PRs, bot-thread reply/resolve/close, Linear status + comments), all through the idempotent actions
journal, with buttons for everything else; the old-feed adapter row goes `disabled: true`.
Verify: live sweeps populate mailboxes with cited events; the `actions` executor's kind set is
proven in code to exclude Slack sends and ticket creation (an unregistered kind is refused by the
executor; wards themselves arrive in Phase 7); worker spawn provably injects the boundary block
into every task context; the instructional boundary is PROBED, not proven: adversarial prompts find
no cheap path, and every found leak becomes a standing-instruction fix; a delegated worker opens a
real PR autonomously within the boundary, and a Slack request surfaces as a draft, never a send;
swap test: disabling `collector-github` by patch stops sweeps and its schedule registration with it.

**Phase 7 — runtime-code hosts and initiative.**
`wards-rhai` (pure emit-actions, one child entry per file, hot reload, engine limits, `bough wards
test` dry-fire), `hooks-exec`, `mcp-subprocess`, `skills`, system schedules (catch-up,
reconsolidation) as schedule rows, the `sleep-listener` row (macOS FFI), idle ticks with backoff.
Verify: an example ward dry-fires then fires live; editing the ward file reconciles exactly one
child entry (its old effects unwound, the rest of the tree untouched); lid close and reopen
produces a catch-up wake; an idle tick produces a claim, never an uncited assertion; a ward that
tries to emit an unregistered action kind is refused by the executor.

**Phase 8 — digging and hardening.**
Projection preview pane (byte-exact through `ctx.projection`), cross-agent timeline, FTS search
pane, drift dashboard, spawn-bound and failure-injection tests, an adversarial review of the full
boundary, docs, and the EVERYTHING-IS-A-PLUGIN AUDIT: a script that, for every row in
`bough-base`, boots the profile with that row disabled and asserts the tree settles (dependents
pending, nothing failed, nothing leaked) and, for every seam with two providers, boots with each
and runs that seam's suite.
Verify: the preview shows byte-exact wake context; timeline filters compose; kill -9 during a wake
loses nothing but the in-flight thought AND replays no outward action (the actions journal absorbs
the crash: intent rows without done marks are reconciled, never blindly re-executed); the audit
passes with zero documented exceptions beyond section 0.1.

## 18. Reference

DeepSeek Harness: github.com/deepseek-ai/deepseek-harness (`docs/architecture.md`,
`docs/cordis-primer.md`, `docs/glossary.md`, `packages/core/scope`, `packages/bundle/base/
cordis.patch.yml`, `AGENTS.md`). Cordis paper: github.com/cordiverse/paper, "A Programming Paradigm
for Spatiotemporal Composability" (Shi, Zhang, Cui; DeepSeek-AI / PKU; draft 2026-08-13), §5 for the
algorithms the kernel reproduces, §6.1 for the system-boundary argument behind "acquisition inside,
emission outside" (which is exactly why the actions journal, not the kernel, owns outward acts).
Rust ports: github.com/dshbox/cordis-rs, crates.io/crates/cordis-core.
