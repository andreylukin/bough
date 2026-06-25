//// Skills (SPEC.md §-): named, reusable instruction bundles the human can pull
//// into a run by typing `/<name>` in their message — e.g. `/exa search for X`.
//// A skill is a folder `~/.bough/skills/<name>/SKILL.md` with YAML-ish
//// frontmatter (`name`, `description`) and a markdown body of instructions.
//// When a run's message names an installed skill, the harness appends that
//// skill's body to the supervisor's system prompt for the run (see engine).

import envoy
import gleam/json
import gleam/list
import gleam/result
import gleam/string
import simplifile

pub type Skill {
  Skill(name: String, description: String)
}

pub fn to_json(s: List(Skill)) -> json.Json {
  json.array(s, fn(sk) {
    json.object([
      #("name", json.string(sk.name)),
      #("description", json.string(sk.description)),
    ])
  })
}

fn dir() -> Result(String, Nil) {
  use home <- result.try(envoy.get("HOME"))
  let d = home <> "/.bough/skills"
  let _ = simplifile.create_directory_all(d)
  Ok(d)
}

fn skill_file(d: String, name: String) -> String {
  d <> "/" <> name <> "/SKILL.md"
}

/// Installed skills (name + one-line description), for discovery/UI.
pub fn list() -> List(Skill) {
  case dir() {
    Error(_) -> []
    Ok(d) ->
      case simplifile.read_directory(d) {
        Error(_) -> []
        Ok(names) ->
          names
          |> list.filter_map(fn(name) {
            case simplifile.read(skill_file(d, name)) {
              Ok(text) -> Ok(Skill(name, description_of(text)))
              Error(_) -> Error(Nil)
            }
          })
      }
  }
}

/// The markdown body of a skill (instructions), frontmatter stripped.
pub fn load_body(name: String) -> Result(String, Nil) {
  use d <- result.try(dir())
  use text <- result.try(
    simplifile.read(skill_file(d, name)) |> result.replace_error(Nil),
  )
  Ok(strip_frontmatter(text))
}

/// The supervisor-prompt section for every installed skill the message invokes
/// via `/<name>`. Empty when none are named (or installed).
pub fn active_for(message: String) -> String {
  let installed = list()
  let named =
    list.filter(installed, fn(s) { mentions(message, s.name) })
  case named {
    [] -> ""
    _ ->
      named
      |> list.filter_map(fn(s) {
        case load_body(s.name) {
          Ok(body) ->
            Ok(
              "\n\n# Active skill: /"
              <> s.name
              <> "\nThe human invoked the `/"
              <> s.name
              <> "` skill for this task. Follow its instructions, which are"
              <> " authoritative for how to do the work (but never override the"
              <> " safety and sandbox rules above):\n\n"
              <> string.trim(body),
            )
          Error(_) -> Error(Nil)
        }
      })
      |> string.concat
  }
}

/// True when `message` contains the token `/<name>` at a word boundary.
fn mentions(message: String, name: String) -> Bool {
  let m = " " <> message <> " "
  list.any([" ", "\n", "\t"], fn(pre) {
    string.contains(m, pre <> "/" <> name <> " ")
    || string.contains(m, pre <> "/" <> name <> "\n")
  })
}

/// `description:` from the frontmatter, or "" if absent.
fn description_of(text: String) -> String {
  frontmatter_lines(text)
  |> list.filter_map(fn(line) {
    case string.split_once(line, ":") {
      Ok(#(k, v)) ->
        case string.trim(k) == "description" {
          True -> Ok(string.trim(v))
          False -> Error(Nil)
        }
      Error(_) -> Error(Nil)
    }
  })
  |> list.first
  |> result.unwrap("")
}

fn frontmatter_lines(text: String) -> List(String) {
  case string.starts_with(string.trim_start(text), "---") {
    False -> []
    True ->
      case string.split(text, "---") {
        [_, fm, ..] -> string.split(fm, "\n")
        _ -> []
      }
  }
}

fn strip_frontmatter(text: String) -> String {
  case string.starts_with(string.trim_start(text), "---") {
    False -> text
    True ->
      case string.split(text, "---") {
        [_, _fm, ..rest] -> string.trim_start(string.join(rest, "---"))
        _ -> text
      }
  }
}
