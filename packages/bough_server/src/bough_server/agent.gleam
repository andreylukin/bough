//// The agent loop (SPEC.md §5): send context to the provider, execute any
//// tool calls (sandboxed), append results, repeat until the model stops.

import bough_server/anthropic
import bough_server/json_value.{type JsonValue, JArray, JObject, JString}
import bough_server/tools
import gleam/list
import gleam/result

const max_turns = 16

pub type Outcome {
  Outcome(text: String, turns: Int)
}

pub fn run(
  api_key: String,
  model: String,
  workspace: String,
  system: String,
  user_prompt: String,
) -> Result(Outcome, String) {
  loop(api_key, model, workspace, system, [user_text(user_prompt)], 0)
}

fn loop(
  api_key: String,
  model: String,
  workspace: String,
  system: String,
  messages: List(JsonValue),
  turn: Int,
) -> Result(Outcome, String) {
  case turn >= max_turns {
    True -> Error("agent exceeded max turns")
    False -> {
      use resp <- result.try(anthropic.complete(
        api_key,
        model,
        system,
        messages,
        tools.definitions(),
      ))
      let messages = list.append(messages, [assistant_msg(resp.content)])

      case resp.stop_reason {
        "tool_use" -> {
          let results =
            list.map(resp.tool_uses, fn(tu) {
              tool_result(tu.id, tools.execute(tu.name, tu.input, workspace))
            })
          loop(
            api_key,
            model,
            workspace,
            system,
            list.append(messages, [user_blocks(results)]),
            turn + 1,
          )
        }
        _ -> Ok(Outcome(text: resp.text, turns: turn + 1))
      }
    }
  }
}

// --- Message construction (as JsonValue) ---------------------------------

fn user_text(text: String) -> JsonValue {
  JObject([#("role", JString("user")), #("content", JString(text))])
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
