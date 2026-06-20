//// Generate the run's nono profile JSON (SPEC §7). This is the single base
//// profile every sandboxed step runs under: it always grants read-only access
//// to the user's git config (the `git_config` policy group — without it git
//// aborts with exit 128, since the config lives outside the workspace) and
//// carries the network posture, either blocked outright or limited to the
//// session's allow rules.
////
//// Each network rule is either a bare host (`api.foo.com` — a CONNECT
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
/// returning it. `block` denies the network entirely (net gate off); otherwise
/// it's limited to `rules`. `groups` are the session's enabled capability groups,
/// layered on the always-on `git_config`. Errors are non-fatal to the caller (it
/// falls back to blocking the run).
pub fn write(
  path: String,
  rules: List(String),
  block: Bool,
  groups: List(String),
  credentials: List(#(String, String)),
) -> Result(String, Nil) {
  let _ = simplifile.create_directory_all(dirname(path))
  case
    simplifile.write(
      path,
      json.to_string(build(rules, block, groups, credentials)),
    )
  {
    Ok(_) -> Ok(path)
    Error(_) -> Error(Nil)
  }
}

/// A host the agent never contacts, always included so the allowlist is
/// non-empty — nono only engages proxy filtering (and thus per-host deny +
/// audit) when it is; otherwise the network would be silently unrestricted.
const sentinel = "bough.sentinel.invalid"

/// Pure: the base profile JSON. Carries the documented nono shape — versioned
/// `meta`, a `groups` include (always `git_config`, plus the session's enabled
/// `groups`), the network posture (`block` or the allowlist), and an optional
/// `env_credentials` map (SPEC §6.4).
pub fn build(
  rules: List(String),
  block: Bool,
  groups: List(String),
  credentials: List(#(String, String)),
) -> json.Json {
  let include =
    ["git_config", ..groups]
    |> list.unique
  let base = [
    #(
      "meta",
      json.object([
        #("name", json.string("bough")),
        #("version", json.string("1.0.0")),
      ]),
    ),
    #("groups", json.object([#("include", json.array(include, json.string))])),
    #("network", network(rules, block)),
  ]
  json.object(case credentials {
    [] -> base
    creds ->
      list.append(base, [#("env_credentials", credentials_object(creds))])
  })
}

/// The network section: blocked outright, or default-deny against the allow
/// rules (the sentinel keeps the allowlist non-empty so filtering engages even
/// when no host is approved yet).
fn network(rules: List(String), block: Bool) -> json.Json {
  case block {
    True -> json.object([#("block", json.bool(True))])
    False ->
      json.object([
        #(
          "allow_domain",
          json.preprocessed_array([json.string(sentinel), ..entries(rules)]),
        ),
      ])
  }
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
