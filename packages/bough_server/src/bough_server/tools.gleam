//// The agent's tools (SPEC.md §5). `bash` runs inside a nono sandbox
//// (workspace read/write, no network). File tools currently run in-process,
//// scoped to the paths the model provides — sandboxing those is a follow-up.

import bough_core/nono
import bough_server/json_value.{type JsonValue}
import bough_server/nono_bridge
import gleam/json
import gleam/list
import gleam/string
import simplifile

/// Anthropic `tools` array describing the toolset to the model.
pub fn definitions() -> json.Json {
  json.preprocessed_array([
    tool(
      "bash",
      "Run a shell command in the sandbox (workspace read/write, no network).",
      [#("command", "The shell command to run")],
      ["command"],
    ),
    tool("read", "Read a file's contents.", [#("path", "Absolute file path")], [
      "path",
    ]),
    tool(
      "write",
      "Write (overwrite) a file.",
      [#("path", "Absolute file path"), #("content", "Full file contents")],
      ["path", "content"],
    ),
    tool(
      "edit",
      "Replace the first occurrence of a string in a file.",
      [
        #("path", "Absolute file path"),
        #("old", "Exact string to replace"),
        #("new", "Replacement string"),
      ],
      ["path", "old", "new"],
    ),
  ])
}

/// The single tool the supervisor calls to act on the workspace: an ordered
/// batch of typed actions plus an optional `check` and `done` (SPEC §5.2,
/// folded into one tool). The schema is intentionally flat — `action` is a
/// strict enum, the per-action fields are optional here and validated in
/// `tool_steps` so a missing field comes back as a tool_result the model fixes.
///
/// Exposed as name/description/schema pieces so each `provider` can wrap them in
/// its own tool format (Anthropic `input_schema` vs OpenAI `function`).
pub const run_steps_name = "run_steps"

pub fn run_steps_description() -> String {
  "The ONLY way to act on the workspace. Provide an ordered batch of typed actions; the harness runs each in the sandbox and returns exit codes and output digests, then runs your check. Use a read/grep action to inspect files before editing."
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
          #("enum", json.array(["run", "write", "edit", "read", "grep", "spawn"], json.string)),
        ])),
        #("title", str("Short human-readable title for this step.")),
        #("command", str("Shell command to run (action=run).")),
        #("path", str("File path, workspace-relative or absolute (action=write/edit/read).")),
        #("content", str("Full file content (action=write).")),
        #("find", str("Exact text to replace — must match byte-for-byte and uniquely (action=edit).")),
        #("replace", str("Replacement text (action=edit).")),
        #("range", str("Optional line range like \"10-40\" (action=read).")),
        #("pattern", str("Search pattern; recursive, line-numbered (action=grep).")),
        #("task", str("Self-contained instructions for a subagent to carry out on this workspace (action=spawn). The subagent runs its own plan to completion and its result is returned to you.")),
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

fn tool(
  name: String,
  description: String,
  properties: List(#(String, String)),
  required: List(String),
) -> json.Json {
  let props =
    json.object(
      properties
      |> list.map(fn(p) {
        #(p.0, json.object([#("type", json.string("string")), #("description", json.string(p.1))]))
      }),
    )
  json.object([
    #("name", json.string(name)),
    #("description", json.string(description)),
    #("input_schema", json.object([
      #("type", json.string("object")),
      #("properties", props),
      #("required", json.array(required, json.string)),
    ])),
  ])
}

/// Execute a tool call, returning the text result for the model.
pub fn execute(name: String, input: JsonValue, workspace: String) -> String {
  case name {
    "bash" -> run_bash(input, workspace)
    "read" -> run_read(input)
    "write" -> run_write(input)
    "edit" -> run_edit(input)
    _ -> "error: unknown tool " <> name
  }
}

fn run_bash(input: JsonValue, workspace: String) -> String {
  case json_value.string_field(input, "command") {
    Error(_) -> "error: missing 'command'"
    Ok(command) -> {
      let profile = nono.Profile(workspace, [], True, False)
      nono_bridge.run(profile, ["sh", "-c", command])
    }
  }
}

fn run_read(input: JsonValue) -> String {
  case json_value.string_field(input, "path") {
    Error(_) -> "error: missing 'path'"
    Ok(path) ->
      case simplifile.read(path) {
        Ok(contents) -> contents
        Error(e) -> "error: " <> string.inspect(e)
      }
  }
}

fn run_write(input: JsonValue) -> String {
  case
    json_value.string_field(input, "path"),
    json_value.string_field(input, "content")
  {
    Ok(path), Ok(content) ->
      case simplifile.write(path, content) {
        Ok(_) -> "wrote " <> path
        Error(e) -> "error: " <> string.inspect(e)
      }
    _, _ -> "error: missing 'path' or 'content'"
  }
}

fn run_edit(input: JsonValue) -> String {
  case
    json_value.string_field(input, "path"),
    json_value.string_field(input, "old"),
    json_value.string_field(input, "new")
  {
    Ok(path), Ok(old), Ok(new) ->
      case simplifile.read(path) {
        Error(e) -> "error: " <> string.inspect(e)
        Ok(contents) ->
          case string.contains(contents, old) {
            False -> "error: 'old' string not found in " <> path
            True ->
              case simplifile.write(path, string.replace(contents, old, new)) {
                Ok(_) -> "edited " <> path
                Error(e) -> "error: " <> string.inspect(e)
              }
          }
      }
    _, _, _ -> "error: missing 'path', 'old', or 'new'"
  }
}
