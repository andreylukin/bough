import bough_core/artifact.{Run, Write}
import bough_server/json_value.{JArray, JBool, JObject, JString}
import bough_server/tool_steps.{Parsed}
import gleam/option.{Some}

fn step(fields: List(#(String, json_value.JsonValue))) -> json_value.JsonValue {
  JObject(fields)
}

pub fn parses_a_batch_with_check_and_done_test() {
  let input =
    JObject([
      #("steps", JArray([
        step([
          #("action", JString("run")),
          #("title", JString("list")),
          #("command", JString("ls -la")),
        ]),
        step([
          #("action", JString("write")),
          #("title", JString("module")),
          #("path", JString("a.txt")),
          #("content", JString("hi")),
        ]),
      ])),
      #("check", JString("test -f a.txt")),
      #("done", JBool(False)),
    ])
  assert tool_steps.parse(input)
    == Ok(Parsed(
      [Run("list", "ls -la"), Write("module", "a.txt", "hi")],
      Some("test -f a.txt"),
      False,
    ))
}

pub fn missing_per_action_field_is_an_error_test() {
  let input =
    JObject([
      #("steps", JArray([
        step([
          #("action", JString("edit")),
          #("title", JString("fix")),
          #("path", JString("f")),
          #("find", JString("a")),
        ]),
      ])),
    ])
  assert tool_steps.parse(input)
    == Error("step 1 (edit): missing \"replace\"")
}

pub fn unknown_action_is_an_error_test() {
  let input =
    JObject([
      #("steps", JArray([
        step([#("action", JString("frobnicate")), #("title", JString("?"))]),
      ])),
    ])
  assert tool_steps.parse(input)
    == Error("step 1: unknown action \"frobnicate\" (use run/write/edit/read/grep)")
}

pub fn missing_steps_array_is_an_error_test() {
  assert tool_steps.parse(JObject([#("done", JBool(True))]))
    == Error("run_steps needs a \"steps\" array")
}
