//// Generate a nono profile JSON from the session's network allow rules
//// (SPEC §7). Each rule is either a bare host (`api.foo.com` — a CONNECT
//// tunnel, all paths) or a URL path-glob (`https://api.foo.com/v1/**` — an L7
//// endpoint rule). Rules are grouped by host so multiple path rules for one
//// host **union** (nono's CLI allowlist is last-wins per host; a profile's
//// `endpoints` array is "allow if any match"). An empty rule set yields an
//// empty allowlist — default-deny, which is the whole point of the leash.

import gleam/dict
import gleam/json
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/result
import gleam/string
import simplifile

/// Write the profile for `rules` (and any injected `credentials`) to `path`,
/// returning it. Errors are non-fatal to the caller (it falls back to blocking
/// the run).
pub fn write(
  path: String,
  rules: List(String),
  credentials: List(#(String, String)),
) -> Result(String, Nil) {
  let _ = simplifile.create_directory_all(dirname(path))
  case simplifile.write(path, json.to_string(build(rules, credentials))) {
    Ok(_) -> Ok(path)
    Error(_) -> Error(Nil)
  }
}

/// A host the agent never contacts, always included so the allowlist is
/// non-empty — nono only engages proxy filtering (and thus per-host deny +
/// audit) when it is; otherwise the network would be silently unrestricted.
const sentinel = "bough.sentinel.invalid"

/// Pure: the profile JSON for a set of allow rules and injected credentials.
/// Carries the documented nono profile shape — versioned `meta`, a `groups`
/// include for git's config/credential helpers (so a sandboxed `git` works),
/// the network allowlist, and an optional `env_credentials` map (SPEC §6.4).
pub fn build(
  rules: List(String),
  credentials: List(#(String, String)),
) -> json.Json {
  let base = [
    #(
      "meta",
      json.object([
        #("name", json.string("bough-net")),
        #("version", json.string("1.0.0")),
      ]),
    ),
    // nono's documented group: grants git its config + credential-helper access
    // inside the sandbox (without it a sandboxed `git` can't read ~/.gitconfig).
    #(
      "groups",
      json.object([#("include", json.array(["git_config"], json.string))]),
    ),
    #(
      "network",
      json.object([
        #(
          "allow_domain",
          json.preprocessed_array([json.string(sentinel), ..entries(rules)]),
        ),
      ]),
    ),
  ]
  json.object(case credentials {
    [] -> base
    creds ->
      list.append(base, [#("env_credentials", credentials_object(creds))])
  })
}

/// `env_credentials` maps a credential name to the env var nono reads (outside
/// the sandbox) and injects on egress — so the raw secret never enters the
/// sandbox. e.g. `{"github_token": "GITHUB_TOKEN"}`.
fn credentials_object(creds: List(#(String, String))) -> json.Json {
  json.object(list.map(creds, fn(c) { #(c.0, json.string(c.1)) }))
}

/// Parse a `BOUGH_NET_CREDENTIALS` spec into (credential_name, env_var) pairs.
/// Comma-separated; each entry is `name=ENV_VAR` or a bare `name` (env var
/// defaults to the upper-cased name). Blanks are dropped.
pub fn parse_credentials(spec: String) -> List(#(String, String)) {
  spec
  |> string.split(",")
  |> list.map(string.trim)
  |> list.filter(fn(s) { s != "" })
  |> list.map(fn(s) {
    case string.split_once(s, "=") {
      Ok(#(name, env)) -> #(string.trim(name), string.trim(env))
      Error(_) -> #(s, string.uppercase(s))
    }
  })
}

type HostRule {
  HostRule(bare: Bool, paths: List(String))
}

fn entries(rules: List(String)) -> List(json.Json) {
  group(rules)
  |> dict.to_list
  |> list.map(fn(pair) {
    let #(host, rule) = pair
    case rule.bare {
      // A bare-host approval allows everything on the host.
      True -> json.string(host)
      False ->
        json.object([
          #("domain", json.string(host)),
          #(
            "endpoints",
            json.array(rule.paths, fn(p) {
              json.object([
                #("method", json.string("*")),
                #("path", json.string(p)),
              ])
            }),
          ),
        ])
    }
  })
}

fn group(rules: List(String)) -> dict.Dict(String, HostRule) {
  list.fold(rules, dict.new(), fn(acc, rule) {
    let #(host, path) = parse(rule)
    let existing = dict.get(acc, host) |> result.unwrap(HostRule(False, []))
    let updated = case path {
      None -> HostRule(..existing, bare: True)
      Some(p) ->
        case list.contains(existing.paths, p) {
          True -> existing
          False -> HostRule(..existing, paths: [p, ..existing.paths])
        }
    }
    dict.insert(acc, host, updated)
  })
}

/// Split a rule into its host and optional path glob.
fn parse(rule: String) -> #(String, Option(String)) {
  let stripped =
    rule
    |> string.replace("https://", "")
    |> string.replace("http://", "")
  case string.split_once(stripped, "/") {
    Ok(#(host, rest)) -> #(host, Some("/" <> rest))
    Error(_) -> #(stripped, None)
  }
}

fn dirname(path: String) -> String {
  case string.split_once(reverse(path), "/") {
    Ok(#(_, rest)) -> reverse(rest)
    Error(_) -> "."
  }
}

fn reverse(s: String) -> String {
  s |> string.to_graphemes |> list.reverse |> string.join("")
}
