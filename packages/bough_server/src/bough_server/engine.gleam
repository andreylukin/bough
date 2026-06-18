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
  StepExec, StepNet, StepPlan, StepReview, StepText, StepWorker,
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
  )
}

/// The subagent operations the engine delegates to the caller (the router wires
/// them to real sessions). Spawning is async — `spawn` returns the new child's
/// id immediately; `tell` sends a running child a message; `collect` waits for a
/// child and returns its result.
pub type Subagents {
  Subagents(
    spawn: fn(String, String) -> String,
    tell: fn(String, String) -> String,
    collect: fn(String) -> String,
  )
}

/// A no-op set, for runs that can't host subagents (the non-streaming path).
pub fn no_subagents() -> Subagents {
  let unavailable = "subagents are only available in a streaming run"
  Subagents(
    spawn: fn(_, _) { unavailable },
    tell: fn(_, _) { unavailable },
    collect: fn(_) { unavailable },
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
    emit: fn(String, List(Activity), Int) -> Nil,
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
    // The network allowlist for sandboxed commands; grows as hosts are approved.
    net_allow: List(String),
    // Where the generated nono network profile for this run is written.
    net_profile_path: String,
    // activities newest-first; reversed on emit/return
    activities: List(Activity),
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
  emit: fn(String, List(Activity), Int) -> Nil,
  await: fn() -> control.Decision,
  inbox: fn() -> Option(String),
  subagents: Subagents,
  net_allow: List(String),
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
      net_allow: net_allow,
      net_profile_path: dir <> "/net.json",
      activities: [],
    )
  let #(state, rounds) = run_rounds(state, 1)
  Ok(Outcome(
    text: last_assistant_text(state.config.provider, state.convo),
    turns: rounds,
    steps: list.reverse(state.activities),
    context_tokens: state.context_tokens,
    net_allow: state.net_allow,
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
) -> Result(Outcome, String) {
  run_streaming(
    api_key,
    sup_model,
    workspace,
    config,
    history,
    user_prompt,
    fn(_, _, _) { Nil },
    fn() { control.Allow },
    fn() { None },
    no_subagents(),
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
              let state = case string.trim(reply.text) {
                "" -> state
                p -> emit_activity(state, StepPlan(p))
              }
              case find_run_steps(reply.tool_uses) {
                // No tool call → a conversational reply ends the turn.
                None -> #(state, round)
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
                        False -> run_steps_round(state, round, parsed, tu.id)
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
            "" -> "The human rejected this plan. Reconsider and propose a different approach."
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
  state.emit("awaiting_plan", list.reverse(activities), state.context_tokens)
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
    push_message(state, provider.tool_result(state.config.provider, tool_id, feedback))
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
    True -> #(emit_activity(state, StepReview("accepted → DONE")), round)
    False ->
      case state.check == None && state.steps_done == 0 {
        // done used loosely on a non-task turn — just a reply.
        True -> #(state, round)
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
          let state = emit_activity(state, StepCall(verb, step_arg(step)))
          let #(state, result, fixes) = apply_with_fixes(state, step)
          let dig = digest.digest(result.output, state.config.digest_limit)
          let #(state, pointer) = maybe_save(state, result.output, dig)
          let state = emit_activity(state, StepExec(verb, result.exit, dig <> pointer))
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
    result.exit != 0
    && fixes < state.config.fix_attempts
    && budget_left(state)
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
    Spawn(title, task) -> #(bump(state), Exec(0, state.subagents.spawn(title, task)))
    Tell(_, target, message) -> #(bump(state), Exec(0, state.subagents.tell(target, message)))
    Collect(_, target) -> #(bump(state), Exec(0, state.subagents.collect(target)))
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
  let #(code, out) = sandboxed(state, command)
  let state = bump(state)
  case state.config.net_gate && tries < net_max_retries {
    False -> #(state, Exec(code, out))
    True ->
      case net_denials(command, watermark, 0) {
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

/// Denied requests for the run just performed. Polls until the run's audit
/// session appears (nono flushes it shortly after the command exits) so a
/// denial isn't missed by reading too early; then inspects it only if it had
/// network events. Bounded (~1s) — if the session never shows, assume no denial.
fn net_denials(
  command: List(String),
  watermark: String,
  polls: Int,
) -> List(nono_bridge.Denial) {
  case nono_bridge.find_session(command, watermark) {
    Ok(#(session_id, net_count)) ->
      case net_count > 0 {
        True -> nono_bridge.denials_of(session_id)
        False -> []
      }
    Error(_) ->
      case polls >= 12 {
        True -> []
        False -> {
          process.sleep(80)
          net_denials(command, watermark, polls + 1)
        }
      }
  }
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
  state.emit("awaiting_net", list.reverse(activities), state.context_tokens)
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
  let exec = case simplifile.write(resolved, content) {
    Ok(_) ->
      Exec(
        0,
        "wrote "
          <> resolved
          <> " ("
          <> int.to_string(string.length(content))
          <> " chars)",
      )
    Error(e) -> Exec(1, "write failed: " <> string.inspect(e))
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
    Error(e) -> Exec(1, "edit: cannot read " <> resolved <> ": " <> string.inspect(e))
    Ok(contents) ->
      case occurrences(contents, search) {
        0 -> Exec(1, "edit: search text not found in " <> resolved)
        1 ->
          case simplifile.write(resolved, string.replace(contents, search, replace)) {
            Ok(_) -> Exec(0, "edited " <> resolved <> " (1 replacement)")
            Error(e) -> Exec(1, "edit: write failed: " <> string.inspect(e))
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
  let #(code, out) = sandboxed(state, ["sh", "-c", cmd])
  #(bump(state), Exec(code, out))
}

fn grep(state: State, pattern: String) -> #(State, Exec) {
  let escaped = string.replace(pattern, "'", "'\\''")
  let cmd = "grep -rnI -- '" <> escaped <> "' . | head -n 200 || true"
  let #(code, out) = sandboxed(state, ["sh", "-c", cmd])
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
          let #(code, out) =
            sandboxed(state, ["sh", "-c", check])
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
      let mutated = integrity.changed_preexisting(state.workspace, state.baseline)
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
    prompts.supervisor_system(state.workspace, state.instructions),
    state.convo,
    // The supervisor acts only through the run_steps tool (§5.2).
    tools.run_steps_name,
    tools.run_steps_description(),
    tools.run_steps_schema(),
  )
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

/// Run a command in the sandbox. With the net gate off, the network is blocked
/// entirely (§5.2); with it on, the run uses a generated nono profile that is
/// default-deny against the session allowlist (§7), so a denied request can be
/// detected and surfaced for approval.
fn sandboxed(state: State, command: List(String)) -> #(Int, String) {
  case state.config.net_gate {
    False ->
      nono_bridge.run_result(
        nono.Profile(state.workspace, [], True, False),
        command,
      )
    True ->
      case net_profile.write(state.net_profile_path, state.net_allow) {
        Ok(path) -> nono_bridge.run_in_profile(state.workspace, path, command)
        // If the profile can't be written, fail safe: block the network.
        Error(_) ->
          nono_bridge.run_result(
            nono.Profile(state.workspace, [], True, False),
            command,
          )
      }
  }
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
  state.emit("running", list.reverse(activities), state.context_tokens)
  State(..state, activities: activities)
}

fn notice(state: State, text: String) -> State {
  emit_activity(state, StepText("⚠ " <> text))
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
        Ok(_) -> #(State(..state, bb_idx: idx), "\n[full output saved: " <> path <> "]")
        Error(_) -> #(state, "")
      }
    }
  }
}
