//// The agent loop (SPEC.md §5): send context to the provider, execute any
//// tool calls (sandboxed), append results, repeat until the model stops.
////
//// `run_streaming` calls `emit` with the running transcript after every step
//// (assistant text, tool call, tool result) so a caller can publish progress
//// as it happens; `run` is the non-streaming variant.

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
  run_streaming(api_key, model, workspace, system, user_prompt, fn(_) { Nil })
}

/// Like `run`, but invokes `emit` with the full chronological transcript after
/// each new step is produced.
pub fn run_streaming(
  api_key: String,
  model: String,
  workspace: String,
  system: String,
  user_prompt: String,
  emit: fn(List(Step)) -> Nil,
) -> Result(Outcome, String) {
  loop(api_key, model, workspace, system, [user_text(user_prompt)], 0, [], emit)
}

fn loop(
  api_key: String,
  model: String,
  workspace: String,
  system: String,
  messages: List(JsonValue),
  turn: Int,
  // Newest first; reversed when emitted / returned.
  steps: List(Step),
  emit: fn(List(Step)) -> Nil,
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
      let steps = push_text(steps, resp.text, emit)

      case resp.stop_reason {
        "tool_use" -> {
          let #(result_blocks, steps) =
            list.fold(resp.tool_uses, #([], steps), fn(acc, tu) {
              let #(blocks, steps) = acc
              let input = json.to_string(json_value.to_json(tu.input))
              let steps = emit_step(StepToolCall(tu.name, input), steps, emit)
              let output = tools.execute(tu.name, tu.input, workspace)
              let steps = emit_step(StepToolResult(tu.name, output), steps, emit)
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
            emit,
          )
        }
        _ ->
          Ok(Outcome(text: resp.text, turns: turn + 1, steps: list.reverse(steps)))
      }
    }
  }
}

fn push_text(
  steps: List(Step),
  text: String,
  emit: fn(List(Step)) -> Nil,
) -> List(Step) {
  case string.trim(text) {
    "" -> steps
    t -> emit_step(StepText(t), steps, emit)
  }
}

fn emit_step(
  step: Step,
  steps: List(Step),
  emit: fn(List(Step)) -> Nil,
) -> List(Step) {
  let steps = [step, ..steps]
  emit(list.reverse(steps))
  steps
}

// --- JSON ----------------------------------------------------------------

pub fn run_json(status: String, steps: List(Step), text: String) -> json.Json {
  json.object([
    #("status", json.string(status)),
    #("text", json.string(text)),
    #("steps", json.preprocessed_array(list.map(steps, step_to_json))),
  ])
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
