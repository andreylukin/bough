//// The supervisor's single tool: `run_steps` (SPEC.md §5.2). Exposed as
//// name/description/schema pieces so each `provider` can wrap them in its own
//// tool format (Anthropic `input_schema` vs OpenAI `function`). The harness
//// (engine) is what executes the batch — under monty + the seatbelt sandbox.

import gleam/json

pub const run_steps_name = "run_steps"

pub fn run_steps_description() -> String {
  "The ONLY way to act on the workspace. Provide an ordered batch of typed actions; the harness runs each and returns exit codes and output digests, then runs your check. The `code` action runs Python in a monty sandbox with host functions bash/read/write/edit — use it to inspect, edit, run, and verify."
}

/// The JSON Schema for the tool's input (the object passed as `input_schema` /
/// `parameters`).
pub fn run_steps_schema() -> json.Json {
  let str = fn(desc) {
    json.object([
      #("type", json.string("string")),
      #("description", json.string(desc)),
    ])
  }
  let step_schema =
    json.object([
      #("type", json.string("object")),
      #("properties", json.object([
        #("action", json.object([
          #("type", json.string("string")),
          #("description", json.string("Which action this step performs.")),
          #("enum", json.array(["code", "spawn", "tell", "collect"], json.string)),
        ])),
        #("title", str("Short human-readable title for this step.")),
        #("code", str("Python program run in the monty sandbox (action=code). Call the host functions bash(cmd)->str, read(path)->str, write(path, content), edit(path, old, new), and print() what you find. A monty Python subset: stdlib only (no third-party imports), and no class or match statements yet.")),
        #("task", str("Self-contained instructions for a subagent (action=spawn). Spawning is asynchronous: the subagent runs concurrently and the step returns its id.")),
        #("target", str("The subagent id (returned by an earlier spawn) to message (action=tell) or check the status of (action=collect). collect does NOT block — a finished subagent's output is delivered to you automatically.")),
        #("message", str("Context, info, or a correction to send the target subagent (action=tell). Delivered at its next round.")),
      ])),
      #("required", json.array(["action", "title"], json.string)),
    ])
  json.object([
    #("type", json.string("object")),
    #("properties", json.object([
      #("steps", json.object([
        #("type", json.string("array")),
        #("description", json.string("Actions to run, in order, this round.")),
        #("items", step_schema),
      ])),
      #("check", str(
        "Shell command that exits 0 if and only if the task's acceptance criteria hold. Commit one as soon as the task is verifiable; it re-runs every round.",
      )),
      #("done", json.object([
        #("type", json.string("boolean")),
        #("description", json.string(
          "Set true only after the check has passed and you have adversarially reviewed the result; honored only then.",
        )),
      ])),
    ])),
    #("required", json.array(["steps"], json.string)),
  ])
}
