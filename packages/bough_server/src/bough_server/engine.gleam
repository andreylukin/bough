//// The supervisor-worker loop — the harness proper (SPEC.md §5.3).
////
//// One append-only conversation per task. Each user message starts a turn: the
//// supervisor replies with prose and/or STEP artifacts, the harness (the only
//// thing that executes) applies them inside the nono sandbox, feeds every
//// result back, and gates completion on a deterministic CHECK plus an
//// adversarial review. Ported from tent's `engine/mod.rs`.
////
//// Output reuses `agent.Step`/`agent.Outcome`/`agent.run_json`, but emits the
//// loop's roles as phased events so the TUI can render each distinctly:
//// supervisor prose is `StepPlan`, a harness step is `StepCall` + `StepExec`
//// (with exit code), a local-worker fix is `StepWorker`, and the guardrails are
//// `StepCheck` and `StepReview`. Plain `StepText` is left for notices.

import bough_core/artifact.{
  type Step, Collect, Edit, Grep, Read, Run, Spawn, Tell, Write,
}
import bough_core/digest
import bough_core/nono
import bough_server/agent.{
  type Outcome, type Step as Activity, Outcome, StepAwait, StepCall, StepCheck,
  StepExec, StepGroup, StepNet, StepPlan, StepReview, StepText, StepWorker,
}
import bough_server/clock
import bough_server/control
import bough_server/integrity
import bough_server/json_value.{type JsonValue}
import bough_server/net_profile
import bough_server/nono_bridge
import bough_server/prompts
import bough_server/provider
import bough_server/tool_steps
import bough_server/tools
import bough_server/worker
import envoy
import gleam/dict.{type Dict}
import gleam/erlang/process
import gleam/int
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/result
import gleam/string
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
    /// Credentials nono injects into sandboxed commands on egress (SPEC §6.4):
    /// (credential_name, env_var) pairs declared in the generated profile so the
    /// raw secret never enters the sandbox. Empty by default (opt-in).
    net_credentials: List(#(String, String)),
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
    spawn: fn(String, String) -> String,
    tell: fn(String, String) -> String,
    collect: fn(String) -> String,
    pending: fn() -> Bool,
  )
}

/// A no-op set, for runs that can't host subagents (the non-streaming path).
pub fn no_subagents() -> Subagents {
  let unavailable = "subagents are only available in a streaming run"
  Subagents(
    spawn: fn(_, _) { unavailable },
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
    net_credentials: [],
  )
}

type Exec {
  Exec(exit: Int, output: String)
}

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
    // Spawn / message / collect subagents (async delegation).
    subagents: Subagents,
    // append-only Anthropic message list, oldest first. Carries tool_use /
    // tool_result blocks verbatim, so it is JsonValue rather than text pairs.
    convo: List(JsonValue),
    // Tokens occupying the supervisor's context after the latest turn.
    context_tokens: Int,
    baseline: Dict(String, Int),
    bb_dir: String,
    bb_idx: Int,
    check: Option(String),
    check_ok: Bool,
    reviewed: Bool,
    steps_done: Int,
    // Project instructions (AGENTS.md), read once at run start.
    instructions: Option(String),
    // A plain-language summary of this run's sandbox reach (network posture,
    // enabled capability groups, filesystem limits), built once at run start
    // and injected into the supervisor's system prompt so it reasons about what
    // it can do up front instead of rediscovering limits round by round.
    capabilities: String,
    // The network allowlist for sandboxed commands; grows as hosts are approved.
    net_allow: List(String),
    // Session-enabled nono capability groups, layered into the run's profile.
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
  subagents: Subagents,
  net_allow: List(String),
  groups: List(String),
  suggested: List(String),
) -> Result(Outcome, String) {
  let dir = bb_dir()
  let state =
    State(
      api_key: api_key,
      sup_model: sup_model,
      workspace: workspace,
      config: config,
      emit: emit,
      await: await,
      inbox: inbox,
      subagents: subagents,
      convo: seed_convo(history, user_prompt),
      context_tokens: 0,
      baseline: integrity.snapshot(workspace),
      bb_dir: dir,
      bb_idx: 0,
      check: None,
      check_ok: False,
      reviewed: False,
      steps_done: 0,
      instructions: read_agents_md(workspace),
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
    no_subagents(),
    [],
    groups,
    [],
  )
}

fn run_rounds(state: State, round: Int) -> #(State, Int) {
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
              let state = case prose {
                "" -> state
                p -> emit_activity(state, StepPlan(p))
              }
              case find_run_steps(reply.tool_uses) {
                // No tool call → a conversational reply ends the turn, unless
                // subagents it spawned are still running (then hold and fold in
                // their results so it can synthesize). Guard the degenerate
                // case: an empty turn (no prose, no tool call, no pending
                // subagent) would otherwise end silently and read as "bough
                // never responded" — surface it with the stop_reason instead.
                None ->
                  case state.subagents.pending() == False && prose == "" {
                    True -> #(
                      notice(
                        state,
                        "supervisor returned an empty turn (stop_reason: "
                          <> reply.stop_reason
                          <> ")",
                      ),
                      round,
                    )
                    False -> settle_subagents(state, round)
                  }
                Some(tu) ->
                  case tool_steps.parse(tu.input) {
                    Error(msg) -> {
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
                    Ok(parsed) -> {
                      let state = update_check(state, parsed.check)
                      case parsed.done && list.is_empty(parsed.steps) {
                        True -> handle_done(state, round, tu.id)
                        False -> {
                          // Every executed run_steps call is its own plan; mark
                          // the boundary even when the supervisor offered no
                          // prose this round, so the plan pane can separate them.
                          let state = case prose {
                            "" -> emit_activity(state, StepPlan(""))
                            _ -> state
                          }
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
  // On a sandbox filesystem denial, pause for the human to enable a capability
  // group that would grant the access, then retry the step (SPEC §7).
  let #(state, result) = gate_groups(state, step, result, 0)
  case state.config.worker {
    None -> #(state, result, 0)
    Some(model) -> fix_loop(state, step, result, model, 0)
  }
}

/// Cap on the approve→retry loop for one step's group gate.
const group_max_retries = 4

/// When a step is denied filesystem access that an enableable group would grant,
/// pause for the human (status "awaiting_group"): approve to enable the
/// candidate group(s) — rebuilding the run's nono profile — and retry the step,
/// or reject and keep the failure for the supervisor. A denial with no candidate
/// group (e.g. a path outside any group's reach) is left to stand.
fn gate_groups(
  state: State,
  step: Step,
  result: Exec,
  tries: Int,
) -> #(State, Exec) {
  case is_fs_denial(result) && tries < group_max_retries {
    False -> #(state, result)
    True ->
      case candidate_groups(state, step, result) {
        [] -> #(state, result)
        candidates -> {
          let enable = group_decision(state, step, result, candidates)
          // Candidates the agent needed but the human didn't enable become
          // advisory suggestions (persisted, surfaced as ✋ and the banner) so
          // the ask isn't lost when the run moves on.
          let unmet =
            list.filter(candidates, fn(c) { !list.contains(enable, c) })
          let state =
            State(
              ..state,
              suggested: list.unique(list.append(state.suggested, unmet)),
            )
          case enable {
            // Rejected (or no usable group named): the failure stands.
            [] -> #(state, result)
            _ -> {
              let state =
                State(..state, groups: list.unique(list.append(state.groups, enable)))
              let #(state, result) = apply(state, step)
              gate_groups(state, step, result, tries + 1)
            }
          }
        }
      }
  }
}

/// True when a step's output carries a sandbox/OS filesystem-permission denial.
fn is_fs_denial(result: Exec) -> Bool {
  result.exit != 0 && contains_any(result.output, denial_markers)
}

/// The toggleable capability groups that would grant the denied access: the
/// deterministic path→group match, plus any the worker proposes, minus those
/// already enabled.
fn candidate_groups(state: State, step: Step, result: Exec) -> List(String) {
  let targets =
    list.append(step_paths(step), denied_paths(result.output))
    |> list.unique
  let deterministic = case targets {
    [] -> []
    _ -> nono_bridge.groups_for_paths(targets)
  }
  list.append(deterministic, suggester_worker(state, result, targets))
  |> list.unique
  |> list.filter(fn(g) { !list.contains(state.groups, g) })
}

/// Publish the gate with status "awaiting_group" and block on the human. Returns
/// the group name(s) to enable: `Allow` enables every candidate; a non-empty
/// `Steer` enables the named candidate(s) it lists; an empty `Steer` (reject)
/// enables none.
fn group_decision(
  state: State,
  _step: Step,
  result: Exec,
  candidates: List(String),
) -> List(String) {
  let detail = group_detail_text(result.output)
  let step = StepGroup(detail, string.join(candidates, ", "))
  let activities = [step, ..state.activities]
  state.emit(
    "awaiting_group",
    list.reverse(activities),
    state.context_tokens,
    state.net_events,
  )
  case state.await() {
    control.Allow -> candidates
    control.Steer(message) -> {
      let named =
        message
        |> string.replace("\n", ",")
        |> string.split(",")
        |> list.map(fn(s) { string.trim(string.lowercase(s)) })
      list.filter(candidates, fn(c) { list.contains(named, string.lowercase(c)) })
    }
  }
}

/// The denied path(s) for the gate prompt — the distinct paths the denial named.
fn group_detail_text(output: String) -> String {
  case denied_paths(output) |> list.unique {
    [] -> "a sandboxed step was denied filesystem access"
    paths -> string.join(paths, ", ")
  }
}

/// Signatures a sandbox/OS permission denial leaves in command output.
const denial_markers = ["Operation not permitted", "Permission denied"]

/// Ask the worker which available groups would resolve the denial. `[]` if the
/// worker is disabled/unreachable or proposes nothing usable. Constrained to the
/// toggleable catalog the human can actually enable.
fn suggester_worker(state: State, result: Exec, paths: List(String)) -> List(String) {
  case state.config.worker {
    None -> []
    Some(model) -> {
      let catalog =
        nono_bridge.list_groups()
        |> list.filter(fn(g) { !g.locked && !list.contains(state.groups, g.name) })
      let listing =
        catalog
        |> list.map(fn(g) { "- " <> g.name <> ": " <> g.description })
        |> string.join("\n")
      let prompt =
        "DENIED PATHS: "
        <> string.join(paths, ", ")
        <> "\n\nOUTPUT:\n"
        <> digest.digest(result.output, state.config.digest_limit)
        <> "\n\nAVAILABLE GROUPS:\n"
        <> listing
      case
        worker.complete(
          state.config.worker_url,
          model,
          prompts.suggester_system,
          prompt,
          200,
        )
      {
        Error(_) -> []
        Ok(text) -> parse_suggested(text, catalog)
      }
    }
  }
}

/// Parse the worker's comma/newline-separated reply into known toggleable group
/// names (case-insensitive), dropping "none" and anything off the catalog.
pub fn parse_suggested(text: String, catalog: List(nono.Group)) -> List(String) {
  let names = list.map(catalog, fn(g) { g.name })
  text
  |> string.replace("\n", ",")
  |> string.split(",")
  |> list.map(fn(s) { string.trim(string.lowercase(s)) })
  |> list.filter_map(fn(s) {
    case list.find(names, fn(n) { string.lowercase(n) == s }) {
      Ok(n) -> Ok(n)
      Error(_) -> Error(Nil)
    }
  })
}

/// Filesystem paths named in denial lines like
/// `mkdir: /Users/x/Library: Operation not permitted` — the text before the
/// marker, last colon-separated token.
pub fn denied_paths(output: String) -> List(String) {
  output
  |> string.split("\n")
  |> list.filter_map(fn(line) {
    case list.find(denial_markers, fn(m) { string.contains(line, m) }) {
      Error(_) -> Error(Nil)
      Ok(marker) ->
        case string.split_once(line, ": " <> marker) {
          Ok(#(before, _)) ->
            case string.split(before, ": ") |> list.last {
              Ok(path) ->
                case string.starts_with(string.trim(path), "/") {
                  True -> Ok(string.trim(path))
                  False -> Error(Nil)
                }
              Error(_) -> Error(Nil)
            }
          Error(_) -> Error(Nil)
        }
    }
  })
  |> list.unique
}

fn contains_any(haystack: String, needles: List(String)) -> Bool {
  list.any(needles, fn(n) { string.contains(haystack, n) })
}

/// The path(s) a step targets — the specific paths to map a denial against. For
/// a RUN, the path-like tokens in its command; for WRITE/EDIT, the file.
fn step_paths(step: Step) -> List(String) {
  case step {
    Run(_, cmd) -> paths_in(cmd)
    Write(_, path, _) -> [path]
    Edit(_, path, _, _) -> [path]
    _ -> []
  }
}

/// Whitespace tokens that look like filesystem paths (absolute, `~`, or
/// `$HOME`-rooted), unquoted — a cheap way to recover what a command touched.
fn paths_in(text: String) -> List(String) {
  text
  |> string.replace("\n", " ")
  |> string.split(" ")
  |> list.map(fn(t) {
    t
    |> string.replace("'", "")
    |> string.replace("\"", "")
  })
  |> list.filter(fn(t) {
    string.starts_with(t, "/")
    || string.starts_with(t, "~")
    || string.starts_with(t, "$HOME")
  })
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
        worker.complete(
          state.config.worker_url,
          model,
          prompts.worker_system,
          prompt,
          1500,
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
              let state = emit_activity(state, StepWorker(cmd, retry.exit))
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
    Run(_, cmd) -> exec_run(state, cmd)
    Write(_, path, content) -> write_file(state, path, content)
    Edit(_, path, search, replace) -> edit_file(state, path, search, replace)
    Read(_, path, range) -> read_file(state, path, range)
    Grep(_, pattern) -> grep(state, pattern)
    // Delegation runs out-of-band (concurrent nested agents), not in the
    // sandbox; the injected ops return text to feed back to the supervisor.
    Spawn(title, task) -> #(
      bump(state),
      Exec(0, state.subagents.spawn(title, task)),
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

/// Cap on the approve→retry loop for one command, so a never-matching rule
/// can't spin forever.
const net_max_retries = 6

fn exec_run(state: State, cmd: String) -> #(State, Exec) {
  exec_run_net(state, cmd, 0)
}

/// Run a command in the sandbox. With the net gate on (SPEC §7), a denied
/// request pauses for the human: approve at host or path-glob granularity and
/// the command is retried with the new rule; deny and the failure stands.
fn exec_run_net(state: State, cmd: String, tries: Int) -> #(State, Exec) {
  let command = ["sh", "-c", cmd]
  // Watermark the audit log before running so denial detection ignores prior
  // runs of an identical command (and this command's own earlier retries).
  let watermark = case state.config.net_gate {
    True -> nono_bridge.session_watermark(command)
    False -> ""
  }
  let #(code, out) = sandboxed(state, command, [])
  let state = bump(state)
  // One audit read serves both: record this run's egress for the network dock,
  // and derive any denials to gate on. Off the net gate there is no audit
  // session, so the feed stays empty (the policy is simply "net blocked").
  let #(state, denials) = case state.config.net_gate {
    False -> #(state, [])
    True -> {
      let events = net_events_for(command, watermark, 0)
      let state =
        State(..state, net_events: list.append(state.net_events, events))
      #(state, denials_from(events))
    }
  }
  case state.config.net_gate && tries < net_max_retries {
    False -> #(state, Exec(code, out))
    True ->
      case denials {
        [] -> #(state, Exec(code, out))
        [denial, ..] ->
          case net_decision(state, denial) {
            // Denied (or no rule given): keep the failure for the supervisor.
            Error(_) -> #(state, Exec(code, out))
            // A new allow rule: add it (deduped) and retry so it can succeed.
            Ok(rule) ->
              exec_run_net(
                State(..state, net_allow: add_rule(state.net_allow, rule)),
                cmd,
                tries + 1,
              )
          }
      }
  }
}

/// Add an allow rule (deduped). The generated nono profile groups rules by host
/// and unions their endpoint globs (`net_profile`), so accumulating multiple
/// path rules for one host widens access rather than replacing it.
fn add_rule(rules: List(String), rule: String) -> List(String) {
  case list.contains(rules, rule) {
    True -> rules
    False -> list.append(rules, [rule])
  }
}

/// The egress events for the run just performed. Polls until the run's audit
/// session appears (nono flushes it shortly after the command exits) so events
/// aren't missed by reading too early; reads them only if the session had
/// network activity. Bounded (~1s) — if the session never shows, assume none.
fn net_events_for(
  command: List(String),
  watermark: String,
  polls: Int,
) -> List(nono.AuditEvent) {
  case nono_bridge.find_session(command, watermark) {
    Ok(#(session_id, net_count)) ->
      case net_count > 0 {
        True -> nono_bridge.audit_events(session_id) |> result.unwrap([])
        False -> []
      }
    Error(_) ->
      case polls >= 12 {
        True -> []
        False -> {
          process.sleep(80)
          net_events_for(command, watermark, polls + 1)
        }
      }
  }
}

/// The distinct denied requests within a run's egress events — what the leash
/// gates on. Mirrors `nono_bridge.denials_of` but over already-read events.
fn denials_from(events: List(nono.AuditEvent)) -> List(nono_bridge.Denial) {
  events
  |> list.filter_map(fn(e) {
    case e.decision {
      nono.Deny ->
        Ok(nono_bridge.Denial(host: e.host, method: e.method, path: e.path))
      nono.Allow -> Error(Nil)
    }
  })
  |> list.unique
}

/// Ask the human about a denied request; map their choice to an allow rule
/// (`Ok`) or a denial (`Error`). `Allow` = the bare host; a typed `Steer`
/// = that exact rule (a path-glob); an empty `Steer` = deny.
fn net_decision(state: State, denial: nono_bridge.Denial) -> Result(String, Nil) {
  case await_net(state, denial) {
    control.Allow -> Ok(denial.host)
    control.Steer(rule) ->
      case string.trim(rule) {
        "" -> Error(Nil)
        r -> Ok(r)
      }
  }
}

/// Publish the denial with status "awaiting_net" and block on the human.
fn await_net(state: State, denial: nono_bridge.Denial) -> control.Decision {
  let step = StepNet(net_detail(denial), net_suggestion(denial))
  let activities = [step, ..state.activities]
  state.emit(
    "awaiting_net",
    list.reverse(activities),
    state.context_tokens,
    state.net_events,
  )
  state.await()
}

/// Human-readable denied request: "GET host/path" when intercepted, else "host".
fn net_detail(denial: nono_bridge.Denial) -> String {
  case denial.method, denial.path {
    Some(m), Some(p) -> m <> " " <> denial.host <> p
    _, _ -> denial.host
  }
}

/// A suggested allow rule to pre-fill the path-glob option: the directory glob
/// of the denied path when known, otherwise the bare host.
fn net_suggestion(denial: nono_bridge.Denial) -> String {
  case denial.path {
    None -> denial.host
    Some(p) -> {
      let segs = string.split(p, "/")
      let prefix = string.join(list.take(segs, list.length(segs) - 1), "/")
      "https://" <> denial.host <> prefix <> "/**"
    }
  }
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
      "blocked entirely — no outbound network. `git push`/`git fetch` over the network, package installs, and any remote fetch will fail."
    // Default-deny against the session allowlist.
    True -> {
      let hosts = case net_allow {
        [] -> "(empty — nothing is allowed yet)"
        _ -> string.join(net_allow, ", ")
      }
      "default-deny against this run's allowlist: "
      <> hosts
      <> ". A connection to any other host is blocked and pauses for human approval — propose the host to add rather than retrying."
    }
  }

  // Enabled capability groups, with nono's own descriptions where available.
  let catalog = nono_bridge.list_groups()
  let group_lines =
    list.map(groups, fn(name) {
      case list.find(catalog, fn(g) { g.name == name }) {
        Ok(g) -> name <> " — " <> g.description
        Error(_) -> name
      }
    })
  let groups_text = case group_lines {
    [] -> "none beyond the locked defaults"
    _ -> string.join(group_lines, "; ")
  }

  "\n\n# Capabilities this run\nYour actions run inside a nono sandbox with a fixed, known reach. Reason about it BEFORE acting; if a request needs access you don't have, say so plainly and propose the smallest change (a host to allowlist, a capability group to enable, or a step for the human to run outside the sandbox) instead of retrying.\n\n- Filesystem: the workspace is read-write, and the language-toolchain directories bough grants are read-only. The rest of $HOME is DENIED — credentials and keys (~/.ssh, ~/.aws, ~/.config, keychains, git signing keys) are not readable. Anything that needs them fails: a signed `git commit` (commit.gpgsign / SSH signing) and SSH-authenticated git will not work from here.\n- Network: "
  <> network
  <> "\n- Capability groups enabled: "
  <> groups_text
  <> "."
}

/// Run a command in the sandbox under the run's nono profile (§7). The profile
/// always grants read-only git config (so git steps don't abort, §5.6) and sets
/// the network posture: blocked entirely with the net gate off (§5.2), or
/// default-deny against the session allowlist with it on, so a denied request
/// can be detected and surfaced for approval.
fn sandboxed(
  state: State,
  command: List(String),
  reads: List(String),
) -> #(Int, String) {
  let block = !state.config.net_gate
  case write_net_profile(state, block) {
    Ok(path) -> nono_bridge.run_in_profile(state.workspace, path, reads, command)
    // If the profile can't be written or doesn't validate, fail safe: block the
    // network rather than run under a profile nono can't parse.
    Error(_) ->
      nono_bridge.run_result(
        nono.Profile(state.workspace, [], True, False),
        reads,
        command,
      )
  }
}

/// Write the run's nono profile, then validate it against the installed nono so
/// a malformed/drifted profile is caught here rather than failing the command
/// opaquely (SPEC §6, §7). Either failure makes the caller block the net.
fn write_net_profile(state: State, block: Bool) -> Result(String, Nil) {
  use path <- result.try(net_profile.write(
    state.net_profile_path,
    state.net_allow,
    block,
    state.groups,
    state.config.net_credentials,
  ))
  nono_bridge.validate_profile(path)
  |> result.replace(path)
  |> result.replace_error(Nil)
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
          process.sleep(250)
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

/// Save full output to a numbered blackboard file when it exceeds its digest,
/// returning a pointer suffix to append to the conversation (empty if inline).
fn maybe_save(state: State, full: String, dig: String) -> #(State, String) {
  case string.length(full) > string.length(dig) {
    False -> #(state, "")
    True -> {
      let idx = state.bb_idx + 1
      let _ = simplifile.create_directory_all(state.bb_dir)
      let path = state.bb_dir <> "/out_" <> int.to_string(idx) <> ".txt"
      case simplifile.write(path, full) {
        Ok(_) -> #(
          State(..state, bb_idx: idx),
          "\n[full output saved: " <> path <> "]",
        )
        Error(_) -> #(state, "")
      }
    }
  }
}
