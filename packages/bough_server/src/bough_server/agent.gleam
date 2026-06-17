//// The agent loop (SPEC.md §5): send context to the provider, execute any
//// tool calls (sandboxed), append results, repeat until the model stops.
////
//// The loop records a transcript of `Step`s (assistant text, tool calls, tool
//// results) so the client can show what the agent did, not just the final
//// answer. Live streaming of these steps is a later SSE refinement.

import bough_server/anthropic
import bough_server/json_value.{type JsonValue, JArray, JObject, JString}
import bough_server/tools
import gleam/json
import gleam/list
import gleam/result
import gleam/string

const max_turns = 16

pub type Step {
  StepText(text: String)
  StepToolCall(name: String, input: String)
  StepToolResult(name: String, output: String)
}

pub type Outcome {
  Outcome(text: String, turns: Int, steps: List(Step))
}

pub fn run(
  api_key: String,
  model: String,
  workspace: String,
  system: String,
  user_prompt: String,
) -> Result(Outcome, String) {
  loop(api_key, model, workspace, system, [user_text(user_prompt)], 0, [])
}

fn loop(
  api_key: String,
  model: String,
  workspace: String,
  system: String,
  messages: List(JsonValue),
  turn: Int,
  // Newest first; reversed into the Outcome.
  steps: List(Step),
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
      let steps = case string.trim(resp.text) {
        "" -> steps
        text -> [StepText(text), ..steps]
      }

      case resp.stop_reason {
        "tool_use" -> {
          let #(result_blocks, steps) =
            list.fold(resp.tool_uses, #([], steps), fn(acc, tu) {
              let #(blocks, steps) = acc
              let input = json.to_string(json_value.to_json(tu.input))
              let output = tools.execute(tu.name, tu.input, workspace)
              let steps = [
                StepToolResult(tu.name, output),
                StepToolCall(tu.name, input),
                ..steps
              ]
              #([tool_result(tu.id, output), ..blocks], steps)
            })
          loop(
            api_key,
            model,
            workspace,
            system,
            list.append(messages, [user_blocks(list.reverse(result_blocks))]),
            turn + 1,
            steps,
          )
        }
        _ ->
          Ok(Outcome(text: resp.text, turns: turn + 1, steps: list.reverse(steps)))
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
