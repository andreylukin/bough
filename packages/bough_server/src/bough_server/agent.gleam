//// The agent's run vocabulary: the `Step`/`Outcome` types the supervisor-worker
//// engine emits (SPEC §5), and their JSON serialization for the run store and
//// the web/TUI clients. The loop itself lives in `engine`.

import bough_core/nono.{type AuditEvent, Allow}
import gleam/json
import gleam/list
import gleam/option

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

