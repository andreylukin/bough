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
