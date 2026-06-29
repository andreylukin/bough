import bough_core/artifact.{
  Code, Collect, Delegate, Request, Run, Spawn, Tell, Write,
}
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

pub fn parses_a_code_action_test() {
  let input =
    JObject([
      #("steps", JArray([
        step([
          #("action", JString("code")),
          #("title", JString("inspect")),
          #("code", JString("print(read('a.txt'))")),
        ]),
      ])),
    ])
  assert tool_steps.parse(input)
    == Ok(Parsed([Code("inspect", "print(read('a.txt'))")], None, False))
}

pub fn parses_a_request_action_test() {
  let input =
    JObject([
      #("steps", JArray([
        step([
          #("action", JString("request")),
          #("title", JString("enable github to push")),
          #("capability", JString("github")),
        ]),
      ])),
    ])
  assert tool_steps.parse(input)
    == Ok(Parsed([Request("enable github to push", "github")], None, False))
}

pub fn request_without_capability_is_an_error_test() {
  let input =
    JObject([
      #("steps", JArray([
        step([#("action", JString("request")), #("title", JString("x"))]),
      ])),
    ])
  assert tool_steps.parse(input)
    == Error("step 1 (request): missing \"capability\"")
}

pub fn code_without_code_is_an_error_test() {
  let input =
    JObject([
      #("steps", JArray([
        step([#("action", JString("code")), #("title", JString("x"))]),
      ])),
    ])
  assert tool_steps.parse(input) == Error("step 1 (code): missing \"code\"")
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

pub fn parses_a_delegate_action_test() {
  let input =
    JObject([
      #("steps", JArray([
        step([
          #("action", JString("delegate")),
          #("title", JString("fix factorial")),
          #("task", JString("In math.py make factorial(0) return 1.")),
          #("check", JString("python3 -c 'import math; assert math.factorial(0)==1'")),
        ]),
      ])),
    ])
  assert tool_steps.parse(input)
    == Ok(Parsed(
      [
        Delegate(
          "fix factorial",
          "In math.py make factorial(0) return 1.",
          "python3 -c 'import math; assert math.factorial(0)==1'",
        ),
      ],
      None,
      False,
    ))
}

pub fn delegate_without_check_is_an_error_test() {
  let input =
    JObject([
      #("steps", JArray([
        step([
          #("action", JString("delegate")),
          #("title", JString("x")),
          #("task", JString("do a thing")),
        ]),
      ])),
    ])
  assert tool_steps.parse(input) == Error("step 1 (delegate): missing \"check\"")
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
    == Error(
      "step 1: unknown action \"frobnicate\" (use code/delegate/spawn/tell/collect/request)",
    )
}

pub fn missing_steps_array_is_an_error_test() {
  assert tool_steps.parse(JObject([#("done", JBool(True))]))
    == Error("run_steps needs a \"steps\" array")
}
