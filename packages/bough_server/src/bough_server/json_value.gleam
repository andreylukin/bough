//// A round-trippable JSON value: parse arbitrary JSON, inspect it, and
//// re-encode it unchanged.
////
//// Needed for the Anthropic tool-use protocol: assistant `tool_use` blocks
//// carry arbitrary `input`, and the whole assistant turn must be echoed back
//// verbatim in the next request (SPEC.md §5).

import gleam/dict
import gleam/dynamic/decode
import gleam/json
import gleam/list
import gleam/option
import gleam/result

pub type JsonValue {
  JString(String)
  JInt(Int)
  JFloat(Float)
  JBool(Bool)
  JNull
  JArray(List(JsonValue))
  JObject(List(#(String, JsonValue)))
}

pub fn decoder() -> decode.Decoder(JsonValue) {
  // `optional` captures JSON null at any level; everything else is non-null.
  decode.optional(non_null())
  |> decode.map(fn(o) {
    case o {
      option.Some(v) -> v
      option.None -> JNull
    }
  })
}

fn non_null() -> decode.Decoder(JsonValue) {
  decode.recursive(fn() {
    decode.one_of(decode.string |> decode.map(JString), [
      decode.bool |> decode.map(JBool),
      decode.int |> decode.map(JInt),
      decode.float |> decode.map(JFloat),
      decode.list(decoder()) |> decode.map(JArray),
      decode.dict(decode.string, decoder())
        |> decode.map(fn(d) { JObject(dict.to_list(d)) }),
    ])
  })
}

pub fn to_json(value: JsonValue) -> json.Json {
  case value {
    JString(s) -> json.string(s)
    JInt(i) -> json.int(i)
    JFloat(f) -> json.float(f)
    JBool(b) -> json.bool(b)
    JNull -> json.null()
    JArray(xs) -> json.preprocessed_array(list.map(xs, to_json))
    JObject(fs) ->
      json.object(list.map(fs, fn(kv) { #(kv.0, to_json(kv.1)) }))
  }
}

/// Look up a key in an object value.
pub fn field(value: JsonValue, key: String) -> Result(JsonValue, Nil) {
  case value {
    JObject(fs) -> list.key_find(fs, key)
    _ -> Error(Nil)
  }
}

pub fn as_string(value: JsonValue) -> Result(String, Nil) {
  case value {
    JString(s) -> Ok(s)
    _ -> Error(Nil)
  }
}

/// Convenience: read a string field from an object value.
pub fn string_field(value: JsonValue, key: String) -> Result(String, Nil) {
  field(value, key) |> result.try(as_string)
}
