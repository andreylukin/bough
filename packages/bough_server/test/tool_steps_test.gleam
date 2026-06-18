import bough_core/artifact.{Collect, Run, Spawn, Tell, Write}
import bough_server/json_value.{JArray, JBool, JObject, JString}
import bough_server/tool_steps.{Parsed}
import gleam/option.{None, Some}

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

pub fn parses_a_spawn_action_test() {
  let input =
    JObject([
      #("steps", JArray([
        step([
          #("action", JString("spawn")),
          #("title", JString("write tests")),
          #("task", JString("Add unit tests for the parser module.")),
        ]),
      ])),
    ])
  assert tool_steps.parse(input)
    == Ok(Parsed(
      [Spawn("write tests", "Add unit tests for the parser module.")],
      None,
      False,
    ))
}

pub fn spawn_without_task_is_an_error_test() {
  let input =
    JObject([
      #("steps", JArray([
        step([#("action", JString("spawn")), #("title", JString("x"))]),
      ])),
    ])
  assert tool_steps.parse(input) == Error("step 1 (spawn): missing \"task\"")
}

pub fn parses_tell_and_collect_test() {
  let input =
    JObject([
      #("steps", JArray([
        step([
          #("action", JString("tell")),
          #("title", JString("nudge")),
          #("target", JString("child123")),
          #("message", JString("also handle the empty case")),
        ]),
        step([
          #("action", JString("collect")),
          #("title", JString("await")),
          #("target", JString("child123")),
        ]),
      ])),
    ])
  assert tool_steps.parse(input)
    == Ok(Parsed(
      [
        Tell("nudge", "child123", "also handle the empty case"),
        Collect("await", "child123"),
      ],
      None,
      False,
    ))
}

pub fn tell_without_target_is_an_error_test() {
  let input =
    JObject([
      #("steps", JArray([
        step([
          #("action", JString("tell")),
          #("title", JString("x")),
          #("message", JString("hi")),
        ]),
      ])),
    ])
  assert tool_steps.parse(input) == Error("step 1 (tell): missing \"target\"")
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
    == Error("step 1: unknown action \"frobnicate\" (use run/write/edit/read/grep/spawn/tell/collect)")
}

pub fn missing_steps_array_is_an_error_test() {
  assert tool_steps.parse(JObject([#("done", JBool(True))]))
    == Error("run_steps needs a \"steps\" array")
}
