//// The agent loop (SPEC.md §5): send context to the provider, execute any
//// tool calls (sandboxed), append results, repeat until the model stops.
////
//// `run_streaming` calls `emit` with the running transcript after every step
//// (assistant text, tool call, tool result) so a caller can publish progress
//// as it happens; `run` is the non-streaming variant.

import bough_core/nono.{type AuditEvent, Allow}
import bough_server/anthropic
import bough_server/json_value.{type JsonValue, JArray, JObject, JString}
import bough_server/tools
import gleam/int
import gleam/json
import gleam/list
import gleam/option
import gleam/result
import gleam/string

pub type Step {
  StepText(text: String)
  StepToolCall(name: String, input: String)
  StepToolResult(name: String, output: String)
  // Phased events from the supervisor-worker engine (SPEC §5), so the TUI can
  // render each role distinctly instead of a flat tool stream.
  /// Supervisor prose (plans, narration).
  StepPlan(text: String)
  /// A harness step starting: the verb (RUN/WRITE/EDIT/READ/GREP), its arg
  /// (command/path/pattern), and `detail` — the full content for WRITE or the
  /// find/replace for EDIT, so a client can show the complete plan. Empty
  /// otherwise.
  StepCall(verb: String, arg: String, detail: String)
  /// A harness step's result: its exit code and an output digest.
  StepExec(verb: String, exit: Int, digest: String)
  /// A local-worker fix attempt: `brief` is what the supervisor handed the
  /// worker (the failing step + its error — the "plan"), `command` is the fix
  /// the worker proposed, `exit` is that command's exit code.
  StepWorker(brief: String, command: String, exit: Int)
  /// The deterministic CHECK result (ground truth for completion).
  StepCheck(ok: Bool, digest: String)
  /// An adversarial-review event before `DONE` is accepted.
  StepReview(note: String)
  /// The plan-review gate: a proposed batch of steps awaiting the human's
  /// approval before the harness runs it (SPEC §5.4). `plan` is the rendered
  /// summary; the run's status is "awaiting_plan" while this is live.
  StepAwait(plan: String)
  /// The network gate (SPEC §7): a sandboxed command was denied a request,
  /// awaiting the human's decision. `detail` is the human-readable denied
  /// request (e.g. "GET api.foo.com/v1/x"); `rule` is a suggested allow rule to
  /// pre-fill for the path-glob option. Run status is "awaiting_net".
  StepNet(detail: String, rule: String)
  /// The capability-group gate (SPEC §7): a sandboxed step was denied
  /// filesystem access that an enableable group would grant, awaiting the
  /// human's decision. `detail` is the denied path(s); `groups` is the
  /// comma-joined candidate group name(s) to enable on approval / pre-fill for
  /// the edit option. Run status is "awaiting_group".
  StepGroup(detail: String, groups: String)
}

pub type Outcome {
  Outcome(
    text: String,
    turns: Int,
    steps: List(Step),
    /// Tokens occupying the supervisor's context after the latest turn.
    context_tokens: Int,
    /// The network allowlist after this run — grown by any hosts approved
    /// during it, so the session can persist them (SPEC §7).
    net_allow: List(String),
    /// Every egress event the sandbox observed this run (allow/deny), for the
    /// network dock's live feed (SPEC §7). Chronological, oldest first.
    net_events: List(AuditEvent),
    /// Capability groups the worker suggested enabling during this run (advisory).
    suggested: List(String),
    /// Capability groups enabled during this run — the human approved them at the
    /// group gate (SPEC §7), so the session should persist them.
    groups: List(String),
  )
}

pub fn run(
  api_key: String,
  model: String,
  workspace: String,
  system: String,
  history: List(#(String, String)),
  user_prompt: String,
  max_turns: Int,
) -> Result(Outcome, String) {
  run_streaming(
    api_key,
    model,
    workspace,
    system,
    history,
    user_prompt,
    max_turns,
    fn(_) { Nil },
  )
}

/// Like `run`, but invokes `emit` with the full chronological transcript after
/// each new step is produced. `history` is the prior conversation along the
/// active branch as `#(role, content)` pairs (role "user"/"assistant"), which
/// is what gives continued and forked sessions their memory.
pub fn run_streaming(
  api_key: String,
  model: String,
  workspace: String,
  system: String,
  history: List(#(String, String)),
  user_prompt: String,
  max_turns: Int,
  emit: fn(List(Step)) -> Nil,
) -> Result(Outcome, String) {
  let messages =
    list.append(history_to_messages(history), [user_text(user_prompt)])
  loop(api_key, model, workspace, system, messages, 0, max_turns, [], emit)
}

fn history_to_messages(history: List(#(String, String))) -> List(JsonValue) {
  list.map(history, fn(turn) {
    let #(role, content) = turn
    case role {
      "assistant" -> assistant_text(content)
      _ -> user_text(content)
    }
  })
}

fn loop(
  api_key: String,
  model: String,
  workspace: String,
  system: String,
  messages: List(JsonValue),
  turn: Int,
  max_turns: Int,
  // Newest first; reversed when emitted / returned.
  steps: List(Step),
  emit: fn(List(Step)) -> Nil,
) -> Result(Outcome, String) {
  case turn >= max_turns {
    // Don't discard the work done so far: end the run with the transcript
    // intact plus a note, instead of erroring it away.
    True -> {
      let steps =
        emit_step(
          StepText(
            "⚠ stopped: reached the "
            <> int.to_string(max_turns)
            <> "-turn limit (raise BOUGH_MAX_TURNS to allow more)",
          ),
          steps,
          emit,
        )
      Ok(Outcome(
        text: "stopped at the turn limit",
        turns: turn,
        steps: list.reverse(steps),
        context_tokens: 0,
        net_allow: [],
        net_events: [],
        suggested: [],
        groups: [],
      ))
    }
    False -> {
      use resp <- result.try(anthropic.complete(
        api_key,
        model,
        system,
        messages,
        tools.definitions(),
      ))
      let messages = list.append(messages, [assistant_msg(resp.content)])
      let steps = push_text(steps, resp.text, emit)

      case resp.stop_reason {
        "tool_use" -> {
          let #(result_blocks, steps) =
            list.fold(resp.tool_uses, #([], steps), fn(acc, tu) {
              let #(blocks, steps) = acc
              let input = json.to_string(json_value.to_json(tu.input))
              let steps = emit_step(StepToolCall(tu.name, input), steps, emit)
              let output = tools.execute(tu.name, tu.input, workspace)
              let steps = emit_step(StepToolResult(tu.name, output), steps, emit)
              #([tool_result(tu.id, output), ..blocks], steps)
            })
          loop(
            api_key,
            model,
            workspace,
            system,
            list.append(messages, [user_blocks(list.reverse(result_blocks))]),
            turn + 1,
            max_turns,
            steps,
            emit,
          )
        }
        _ ->
          Ok(Outcome(
            text: resp.text,
            turns: turn + 1,
            steps: list.reverse(steps),
            context_tokens: 0,
            net_allow: [],
            net_events: [],
            suggested: [],
            groups: [],
          ))
      }
    }
  }
}

fn push_text(
  steps: List(Step),
  text: String,
  emit: fn(List(Step)) -> Nil,
) -> List(Step) {
  case string.trim(text) {
    "" -> steps
    t -> emit_step(StepText(t), steps, emit)
  }
}

fn emit_step(
  step: Step,
  steps: List(Step),
  emit: fn(List(Step)) -> Nil,
) -> List(Step) {
  let steps = [step, ..steps]
  emit(list.reverse(steps))
  steps
}

// --- JSON ----------------------------------------------------------------

pub fn run_json(
  status: String,
  steps: List(Step),
  text: String,
  context_tokens: Int,
  net_events: List(AuditEvent),
) -> json.Json {
  json.object([
    #("status", json.string(status)),
    #("text", json.string(text)),
    #("steps", json.preprocessed_array(list.map(steps, step_to_json))),
    #("context_tokens", json.int(context_tokens)),
    #("network", json.preprocessed_array(list.map(net_events, audit_to_json))),
  ])
}

/// One egress event serialized for the network dock. `decision` is "allow" or
/// "deny"; `method`/`path`/`reason` are null when nono saw only the CONNECT.
pub fn audit_to_json(e: AuditEvent) -> json.Json {
  let decision = case e.decision {
    Allow -> "allow"
    nono.Deny -> "deny"
  }
  let opt = fn(o) {
    case o {
      option.Some(s) -> json.string(s)
      option.None -> json.null()
    }
  }
  json.object([
    #("host", json.string(e.host)),
    #("port", json.int(e.port)),
    #("method", opt(e.method)),
    #("path", opt(e.path)),
    #("decision", json.string(decision)),
    #("reason", opt(e.reason)),
    #("timestamp", json.int(e.timestamp)),
  ])
}

/// A step serialized to a compact JSON string — used to persist run activities
/// as display-only `ToolResult` tree entries (same shape the TUI already
/// decodes for the live chat).
pub fn step_json_string(step: Step) -> String {
  json.to_string(step_to_json(step))
}

pub fn step_to_json(step: Step) -> json.Json {
  case step {
    StepText(text) ->
      json.object([#("type", json.string("text")), #("text", json.string(text))])
    StepToolCall(name, input) ->
      json.object([
        #("type", json.string("tool")),
        #("name", json.string(name)),
        #("input", json.string(input)),
      ])
    StepToolResult(name, output) ->
      json.object([
        #("type", json.string("result")),
        #("name", json.string(name)),
        #("output", json.string(output)),
      ])
    StepPlan(text) ->
      json.object([#("type", json.string("plan")), #("text", json.string(text))])
    StepCall(verb, arg, detail) ->
      json.object([
        #("type", json.string("call")),
        #("verb", json.string(verb)),
        #("arg", json.string(arg)),
        #("detail", json.string(detail)),
      ])
    StepExec(verb, exit, digest) ->
      json.object([
        #("type", json.string("exec")),
        #("verb", json.string(verb)),
        #("exit", json.int(exit)),
        #("digest", json.string(digest)),
      ])
    StepWorker(brief, command, exit) ->
      json.object([
        #("type", json.string("worker")),
        #("brief", json.string(brief)),
        #("command", json.string(command)),
        #("exit", json.int(exit)),
      ])
    StepCheck(ok, digest) ->
      json.object([
        #("type", json.string("check")),
        #("ok", json.bool(ok)),
        #("digest", json.string(digest)),
      ])
    StepReview(note) ->
      json.object([
        #("type", json.string("review")),
        #("note", json.string(note)),
      ])
    StepAwait(plan) ->
      json.object([
        #("type", json.string("await")),
        #("plan", json.string(plan)),
      ])
    StepNet(detail, rule) ->
      json.object([
        #("type", json.string("net")),
        #("detail", json.string(detail)),
        #("rule", json.string(rule)),
      ])
    StepGroup(detail, groups) ->
      json.object([
        #("type", json.string("group")),
        #("detail", json.string(detail)),
        #("groups", json.string(groups)),
      ])
  }
}

// --- Message construction (as JsonValue) ---------------------------------

fn user_text(text: String) -> JsonValue {
  JObject([#("role", JString("user")), #("content", JString(text))])
}

fn assistant_text(text: String) -> JsonValue {
  JObject([#("role", JString("assistant")), #("content", JString(text))])
}

fn assistant_msg(content: List(JsonValue)) -> JsonValue {
  JObject([#("role", JString("assistant")), #("content", JArray(content))])
}

fn user_blocks(blocks: List(JsonValue)) -> JsonValue {
  JObject([#("role", JString("user")), #("content", JArray(blocks))])
}

fn tool_result(tool_use_id: String, output: String) -> JsonValue {
  JObject([
    #("type", JString("tool_result")),
    #("tool_use_id", JString(tool_use_id)),
    #("content", JString(output)),
  ])
}
