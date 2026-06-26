//// The supervisor-worker loop — the harness proper (SPEC.md §5.3).
////
//// One append-only conversation per task. Each user message starts a turn: the
//// supervisor replies with prose and/or STEP artifacts, the harness (the only
//// thing that executes) applies them inside the monty + Seatbelt sandbox, feeds
//// every result back, and gates completion on a deterministic CHECK plus an
//// adversarial review.
////
//// Output reuses `agent.Step`/`agent.Outcome`/`agent.run_json`, but emits the
//// loop's roles as phased events so the web client can render each distinctly:
//// supervisor prose is `StepPlan`, a harness step is `StepCall` + `StepExec`
//// (with exit code), a local-worker fix is `StepWorker`, and the guardrails are
//// `StepCheck` and `StepReview`. Plain `StepText` is left for notices.

import bough_core/artifact.{
  type Step, Code, Collect, Edit, Grep, Read, Run, Spawn, Tell, Write,
}
import bough_core/digest
import bough_core/nono
import bough_server/agent.{
  type Outcome, type Step as Activity, Outcome, StepAwait, StepCall, StepCheck,
  StepExec, StepPlan, StepReview, StepText, StepWorker,
}
import bough_server/clock
import bough_server/control
import bough_server/integrity
import bough_server/json_value.{type JsonValue}
import bough_server/monty_bridge
import bough_server/prompts
import bough_server/provider
import bough_server/providers
import bough_server/proxy
import bough_server/seatbelt
import bough_server/skills
import bough_server/tool_steps
import bough_server/tools
import bough_server/worker
import envoy
import gleam/dict.{type Dict}
import gleam/erlang/process
import gleam/int
import gleam/json
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/result
import gleam/string
import shellout
import simplifile

pub type Config {
  Config(
    /// Which supervisor provider to call (Anthropic or an OpenAI-compatible
    /// endpoint such as OpenRouter).
    provider: provider.Provider,
    /// Worker model name, or `None` to disable worker fixes (supervisor fixes
    /// its own failures).
    worker: Option(String),
    worker_url: String,
    max_rounds: Int,
    max_steps: Int,
    digest_limit: Int,
    fix_attempts: Int,
    /// When true, each non-empty plan pauses for human approval before the
    /// harness runs it (the plan-review gate, SPEC §5.4).
    review: Bool,
    /// When true, the agent's commands get the network (default-deny + the
    /// session allowlist) instead of being fully blocked, and a denied host
    /// pauses for human approval — the network leash (SPEC §7). When false,
    /// commands run with the network blocked, as before.
    net_gate: Bool,
    /// Sampling for the worker's fix/suggestion calls. A fast instruct-coder
    /// worker (the default) wants a low temperature for deterministic fixes; a
    /// reasoning worker (e.g. VibeThinker-3B) wants its documented decoding
    /// (temperature 1.0, top_p 0.95) — lowering it degrades that model. `None`
    /// leaves the field off so the server default applies.
    worker_temperature: Option(Float),
    worker_top_p: Option(Float),
  )
}

/// The subagent operations the engine delegates to the caller (the router wires
/// them to real sessions). Spawning is async — `spawn` returns the new child's
/// id immediately; `tell` sends a running child a message; `collect` reports a
/// child's status without blocking (its final output is delivered to the parent
/// automatically when it finishes); `pending` is true while any spawned child is
/// still running, so the harness can hold the turn open until they all report.
pub type Subagents {
  Subagents(
    // `spawn` is handed the run's `wake` subject so the child can signal this
    // process the instant it finishes (event-driven; no busy-polling).
    spawn: fn(String, String, process.Subject(Nil)) -> String,
    tell: fn(String, String) -> String,
    collect: fn(String) -> String,
    pending: fn() -> Bool,
  )
}

/// A no-op set, for runs that can't host subagents (the non-streaming path).
pub fn no_subagents() -> Subagents {
  let unavailable = "subagents are only available in a streaming run"
  Subagents(
    spawn: fn(_, _, _) { unavailable },
    tell: fn(_, _) { unavailable },
    collect: fn(_) { unavailable },
    pending: fn() { False },
  )
}

pub fn default_config() -> Config {
  Config(
    provider: provider.Anthropic,
    worker: None,
    worker_url: "http://127.0.0.1:8080",
    max_rounds: 20,
    max_steps: 120,
    digest_limit: 1500,
    fix_attempts: 1,
    review: False,
    net_gate: False,
    // Default worker is a fast instruct-coder: low temperature, deterministic.
    worker_temperature: Some(0.2),
    worker_top_p: None,
  )
}

type Exec {
  Exec(exit: Int, output: String)
}

/// Consecutive unproductive supervisor turns to tolerate before giving up with a
/// diagnostic (instead of retrying to the round budget).
const max_bad_turns = 3

type State {
  State(
    api_key: String,
    sup_model: String,
    workspace: String,
    config: Config,
    // Publishes the run's status + full activity list + context tokens after
    // each new activity. Status is "running", or "awaiting_plan" while a plan
    // is paused at the review gate.
    emit: fn(String, List(Activity), Int, List(nono.AuditEvent)) -> Nil,
    // Blocks until the human resolves a paused plan (review gate). Supplied by
    // the caller; the engine only calls it when `config.review` is on.
    await: fn() -> control.Decision,
    // Non-blocking: a message the human added to this run since the last round,
    // injected into the conversation so they can steer any agent mid-flight.
    inbox: fn() -> Option(String),
    // Non-blocking: True if the human asked to stop this run. Checked at each
    // round boundary so a freely-running turn can be halted.
    stopped: fn() -> Bool,
    // Spawn / message / collect subagents (async delegation).
    subagents: Subagents,
    // A subject this run process owns; a subagent sends to it on completion so
    // the synthesis wait is woken by an event instead of a 250 ms poll loop.
    wake: process.Subject(Nil),
    // Consecutive unproductive supervisor turns (empty / cut-off / malformed
    // tool args). Reset on any valid run_steps call; capped so a model that
    // keeps emitting garbage fails fast with a clear message instead of burning
    // the whole round budget.
    bad_turns: Int,
    // append-only Anthropic message list, oldest first. Carries tool_use /
    // tool_result blocks verbatim, so it is JsonValue rather than text pairs.
    convo: List(JsonValue),
    // Tokens occupying the supervisor's context after the latest turn.
    context_tokens: Int,
    baseline: Dict(String, Int),
    bb_dir: String,
    check: Option(String),
    check_ok: Bool,
    reviewed: Bool,
    steps_done: Int,
    // Project instructions (AGENTS.md), read once at run start.
    instructions: Option(String),
    // Instructions from any `/<skill>` the run's message invoked, appended to
    // the system prompt for this run. Empty when none named.
    skills: String,
    // A plain-language summary of this run's sandbox reach (network posture,
    // enabled capability groups, filesystem limits), built once at run start
    // and injected into the supervisor's system prompt so it reasons about what
    // it can do up front instead of rediscovering limits round by round.
    capabilities: String,
    // The network allowlist for sandboxed commands; grows as hosts are approved.
    net_allow: List(String),
    // Session-enabled capabilities (providers), layered into the run's proxy
    // config and seatbelt profile.
    groups: List(String),
    // Groups the worker suggested enabling after a denial this run (advisory).
    suggested: List(String),
    // Where the generated nono network profile for this run is written.
    net_profile_path: String,
    // activities newest-first; reversed on emit/return
    activities: List(Activity),
    // Egress events the sandbox observed this run, oldest first — published to
    // the network dock with every emit (SPEC §7).
    net_events: List(nono.AuditEvent),
  )
}

/// One full turn: user message in, control back when the supervisor has either
/// answered conversationally or driven the task to DONE (or a budget/refusal
/// stopped it). `emit` is called with the full chronological activity list
/// after each new activity, for live progress.
pub fn run_streaming(
  api_key: String,
  sup_model: String,
  workspace: String,
  config: Config,
  history: List(#(String, String)),
  user_prompt: String,
  emit: fn(String, List(Activity), Int, List(nono.AuditEvent)) -> Nil,
  await: fn() -> control.Decision,
  inbox: fn() -> Option(String),
  stopped: fn() -> Bool,
  subagents: Subagents,
  net_allow: List(String),
  groups: List(String),
  suggested: List(String),
) -> Result(Outcome, String) {
  let dir = bb_dir()
  // Owned by this run process (run_streaming runs inside the spawned run
  // process), so `process.receive` on it is legal and a child's `process.send`
  // from another process wakes us immediately.
  let wake = process.new_subject()
  let state =
    State(
      api_key: api_key,
      sup_model: sup_model,
      workspace: workspace,
      config: config,
      emit: emit,
      await: await,
      inbox: inbox,
      stopped: stopped,
      subagents: subagents,
      wake: wake,
      bad_turns: 0,
      convo: seed_convo(history, user_prompt),
      context_tokens: 0,
      baseline: integrity.snapshot(workspace),
      bb_dir: dir,
      check: None,
      check_ok: False,
      reviewed: False,
      steps_done: 0,
      instructions: read_agents_md(workspace),
      skills: skills.active_for(user_prompt),
      capabilities: capabilities_summary(config, net_allow, groups),
      net_allow: net_allow,
      groups: groups,
      suggested: suggested,
      net_profile_path: dir <> "/net.json",
      activities: [],
      net_events: [],
    )
  let #(state, rounds) = run_rounds(state, 1)
  Ok(Outcome(
    text: last_assistant_text(state.config.provider, state.convo),
    turns: rounds,
    steps: list.reverse(state.activities),
    context_tokens: state.context_tokens,
    net_allow: state.net_allow,
    net_events: state.net_events,
    suggested: state.suggested,
    groups: state.groups,
  ))
}

/// Non-streaming variant.
pub fn run(
  api_key: String,
  sup_model: String,
  workspace: String,
  config: Config,
  history: List(#(String, String)),
  user_prompt: String,
  groups: List(String),
) -> Result(Outcome, String) {
  run_streaming(
    api_key,
    sup_model,
    workspace,
    config,
    history,
    user_prompt,
    fn(_, _, _, _) { Nil },
    fn() { control.Allow },
    fn() { None },
    fn() { False },
    no_subagents(),
    [],
    groups,
    [],
  )
}

fn run_rounds(state: State, round: Int) -> #(State, Int) {
  // Honor a human stop request before doing any more work this turn.
  case state.stopped() {
    True -> #(notice(state, "Stopped by you."), round - 1)
    False -> run_rounds_active(state, round)
  }
}

fn run_rounds_active(state: State, round: Int) -> #(State, Int) {
  // Fold in any message the human added to this run since the last round, so a
  // human can steer the agent (or a subagent they've jumped into) mid-flight.
  let state = drain_inbox(state)
  case round > state.config.max_rounds {
    True -> #(notice(state, "Round budget exhausted."), round - 1)
    False ->
      case supervisor_call(state) {
        Error(e) -> #(notice(state, "error: " <> e), round)
        Ok(reply) -> {
          log_supervisor(state, round, reply)
          let state =
            State(
              ..state,
              context_tokens: reply.input_tokens + reply.output_tokens,
            )
          case reply.stop_reason == "refusal" {
            True -> #(notice(state, reply.text), round)
            False -> {
              // Echo the assistant turn verbatim (carries the tool_use block).
              let state = push_message(state, reply.assistant)
              let prose = string.trim(reply.text)
              // Prose is recorded as a plan step only when it narrates steps the
              // round is about to run (see run_steps_round below). A final
              // conversational reply is NOT emitted as a plan, so it isn't
              // duplicated against the assistant answer (`Outcome.text`, which is
              // the last assistant message).
              case find_run_steps(reply.tool_uses) {
                // No tool call → a conversational reply ends the turn, unless
                // subagents it spawned are still running (then hold and fold in
                // their results so it can synthesize). Guard the degenerate
                // case: an empty turn (no prose, no tool call, no pending
                // subagent) would otherwise end silently and read as "bough
                // never responded" — surface it with the stop_reason instead.
                None ->
                  case state.subagents.pending() == False && prose == "" {
                    // Empty / cut-off turn with nothing to act on: don't give up
                    // — nudge and retry, bounded by max_bad_turns.
                    True ->
                      recover_bad_turn(
                        state,
                        round,
                        "Supervisor returned an empty turn (stop_reason: "
                          <> reply.stop_reason
                          <> ")",
                        empty_turn_nudge(reply.stop_reason),
                      )
                    False -> settle_subagents(state, round)
                  }
                Some(tu) ->
                  case tool_steps.parse(tu.input) {
                    // Malformed run_steps args: feed the parse error back and
                    // retry, but capped so a model stuck emitting garbage fails
                    // fast instead of looping to the round budget.
                    Error(msg) -> {
                      let n = state.bad_turns + 1
                      case n >= max_bad_turns {
                        True -> #(
                          notice(
                            state,
                            "Supervisor kept sending malformed run_steps arguments ("
                              <> msg
                              <> ") — gave up after "
                              <> int.to_string(n)
                              <> " tries in a row.",
                          ),
                          round,
                        )
                        False -> {
                          let state = State(..state, bad_turns: n)
                          let state =
                            push_message(
                              state,
                              provider.tool_result(
                                state.config.provider,
                                tu.id,
                                "run_steps rejected: "
                                  <> msg
                                  <> "\nFix the arguments and call run_steps again.",
                              ),
                            )
                          run_rounds(state, round + 1)
                        }
                      }
                    }
                    Ok(parsed) -> {
                      // A valid call — the supervisor is productive again.
                      let state = State(..state, bad_turns: 0)
                      let state = update_check(state, parsed.check)
                      case parsed.done && list.is_empty(parsed.steps) {
                        True -> handle_done(state, round, tu.id)
                        False -> {
                          // This round narrates and runs steps: record its prose
                          // as the plan (an empty StepPlan still marks the
                          // boundary so consecutive run_steps calls stay separate).
                          let state = emit_activity(state, StepPlan(prose))
                          run_steps_round(state, round, parsed, tu.id)
                        }
                      }
                    }
                  }
              }
            }
          }
        }
      }
  }
}

fn run_steps_round(
  state: State,
  round: Int,
  parsed: tool_steps.Parsed,
  tool_id: String,
) -> #(State, Int) {
  // A round whose steps are ALL `collect` (and nothing else) accomplishes
  // nothing while children are still running — their output is delivered
  // automatically. Treating it as a productive round is what lets the model
  // busy-poll forever. Short-circuit it into the event-driven wait: answer the
  // tool call once, then hold for the next real result instead of re-prompting.
  case is_poll_round(parsed.steps) && state.subagents.pending() {
    True -> hold_for_subagents(state, round, tool_id)
    False -> run_steps_round_active(state, round, parsed, tool_id)
  }
}

/// True when the batch is non-empty and every step is a `collect` — a pure
/// status probe with no workspace effect.
pub fn is_poll_round(steps: List(Step)) -> Bool {
  !list.is_empty(steps)
  && list.all(steps, fn(s) {
    case s {
      Collect(_, _) -> True
      _ -> False
    }
  })
}

/// Answer a pure-poll round with one short note (no per-target "still running"
/// cards) and fold into `settle_subagents`, which blocks on `wake` until a
/// child reports. Converts the model's busy-wait into the intended async wait.
fn hold_for_subagents(
  state: State,
  round: Int,
  tool_id: String,
) -> #(State, Int) {
  let note =
    "Subagents are still running. Their final output is delivered to you "
    <> "automatically as a message when each finishes — you do not need to "
    <> "`collect`. Holding for the next result."
  let state =
    push_message(
      state,
      provider.tool_result(state.config.provider, tool_id, note),
    )
  // `settle_subagents` adds its own "… waiting for subagents" activity, so no
  // extra timeline noise here — and none of the loud cards a real `collect`
  // would emit per target.
  settle_subagents(state, round)
}

fn run_steps_round_active(
  state: State,
  round: Int,
  parsed: tool_steps.Parsed,
  tool_id: String,
) -> #(State, Int) {
  // Plan-review gate (SPEC §5.4): pause a non-empty plan for human approval.
  case state.config.review && !list.is_empty(parsed.steps) {
    False -> execute_plan(state, round, parsed, tool_id)
    True ->
      case await_plan(state, parsed.steps) {
        control.Allow -> execute_plan(state, round, parsed, tool_id)
        control.Steer(message) -> {
          let guidance = case string.trim(message) {
            "" ->
              "The human rejected this plan. Reconsider and propose a different approach."
            m ->
              "The human reviewed your plan and did NOT approve it. Their guidance:\n"
              <> m
              <> "\nRevise the plan accordingly and call run_steps again."
          }
          let state = emit_activity(state, StepReview("plan rejected by human"))
          let state =
            push_message(
              state,
              provider.tool_result(state.config.provider, tool_id, guidance),
            )
          run_rounds(state, round + 1)
        }
      }
  }
}

/// Publish the proposed plan with status "awaiting_plan" and block on the
/// caller's `await` until the human allows or steers it. The await marker is
/// transient — it isn't kept in the persisted activity list.
fn await_plan(state: State, steps: List(Step)) -> control.Decision {
  let activities = [StepAwait(plan_summary(steps)), ..state.activities]
  state.emit(
    "awaiting_plan",
    list.reverse(activities),
    state.context_tokens,
    state.net_events,
  )
  state.await()
}

/// A compact, human-scannable rendering of a planned batch: one line per step.
fn plan_summary(steps: List(Step)) -> String {
  steps
  |> list.index_map(fn(step, i) {
    int.to_string(i + 1) <> ". " <> step_verb(step) <> "  " <> step_arg(step)
  })
  |> string.join("\n")
}

fn execute_plan(
  state: State,
  round: Int,
  parsed: tool_steps.Parsed,
  tool_id: String,
) -> #(State, Int) {
  let #(state, fb_rev) = exec_steps(state, parsed.steps, 1, [])
  let #(state, fb_rev) = run_check(state, fb_rev)
  let #(state, fb_rev) = review_or_status(state, fb_rev)
  let feedback =
    [round_line(state, round), ..fb_rev]
    |> list.reverse
    |> string.join("\n\n")
  let state =
    push_message(
      state,
      provider.tool_result(state.config.provider, tool_id, feedback),
    )
  case budget_left(state) {
    False -> #(
      notice(
        state,
        "Budget exhausted: " <> int.to_string(state.steps_done) <> " steps.",
      ),
      round,
    )
    True -> run_rounds(state, round + 1)
  }
}

fn handle_done(state: State, round: Int, tool_id: String) -> #(State, Int) {
  case state.check_ok && state.reviewed {
    True ->
      settle_subagents(emit_activity(state, StepReview("accepted → DONE")), round)
    False ->
      case state.check == None && state.steps_done == 0 {
        // done used loosely on a non-task turn — just a reply.
        True -> settle_subagents(state, round)
        False -> {
          let why = case state.check_ok {
            False -> "the check has not passed (or was never committed). "
            True -> "final review not completed. "
          }
          let state =
            push_message(
              state,
              provider.tool_result(
                state.config.provider,
                tool_id,
                "Cannot finish: "
                  <> why
                  <> "Call run_steps with corrective steps (and a stricter check).",
              ),
            )
          run_rounds(state, round + 1)
        }
      }
  }
}

/// The first `run_steps` call in the assistant turn, if any.
fn find_run_steps(tool_uses: List(provider.ToolUse)) -> Option(provider.ToolUse) {
  list.find(tool_uses, fn(tu) { tu.name == tools.run_steps_name })
  |> option.from_result
}

// --- Step execution ------------------------------------------------------

fn exec_steps(
  state: State,
  steps: List(Step),
  idx: Int,
  fb_rev: List(String),
) -> #(State, List(String)) {
  case steps {
    [] -> #(state, fb_rev)
    [step, ..rest] ->
      case budget_left(state) {
        False -> #(state, fb_rev)
        True -> {
          let title = artifact.step_title(step)
          let verb = step_verb(step)
          let state =
            emit_activity(
              state,
              StepCall(verb, step_arg(step), step_full(step)),
            )
          let #(state, result, fixes) = apply_with_fixes(state, step)
          let dig = digest.digest(result.output, state.config.digest_limit)
          let #(state, pointer) = maybe_save(state, result.output, dig)
          let state =
            emit_activity(state, StepExec(verb, result.exit, dig <> pointer))
          let fixed = case fixes > 0 {
            True -> " (after " <> int.to_string(fixes) <> " worker fix)"
            False -> ""
          }
          let fb =
            "### RESULT "
            <> int.to_string(idx)
            <> ": "
            <> title
            <> fixed
            <> "\nexit "
            <> int.to_string(result.exit)
            <> "\n"
            <> dig
            <> pointer
          exec_steps(state, rest, idx + 1, [fb, ..fb_rev])
        }
      }
  }
}

/// Apply one step; on failure, give the worker `fix_attempts` shots at one fix
/// command each, through the same sandbox path.
fn apply_with_fixes(state: State, step: Step) -> #(State, Exec, Int) {
  let #(state, result) = apply(state, step)
  case state.config.worker {
    None -> #(state, result, 0)
    Some(model) -> fix_loop(state, step, result, model, 0)
  }
}


fn fix_loop(
  state: State,
  step: Step,
  result: Exec,
  model: String,
  fixes: Int,
) -> #(State, Exec, Int) {
  case
    result.exit != 0 && fixes < state.config.fix_attempts && budget_left(state)
  {
    False -> #(state, result, fixes)
    True -> {
      let prompt =
        "FAILED STEP: "
        <> artifact.step_title(step)
        <> "\n"
        <> step_detail(step)
        <> "\n\nEXIT CODE: "
        <> int.to_string(result.exit)
        <> "\nOUTPUT:\n"
        <> digest.digest(result.output, state.config.digest_limit)
      case
        worker.complete_with(
          state.config.worker_url,
          model,
          prompts.worker_system,
          prompt,
          1500,
          state.config.worker_temperature,
          state.config.worker_top_p,
        )
      {
        Error(e) -> {
          let state = notice(state, "worker unavailable: " <> e)
          #(state, result, fixes)
        }
        Ok(text) ->
          case artifact.first_fence(text) {
            None -> #(state, result, fixes)
            Some(cmd) -> {
              let #(state, retry) = exec_run(state, cmd)
              // Carry the brief the worker was handed (the failing step + its
              // error), so the UI can show the plan, not just the fix command.
              let state = emit_activity(state, StepWorker(prompt, cmd, retry.exit))
              // Keep the retry if it fixed things or at least changed the
              // failure mode; otherwise keep the original result.
              let result = case retry.exit == 0 || retry.exit != result.exit {
                True -> retry
                False -> result
              }
              fix_loop(state, step, result, model, fixes + 1)
            }
          }
      }
    }
  }
}

fn apply(state: State, step: Step) -> #(State, Exec) {
  case step {
    Code(_, code) -> exec_code(state, code)
    Run(_, cmd) -> exec_run(state, cmd)
    Write(_, path, content) -> write_file(state, path, content)
    Edit(_, path, search, replace) -> edit_file(state, path, search, replace)
    Read(_, path, range) -> read_file(state, path, range)
    Grep(_, pattern) -> grep(state, pattern)
    // Delegation runs out-of-band (concurrent nested agents), not in the
    // sandbox; the injected ops return text to feed back to the supervisor.
    Spawn(title, task) -> #(
      bump(state),
      Exec(0, state.subagents.spawn(title, task, state.wake)),
    )
    Tell(_, target, message) -> #(
      bump(state),
      Exec(0, state.subagents.tell(target, message)),
    )
    Collect(_, target) -> #(
      bump(state),
      Exec(0, state.subagents.collect(target)),
    )
  }
}

/// Run a Python program in the monty sandbox (SPEC §5.2) — the supervisor's
/// primary action. The program's `bash` calls go through nono *inside* the
/// sidecar; the engine scopes them with the session profile (capability groups +
/// net leash), so the policy is enforced. The live egress feed and the
/// net-approval gate do NOT cover code-mode: nono flushes a session's audited
/// egress events only on finalization (10+s later), so there's no timely,
/// per-command signal to surface or gate on — they remain RUN-path features.
fn exec_code(state: State, code: String) -> #(State, Exec) {
  // Sandbox the sidecar's `bash` with a generated macOS Seatbelt profile, and
  // bring up the session's mitmproxy (allowlist + credential injection). The
  // profile locks egress to its loopback port and bash's clients point at it
  // (HTTPS_PROXY + the proxy CA, via env monty reads). If the proxy can't start,
  // `None` leaves the network open (transitional fallback).
  let port = ensure_proxy(state)
  let profile = option.from_result(write_seatbelt_profile(state, port))
  let #(exit, output) =
    monty_bridge.run_code(state.workspace, code, profile, bash_proxy_env(port))
  #(bump(state), Exec(exit, output))
}

/// Start (or reuse) the workspace's mitmproxy, configured from the session's
/// allowlist + enabled providers. Returns its loopback port.
fn ensure_proxy(state: State) -> Option(Int) {
  let #(config, secrets) = proxy_inputs(state)
  proxy.ensure(state.workspace, config, secrets) |> option.from_result
}

/// Build the proxy's config (`{allow, inject}`) and secret env from the session:
/// the approved hosts plus any enabled provider's hosts/injection. (github is
/// wired today; other providers generalize to more inject rules.)
fn proxy_inputs(state: State) -> #(String, List(#(String, String))) {
  let github = list.contains(state.groups, "github")
  let gh_hosts = case github {
    True -> ["github.com", "api.github.com", "codeload.github.com"]
    False -> []
  }
  let allow = list.unique(list.append(state.net_allow, gh_hosts))
  let inject = case github {
    False -> []
    True -> [
      json.object([
        #("hosts", json.array(["api.github.com"], json.string)),
        #("header", json.string("Authorization")),
        #("format", json.string("Bearer {}")),
        #("secret_env", json.string("BOUGH_SECRET_github")),
      ]),
      json.object([
        #("hosts", json.array(["github.com", "codeload.github.com"], json.string)),
        #("scheme", json.string("basic")),
        #("user", json.string("x-access-token")),
        #("secret_env", json.string("BOUGH_SECRET_github")),
      ]),
    ]
  }
  let config =
    json.to_string(
      json.object([
        #("allow", json.array(allow, json.string)),
        #("inject", json.preprocessed_array(inject)),
      ]),
    )
  let secrets = case github, github_token() {
    True, Ok(token) -> [#("BOUGH_SECRET_github", token)]
    _, _ -> []
  }
  #(config, secrets)
}

/// The real GitHub token, read OUTSIDE the sandbox for proxy-side injection.
fn github_token() -> Result(String, Nil) {
  shellout.command("gh", ["auth", "token"], ".", [])
  |> result.map(string.trim)
  |> result.replace_error(Nil)
}

/// The env that routes monty's `bash` through the proxy — passed per-invocation
/// (not via bough's global env), so concurrent sessions on different ports can't
/// clobber each other.
fn bash_proxy_env(port: Option(Int)) -> List(#(String, String)) {
  case port {
    None -> []
    Some(p) -> {
      let home = envoy.get("HOME") |> result.unwrap("")
      [
        #("BOUGH_BASH_PROXY", "http://127.0.0.1:" <> int.to_string(p)),
        #("BOUGH_BASH_PROXY_CA", home <> "/.mitmproxy/mitmproxy-ca-cert.pem"),
      ]
    }
  }
}

/// Write the run's Seatbelt profile (sibling of the net profile in the run dir).
fn write_seatbelt_profile(
  state: State,
  port: Option(Int),
) -> Result(String, Nil) {
  let path = string.replace(state.net_profile_path, "net.json", "sandbox.sb")
  let home = envoy.get("HOME") |> result.unwrap("")
  seatbelt.write(path, state.workspace, home, port)
}

/// Cap on the approve→retry loop for one command, so a never-matching rule
/// can't spin forever.

/// Run a one-off shell command (worker fix, CHECK) under the same Seatbelt +
/// proxy sandbox as code-mode `bash`.
fn exec_run(state: State, cmd: String) -> #(State, Exec) {
  let #(code, out) = sandboxed(state, ["sh", "-c", cmd], [])
  #(bump(state), Exec(code, out))
}

fn write_file(state: State, path: String, content: String) -> #(State, Exec) {
  let resolved = resolve(state.workspace, path)
  let exec = case sandboxed_write(state, resolved, content) {
    #(0, _) ->
      Exec(
        0,
        "wrote "
          <> resolved
          <> " ("
          <> int.to_string(string.length(content))
          <> " chars)",
      )
    #(code, out) ->
      Exec(code, "write failed (exit " <> int.to_string(code) <> "): " <> out)
  }
  #(bump(state), exec)
}

fn edit_file(
  state: State,
  path: String,
  search: String,
  replace: String,
) -> #(State, Exec) {
  let resolved = resolve(state.workspace, path)
  let exec = case simplifile.read(resolved) {
    Error(e) ->
      Exec(1, "edit: cannot read " <> resolved <> ": " <> string.inspect(e))
    Ok(contents) ->
      case occurrences(contents, search) {
        0 -> Exec(1, "edit: search text not found in " <> resolved)
        1 ->
          case
            sandboxed_write(
              state,
              resolved,
              string.replace(contents, search, replace),
            )
          {
            #(0, _) -> Exec(0, "edited " <> resolved <> " (1 replacement)")
            #(code, out) ->
              Exec(
                code,
                "edit: write failed (exit "
                  <> int.to_string(code)
                  <> "): "
                  <> out,
              )
          }
        n ->
          Exec(
            1,
            "edit: search text is not unique ("
              <> int.to_string(n)
              <> " matches) — READ the file and make the search text unambiguous",
          )
      }
  }
  #(bump(state), exec)
}

fn read_file(
  state: State,
  path: String,
  range: Option(#(Int, Int)),
) -> #(State, Exec) {
  let resolved = resolve(state.workspace, path)
  let cmd = case range {
    Some(#(s, e)) ->
      "cat -n -- '"
      <> resolved
      <> "' | sed -n '"
      <> int.to_string(s)
      <> ","
      <> int.to_string(e)
      <> "p'"
    None -> "cat -n -- '" <> resolved <> "'"
  }
  let #(code, out) = sandboxed(state, ["sh", "-c", cmd], [])
  #(bump(state), Exec(code, out))
}

fn grep(state: State, pattern: String) -> #(State, Exec) {
  let escaped = string.replace(pattern, "'", "'\\''")
  let cmd = "grep -rnI -- '" <> escaped <> "' . | head -n 200 || true"
  let #(code, out) = sandboxed(state, ["sh", "-c", cmd], [])
  #(bump(state), Exec(code, out))
}

fn occurrences(haystack: String, needle: String) -> Int {
  list.length(string.split(haystack, needle)) - 1
}

// --- CHECK and review ----------------------------------------------------

fn run_check(state: State, fb_rev: List(String)) -> #(State, List(String)) {
  case state.check {
    None -> #(state, fb_rev)
    Some(check) ->
      case budget_left(state) {
        False -> #(state, fb_rev)
        True -> {
          let #(code, out) = sandboxed(state, ["sh", "-c", check], [])
          let state = State(..bump(state), check_ok: code == 0)
          let dig = digest.digest(out, 1000)
          let state = emit_activity(state, StepCheck(code == 0, dig))
          let fb =
            "### CHECK RESULT\n`"
            <> check
            <> "`\nexit "
            <> int.to_string(code)
            <> "\n"
            <> dig
          #(state, [fb, ..fb_rev])
        }
      }
  }
}

fn review_or_status(
  state: State,
  fb_rev: List(String),
) -> #(State, List(String)) {
  case state.check_ok, state.reviewed {
    True, False -> {
      let state = State(..state, reviewed: True)
      let mutated =
        integrity.changed_preexisting(state.workspace, state.baseline)
      let note = case mutated {
        [] -> "requested"
        _ -> "requested · touched " <> string.join(list.take(mutated, 5), ", ")
      }
      let state = emit_activity(state, StepReview(note))
      #(state, [review_prompt(mutated), ..fb_rev])
    }
    True, True -> #(state, [
      "### STATUS\nCheck passing. Call run_steps with done:true to finish, or send more steps.",
      ..fb_rev
    ])
    _, _ ->
      case state.check {
        None -> #(state, [
          "### STATUS\nNo check committed yet — pass a `check` (exits 0 iff the acceptance criteria hold) before you can finish.",
          ..fb_rev
        ])
        Some(_) -> #(state, fb_rev)
      }
  }
}

fn review_prompt(mutated: List(String)) -> String {
  "### REVIEW REQUESTED\nThe check passes — but a passing check is not proof, only evidence. Do NOT set done:true yet. First, adversarially verify: (1) re-read the task and list its literal acceptance criteria; (2) for at least one criterion, compute the expected result independently and compare it to what was produced with a concrete run action; (3) probe an edge case whose correct answer you know without running the implementation; (4) confirm your check actually tests those criteria on real values — if it only checks that a file exists or a command exited 0, pass a stricter `check` and let it re-run. Call run_steps with done:true only once an independent probe has confirmed correctness; otherwise send corrective steps."
  <> mutated_suffix(mutated)
}

fn mutated_suffix(mutated: List(String)) -> String {
  case mutated {
    [] -> ""
    _ ->
      "\nNote — pre-existing files modified this session: "
      <> string.join(list.take(mutated, 10), ", ")
      <> ". If any are tests or references your CHECK relies on, make sure you did not weaken them; a check that passes against weakened references is a failure."
  }
}

// --- Supervisor call -----------------------------------------------------

fn supervisor_call(state: State) -> Result(provider.Response, String) {
  provider.complete(
    state.config.provider,
    state.api_key,
    state.sup_model,
    prompts.supervisor_system(
      state.workspace,
      state.instructions,
      state.capabilities,
      state.skills,
    ),
    state.convo,
    // The supervisor acts only through the run_steps tool (§5.2).
    tools.run_steps_name,
    tools.run_steps_description(),
    tools.run_steps_schema(),
  )
}

/// Persist one line of supervisor-call telemetry to the run's bb dir, so a
/// surprising turn (notably an empty one — no prose, no run_steps) is
/// diagnosable after the fact: stop_reason, token counts, and whether a
/// run_steps tool call was present. Best-effort; never fails the run.
fn log_supervisor(state: State, round: Int, reply: provider.Response) -> Nil {
  let has_run_steps = case find_run_steps(reply.tool_uses) {
    Some(_) -> "true"
    None -> "false"
  }
  let line =
    "{\"round\":"
    <> int.to_string(round)
    <> ",\"stop_reason\":\""
    <> reply.stop_reason
    <> "\",\"input_tokens\":"
    <> int.to_string(reply.input_tokens)
    <> ",\"output_tokens\":"
    <> int.to_string(reply.output_tokens)
    <> ",\"text_len\":"
    <> int.to_string(string.length(reply.text))
    <> ",\"tool_uses\":"
    <> int.to_string(list.length(reply.tool_uses))
    <> ",\"run_steps\":"
    <> has_run_steps
    <> "}\n"
  let _ = simplifile.create_directory_all(state.bb_dir)
  let _ = simplifile.append(state.bb_dir <> "/supervisor.jsonl", line)
  Nil
}

// --- State helpers -------------------------------------------------------

/// Project instructions for the supervisor: the workspace's AGENTS.md, or
/// CLAUDE.md as a fallback, if present and non-empty (SPEC.md §5). Read once per
/// run and injected into the system prompt.
fn read_agents_md(workspace: String) -> Option(String) {
  [workspace <> "/AGENTS.md", workspace <> "/CLAUDE.md"]
  |> list.find_map(fn(path) {
    case simplifile.read(path) {
      Ok(content) ->
        case string.trim(content) {
          "" -> Error(Nil)
          c -> Ok(c)
        }
      Error(_) -> Error(Nil)
    }
  })
  |> option.from_result
}

/// A plain-language description of this run's sandbox reach, injected into the
/// supervisor's system prompt (SPEC §7). The point is that the supervisor can
/// predict what will fail — a signed commit, an SSH push, a fetch to a host
/// that isn't allowlisted — instead of burning rounds rediscovering each limit.
/// Built once at run start.
fn capabilities_summary(
  config: Config,
  net_allow: List(String),
  groups: List(String),
) -> String {
  let network = case config.net_gate {
    // Fully blocked: no outbound connection succeeds.
    False ->
      "blocked entirely — no outbound network. `git push`/`git fetch`, package installs, and any remote fetch will fail."
    // Default-deny, routed through the session's filtering proxy.
    True -> {
      let hosts = case net_allow {
        [] -> "(none beyond what enabled capabilities grant)"
        _ -> string.join(net_allow, ", ")
      }
      "routed through this session's filtering proxy, default-deny: only allowlisted hosts ("
      <> hosts
      <> ") and the hosts your enabled capabilities grant are reachable; any other host is blocked."
    }
  }

  // Enabled capabilities (providers), with their descriptions.
  let group_lines =
    list.map(groups, fn(name) {
      case providers.get(name) {
        Ok(p) -> name <> " — " <> p.description
        Error(_) -> name
      }
    })
  let groups_text = case group_lines {
    [] -> "none beyond the defaults"
    _ -> string.join(group_lines, "; ")
  }

  // GitHub auth is injected at the network proxy (token never in the sandbox),
  // but `gh` can't use it (Go ignores the proxy CA on macOS) — tell the model to
  // reach for git + curl with the phantom token instead.
  let active = list.filter_map(groups, providers.get)
  let github_note = case list.any(active, fn(p) { p.name == "github" }) {
    False -> ""
    True ->
      " GitHub auth is injected at the network proxy: $GITHUB_TOKEN in the sandbox is a PHANTOM that nono swaps for the real token on egress. Use git + curl, NOT `gh` (it won't trust the proxy CA here). git push: `git -c credential.helper='!f(){ echo username=x-access-token; echo password=$GITHUB_TOKEN; };f' push ...`. REST: `curl -H \"Authorization: Bearer $GITHUB_TOKEN\" https://api.github.com/...`."
  }

  "\n\n# Capabilities this run\nYour actions run inside a monty + macOS Seatbelt sandbox with a fixed, known reach. Reason about it BEFORE acting; if a request needs access you don't have, say so plainly and propose the smallest change (a host to allowlist, a capability to enable, or a step for the human to run outside the sandbox) instead of retrying.\n\n- Filesystem: the workspace is read-write (plus toolchain caches like ~/.cargo, ~/.npm). The rest of $HOME is DENIED — credentials and keys (~/.ssh, ~/.aws, ~/.config, keychains, git signing keys) are not readable, and writes outside the workspace are denied. A signed `git commit` (commit.gpgsign / SSH signing) and SSH-authenticated git will not work from here."
  <> github_note
  <> "\n- Network: "
  <> network
  <> "\n- Capabilities enabled: "
  <> groups_text
  <> "."
}

/// Run a command under the run's Seatbelt sandbox + the session mitmproxy — the
/// same filesystem/egress policy as code-mode `bash`. The proxy env is set only
/// on the child (inline), so bough's own process is unaffected; the real exit
/// code comes back via the shell.
fn sandboxed(
  state: State,
  command: List(String),
  _reads: List(String),
) -> #(Int, String) {
  let port = ensure_proxy(state)
  let inner = case write_seatbelt_profile(state, port) {
    Ok(profile) -> "sandbox-exec -f " <> shq(profile) <> " " <> join_args(command)
    Error(_) -> join_args(command)
  }
  run_shell(state.workspace, proxy_env_prefix(port) <> inner)
}

/// Inline `KEY=val ` env so a sandboxed child routes through the proxy (and
/// trusts its CA) without touching bough's own environment.
fn proxy_env_prefix(port: Option(Int)) -> String {
  case port {
    None -> ""
    Some(p) -> {
      let url = "http://127.0.0.1:" <> int.to_string(p)
      let ca = case envoy.get("HOME") {
        Ok(home) -> home <> "/.mitmproxy/mitmproxy-ca-cert.pem"
        Error(_) -> ""
      }
      "HTTPS_PROXY=" <> url <> " HTTP_PROXY=" <> url <> " SSL_CERT_FILE=" <> ca
      <> " CURL_CA_BUNDLE=" <> ca <> " GIT_SSL_CAINFO=" <> ca <> " "
    }
  }
}

fn join_args(command: List(String)) -> String {
  command |> list.map(shq) |> string.join(" ")
}

fn run_shell(workspace: String, full: String) -> #(Int, String) {
  case shellout.command("sh", ["-c", full], workspace, []) {
    Ok(out) -> #(0, out)
    Error(#(code, out)) -> #(code, out)
  }
}


/// Write `content` to `dest` through the nono sandbox, so the workspace boundary
/// is kernel-enforced — a path that escapes the workspace (including via a
/// symlink) is denied, matching how run/read/grep are confined. The content is
/// staged in bough's own dir and granted to the sandbox read-only, so it never
/// passes through argv (no length limit, no escaping of the bytes).
fn sandboxed_write(state: State, dest: String, content: String) -> #(Int, String) {
  let _ = simplifile.create_directory_all(state.bb_dir)
  let stage = state.bb_dir <> "/stage"
  case simplifile.write(stage, content) {
    Error(e) -> #(1, "could not stage write: " <> string.inspect(e))
    Ok(_) -> {
      let cmd = "cat -- " <> shq(stage) <> " > " <> shq(dest)
      let result = sandboxed(state, ["sh", "-c", cmd], [stage])
      let _ = simplifile.delete(stage)
      result
    }
  }
}

/// Single-quote a path for `sh -c`, escaping any embedded single quotes.
fn shq(s: String) -> String {
  "'" <> string.replace(s, "'", "'\\''") <> "'"
}

fn budget_left(state: State) -> Bool {
  state.steps_done < state.config.max_steps
}

fn bump(state: State) -> State {
  State(..state, steps_done: state.steps_done + 1)
}

fn push_message(state: State, message: JsonValue) -> State {
  State(..state, convo: list.append(state.convo, [message]))
}

/// Seed the conversation from the branch's prior turns (plain text) plus the
/// new user prompt. tool_use/tool_result blocks only appear within this run.
fn seed_convo(
  history: List(#(String, String)),
  user_prompt: String,
) -> List(JsonValue) {
  list.append(
    list.map(history, fn(turn) {
      case turn.0 {
        "assistant" -> provider.assistant_text(turn.1)
        _ -> provider.user_text(turn.1)
      }
    }),
    [provider.user_text(user_prompt)],
  )
}

/// The text of the most recent assistant message, for the turn's Outcome.
fn last_assistant_text(p: provider.Provider, convo: List(JsonValue)) -> String {
  convo
  |> list.reverse
  |> list.find_map(fn(msg) {
    case json_value.string_field(msg, "role") {
      Ok("assistant") -> Ok(provider.message_text(p, msg))
      _ -> Error(Nil)
    }
  })
  |> result.unwrap("")
}

fn update_check(state: State, check: Option(String)) -> State {
  case check {
    Some(c) ->
      case Some(c) != state.check {
        True -> State(..state, check: Some(c), check_ok: False)
        False -> state
      }
    None -> state
  }
}

fn emit_activity(state: State, activity: Activity) -> State {
  let activities = [activity, ..state.activities]
  state.emit(
    "running",
    list.reverse(activities),
    state.context_tokens,
    state.net_events,
  )
  State(..state, activities: activities)
}

fn notice(state: State, text: String) -> State {
  emit_activity(state, StepText("⚠ " <> text))
}

/// A supervisor turn produced nothing usable. Inject a corrective nudge and
/// retry — but only up to `max_bad_turns` in a row, then surface a clear
/// diagnostic instead of looping (recovery, not give-up; bounded, not endless).
fn recover_bad_turn(
  state: State,
  round: Int,
  diagnostic: String,
  nudge: String,
) -> #(State, Int) {
  let n = state.bad_turns + 1
  case n >= max_bad_turns {
    True -> #(
      notice(
        state,
        diagnostic
          <> " — gave up after "
          <> int.to_string(n)
          <> " unproductive turns in a row. Try a more capable model or a smaller task.",
      ),
      round,
    )
    False -> {
      let state = State(..state, bad_turns: n)
      let state = push_message(state, provider.user_text(nudge))
      run_rounds(state, round + 1)
    }
  }
}

/// The corrective message for an empty turn, tailored to whether the output was
/// truncated (cut off mid-thought) or just empty.
fn empty_turn_nudge(stop_reason: String) -> String {
  case stop_reason {
    "max_tokens" | "length" ->
      "Your previous response was cut off before you produced an action. Be concise — emit a single `run_steps` tool call now, splitting a long program into smaller steps if needed."
    _ ->
      "Your previous turn produced no action. Respond now with a single `run_steps` tool call, or give your final answer if the task is already done."
  }
}

/// The turn is about to end. If subagents the supervisor spawned are still
/// running, don't finish — wait for the next one to deliver its result, fold it
/// into the conversation, and let the supervisor run again so it can synthesize.
/// Ends only once every spawned subagent has reported. This is what lets the
/// supervisor `spawn` and move on instead of sitting in a blocking `collect`.
fn settle_subagents(state: State, round: Int) -> #(State, Int) {
  case state.subagents.pending() {
    False -> #(state, round)
    True ->
      wait_for_subagent(
        emit_activity(state, StepText("… waiting for subagents to report")),
        round,
      )
  }
}

fn wait_for_subagent(state: State, round: Int) -> #(State, Int) {
  case state.stopped() {
    True -> #(notice(state, "Stopped by you."), round)
    False -> wait_for_subagent_active(state, round)
  }
}

fn wait_for_subagent_active(state: State, round: Int) -> #(State, Int) {
  case state.inbox() {
    // A subagent result (or a human steer) arrived — fold it in and let the
    // supervisor continue so it can act on it.
    Some(msg) -> {
      let state = emit_activity(state, StepText("⟵ subagent: " <> msg))
      let state = push_message(state, provider.user_text(msg))
      run_rounds(state, round + 1)
    }
    // Nothing yet. Keep waiting while a child is still working; otherwise end.
    None ->
      case state.subagents.pending() {
        True -> {
          // Block until a child signals completion on `wake` — event-driven, no
          // busy-poll. The 1 s cap is just a fallback so a human steer (which
          // writes the disk inbox without a wake) is still picked up promptly.
          let _ = process.receive(state.wake, 1000)
          wait_for_subagent(state, round)
        }
        False -> #(state, round)
      }
  }
}

/// Inject a pending human message into the conversation (echoed as an activity),
/// so the supervisor sees it on its next call. At most one per round.
fn drain_inbox(state: State) -> State {
  case state.inbox() {
    None -> state
    Some(msg) -> {
      let state = emit_activity(state, StepText("⟵ human: " <> msg))
      push_message(state, provider.user_text(msg))
    }
  }
}

fn round_line(state: State, round: Int) -> String {
  "Round "
  <> int.to_string(round)
  <> "/"
  <> int.to_string(state.config.max_rounds)
  <> ", steps "
  <> int.to_string(state.steps_done)
  <> "/"
  <> int.to_string(state.config.max_steps)
  <> "."
}

fn step_detail(step: Step) -> String {
  case step {
    Code(_, code) -> "CODE:\n" <> code
    Run(_, cmd) -> "RUN: " <> cmd
    Write(_, path, _) -> "WRITE " <> path
    Edit(_, path, _, _) -> "EDIT " <> path
    Read(_, path, _) -> "READ " <> path
    Grep(_, pattern) -> "GREP " <> pattern
    Spawn(title, _) -> "SPAWN " <> title
    Tell(_, target, _) -> "TELL " <> target
    Collect(_, target) -> "COLLECT " <> target
  }
}

/// The verb label shown in the step timeline.
fn step_verb(step: Step) -> String {
  case step {
    Code(_, _) -> "CODE"
    Run(_, _) -> "RUN"
    Write(_, _, _) -> "WRITE"
    Edit(_, _, _, _) -> "EDIT"
    Read(_, _, _) -> "READ"
    Grep(_, _) -> "GREP"
    Spawn(_, _) -> "SPAWN"
    Tell(_, _, _) -> "TELL"
    Collect(_, _) -> "COLLECT"
  }
}

/// The full content of a step for the plan view: the file body for WRITE, the
/// find/replace for EDIT, the task for SPAWN. Empty when the arg already says it
/// all (RUN/READ/GREP/TELL/COLLECT).
fn step_full(step: Step) -> String {
  case step {
    Code(_, code) -> code
    Write(_, _, content) -> content
    Edit(_, _, find, replace) ->
      "── find ──\n" <> find <> "\n── replace ──\n" <> replace
    Spawn(_, task) -> task
    _ -> ""
  }
}

/// The concrete argument (command / path / pattern) for a step.
fn step_arg(step: Step) -> String {
  case step {
    Code(_, _) -> ""
    Run(_, cmd) -> cmd
    Write(_, path, _) -> path
    Edit(_, path, _, _) -> path
    Read(_, path, _) -> path
    Grep(_, pattern) -> pattern
    Spawn(title, _) -> title
    Tell(_, target, _) -> target
    Collect(_, target) -> target
  }
}

fn resolve(workspace: String, path: String) -> String {
  case string.starts_with(path, "/") {
    True -> path
    False -> workspace <> "/" <> path
  }
}

// --- Blackboard ----------------------------------------------------------

fn bb_dir() -> String {
  let home = result.unwrap(envoy.get("HOME"), "/tmp")
  home <> "/.bough/bb/" <> int.to_string(clock.now_ms())
}

/// When output exceeds its digest, return a suffix telling the model the output
/// was truncated and how to see more. It used to point at a blackboard file
/// under ~/.bough — but read/code/bash are all sandboxed to the workspace, so
/// the model could never open that path (it just burned steps on a guaranteed
/// "outside the workspace" error). A re-query hint is honest and actionable.
fn maybe_save(state: State, full: String, dig: String) -> #(State, String) {
  case string.length(full) > string.length(dig) {
    False -> #(state, "")
    True -> #(
      state,
      "\n[output truncated to the digest above — re-run more narrowly "
        <> "(grep, head, or a specific path) to see the rest]",
    )
  }
}
