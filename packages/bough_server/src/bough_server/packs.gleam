//// Allowlist *packs*: named, reusable bundles of capability groups + network
//// allow-rules a human can apply to a session up front, instead of approving
//// hosts and groups piecemeal. A pack composes with the existing profile
//// pipeline — applying one just unions its `groups`/`allow` into the session,
//// Persisted as a JSON array at `~/.bough/packs.json`.

import envoy
import gleam/dynamic/decode
import gleam/json
import gleam/list
import gleam/result
import simplifile

pub type Pack {
  Pack(
    name: String,
    description: String,
    groups: List(String),
    allow: List(String),
  )
}

fn store_path() -> Result(String, Nil) {
  use home <- result.try(envoy.get("HOME"))
  Ok(home <> "/.bough/packs.json")
}

/// Every saved pack (empty if the store is missing or unreadable).
pub fn list() -> List(Pack) {
  case store_path() {
    Error(_) -> []
    Ok(path) ->
      case simplifile.read(path) {
        Error(_) -> []
        Ok(body) ->
          json.parse(body, decode.list(decoder())) |> result.unwrap([])
      }
  }
}

pub fn get(name: String) -> Result(Pack, Nil) {
  list() |> list.find(fn(p) { p.name == name })
}

/// Upsert a pack by name.
pub fn save(pack: Pack) -> Nil {
  list()
  |> list.filter(fn(p) { p.name != pack.name })
  |> list.append([pack])
  |> write
}

pub fn delete(name: String) -> Nil {
  list() |> list.filter(fn(p) { p.name != name }) |> write
}

fn write(packs: List(Pack)) -> Nil {
  case store_path() {
    Error(_) -> Nil
    Ok(path) -> {
      let _ = simplifile.write(path, json.to_string(json.array(packs, to_json)))
      Nil
    }
  }
}

pub fn to_json(pack: Pack) -> json.Json {
  json.object([
    #("name", json.string(pack.name)),
    #("description", json.string(pack.description)),
    #("groups", json.array(pack.groups, json.string)),
    #("allow", json.array(pack.allow, json.string)),
  ])
}

pub fn decoder() -> decode.Decoder(Pack) {
  use name <- decode.field("name", decode.string)
  use description <- decode.optional_field("description", "", decode.string)
  use groups <- decode.optional_field("groups", [], decode.list(decode.string))
  use allow <- decode.optional_field("allow", [], decode.list(decode.string))
  decode.success(Pack(name:, description:, groups:, allow:))
}
