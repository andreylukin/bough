//// Parse a `run_steps` tool call (SPEC §5.2) into the harness's `artifact.Step`
//// values plus the round's `check`/`done`. The Anthropic schema is flat (all
//// per-action fields optional), so the strict per-action validation lives here:
//// a malformed action yields a clear `Error` the engine hands back as a
//// tool_result for the model to correct.

import bough_core/artifact.{type Step, Edit, Grep, Read, Run, Spawn, Write}
import bough_server/json_value.{type JsonValue, JArray, JBool}
import gleam/int
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/result
import gleam/string

pub type Parsed {
  Parsed(steps: List(Step), check: Option(String), done: Bool)
}

/// Parse the `input` object of a `run_steps` tool call.
pub fn parse(input: JsonValue) -> Result(Parsed, String) {
  use steps_val <- result.try(
    json_value.field(input, "steps")
    |> result.replace_error("run_steps needs a \"steps\" array"),
  )
  use raw <- result.try(case steps_val {
    JArray(xs) -> Ok(xs)
    _ -> Error("\"steps\" must be an array")
  })
  use steps <- result.try(parse_steps(raw, 1, []))
  let check = case json_value.string_field(input, "check") {
    Ok(c) -> Some(c)
    Error(_) -> None
  }
  let done = case json_value.field(input, "done") {
    Ok(JBool(b)) -> b
    _ -> False
  }
  Ok(Parsed(steps, check, done))
}

fn parse_steps(
  raw: List(JsonValue),
  idx: Int,
  acc: List(Step),
) -> Result(List(Step), String) {
  case raw {
    [] -> Ok(list.reverse(acc))
    [s, ..rest] ->
      case parse_step(s, idx) {
        Ok(step) -> parse_steps(rest, idx + 1, [step, ..acc])
        Error(e) -> Error(e)
      }
  }
}

fn parse_step(s: JsonValue, idx: Int) -> Result(Step, String) {
  let at = "step " <> int.to_string(idx)
  use action <- result.try(
    field(s, "action") |> result.replace_error(at <> ": missing \"action\""),
  )
  let title = field(s, "title") |> result.unwrap("")
  case action {
    "run" ->
      field(s, "command")
      |> result.map(Run(title, _))
      |> result.replace_error(at <> " (run): missing \"command\"")
    "write" -> {
      use path <- result.try(req(s, "path", at, "write"))
      use content <- result.try(req(s, "content", at, "write"))
      Ok(Write(title, path, content))
    }
    "edit" -> {
      use path <- result.try(req(s, "path", at, "edit"))
      use find <- result.try(req(s, "find", at, "edit"))
      use replace <- result.try(req(s, "replace", at, "edit"))
      Ok(Edit(title, path, find, replace))
    }
    "read" -> {
      use path <- result.try(req(s, "path", at, "read"))
      Ok(Read(title, path, parse_range(field(s, "range"))))
    }
    "grep" ->
      field(s, "pattern")
      |> result.map(Grep(title, _))
      |> result.replace_error(at <> " (grep): missing \"pattern\"")
    "spawn" ->
      field(s, "task")
      |> result.map(Spawn(title, _))
      |> result.replace_error(at <> " (spawn): missing \"task\"")
    other ->
      Error(
        at
        <> ": unknown action \""
        <> other
        <> "\" (use run/write/edit/read/grep/spawn)",
      )
  }
}

fn field(s: JsonValue, key: String) -> Result(String, Nil) {
  json_value.string_field(s, key)
}

fn req(
  s: JsonValue,
  key: String,
  at: String,
  action: String,
) -> Result(String, String) {
  field(s, key)
  |> result.replace_error(at <> " (" <> action <> "): missing \"" <> key <> "\"")
}

fn parse_range(r: Result(String, Nil)) -> Option(#(Int, Int)) {
  case r {
    Error(_) -> None
    Ok(s) ->
      case string.split(s, "-") {
        [a, b] ->
          case int.parse(string.trim(a)), int.parse(string.trim(b)) {
            Ok(x), Ok(y) -> Some(#(x, y))
            _, _ -> None
          }
        _ -> None
      }
  }
}
