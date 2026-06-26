//// User-extensible credential/tool providers — how auth gets set up for a tool
//// the sandboxed agent uses, without baking a fixed catalog into bough.
////
//// A provider is a JSON spec at `~/.bough/providers/<name>.json` (built-in
//// defaults below are overridden by a user file of the same name). The
//// universal primitive: `prepare` runs a shell command OUTSIDE the sandbox and
//// prints `KEY=VALUE` lines; `mode` decides how the sandbox consumes them:
////
////   - "egress" — the secret (the `key` field, default `TOKEN`) is injected
////       into outbound requests to `domain` by the mitmproxy's managed-credential
////       routes. The secret NEVER enters the sandbox. For token/header
////       auth: gh, GitHub git push, bearer-token APIs.
////   - "env" — the `KEY=VALUE`s are forwarded into the sandbox env. For tools
////       that must sign locally (AWS SigV4) — pair
////       with short-lived, scoped creds so exposure is bounded.
////   - "none" — `prepare` only stood something up (e.g. `kubectl proxy`); the
////       sandbox just gets `allow`-listed access to the loopback endpoint.
////
//// Every provider requests `allow` hosts; the always-on network sandbox still
//// gates them. `prepare` runs unsandboxed by design — it is the privileged
//// setup, run in bough's own process (outside the agent's sandbox).

import envoy
import gleam/dict.{type Dict}
import gleam/dynamic/decode
import gleam/json
import gleam/list
import gleam/result
import gleam/string
import simplifile

pub type Mode {
  Egress
  Env
  EndpointOnly
}

pub type Provider {
  Provider(
    name: String,
    description: String,
    /// Hosts this provider needs the network sandbox to allow.
    allow: List(String),
    /// Hosts to TLS-passthrough (tunnel, not MITM) because the tool's client
    /// won't trust our CA — e.g. Go CLIs (gh, kubectl) and locally-signed
    /// requests (aws SigV4). Still host-gated; creds for these must reach the
    /// tool via the sandbox (env or a readable config), not proxy injection.
    passthrough: List(String),
    /// Filesystem paths the tool needs to read (e.g. its config dir). Granted
    /// via the same read + bypass carve-out as credential capabilities.
    reads: List(String),
    /// Shell run OUTSIDE the sandbox; prints `KEY=VALUE` lines.
    prepare: String,
    mode: Mode,
    /// Egress: the mitmproxy's managed-credential service name (e.g. "github").
    /// The mitmproxy injects a phantom into the sandbox and swaps it for the real
    /// secret on egress, so the secret never enters the sandbox. The `prepare`
    /// output must set the env var the mitmproxy's route reads (e.g. `GITHUB_TOKEN`).
    service: String,
  )
}

/// Built-in defaults, so the common tools work out of the box. A user file at
/// `~/.bough/providers/<name>.json` with the same name overrides the default.
pub fn builtins() -> List(Provider) {
  [
    Provider(
      name: "github",
      description: "GitHub git push + REST API. `gh` works (api.github.com is"
        <> " tunnelled, token in the sandbox env); git push/pull use proxy-side"
        <> " injection (no token in the sandbox). Egress host-gated to GitHub.",
      allow: ["github.com", "api.github.com", "codeload.github.com"],
      passthrough: ["api.github.com"],
      reads: [],
      prepare: "echo GITHUB_TOKEN=$(gh auth token)",
      mode: Egress,
      service: "github",
    ),
    Provider(
      name: "exa",
      description: "Exa web search via `restish exa ...` (search/answer/contents)."
        <> " api.exa.ai is tunnelled; restish reads its key from its own config.",
      allow: ["api.exa.ai", "raw.githubusercontent.com"],
      passthrough: ["api.exa.ai", "raw.githubusercontent.com"],
      reads: [],
      prepare: "",
      mode: EndpointOnly,
      service: "",
    ),
    Provider(
      name: "aws",
      description: "AWS CLI via short-lived STS creds (signs locally)",
      allow: ["sts.amazonaws.com"],
      passthrough: ["sts.amazonaws.com"],
      reads: [],
      prepare: "aws sts get-session-token --output text"
        <> " --query 'Credentials.[AccessKeyId,SecretAccessKey,SessionToken]'"
        <> " | awk '{print \"AWS_ACCESS_KEY_ID=\"$1\"\\nAWS_SECRET_ACCESS_KEY=\"$2\"\\nAWS_SESSION_TOKEN=\"$3}'",
      mode: Env,
      service: "",
    ),
    Provider(
      name: "kube",
      description: "kubectl via a local kubectl proxy (creds stay outside)",
      allow: ["127.0.0.1:8001"],
      passthrough: [],
      reads: [],
      prepare: "kubectl proxy --port=8001 >/dev/null 2>&1 &",
      mode: EndpointOnly,
      service: "",
    ),
  ]
}

fn dir() -> Result(String, Nil) {
  use home <- result.try(envoy.get("HOME"))
  let d = home <> "/.bough/providers"
  let _ = simplifile.create_directory_all(d)
  Ok(d)
}

/// All providers: built-ins, with any user file of the same name overriding,
/// plus user-only providers.
pub fn list() -> List(Provider) {
  let user = user_providers()
  let user_names = list.map(user, fn(p) { p.name })
  let kept = list.filter(builtins(), fn(b) { !list.contains(user_names, b.name) })
  list.append(kept, user)
}

pub fn get(name: String) -> Result(Provider, Nil) {
  list() |> list.find(fn(p) { p.name == name })
}

fn user_providers() -> List(Provider) {
  case dir() {
    Error(_) -> []
    Ok(d) ->
      case simplifile.read_directory(d) {
        Error(_) -> []
        Ok(files) ->
          files
          |> list.filter(fn(f) { string.ends_with(f, ".json") })
          |> list.filter_map(fn(f) {
            simplifile.read(d <> "/" <> f)
            |> result.replace_error(Nil)
            |> result.try(fn(body) {
              json.parse(body, decoder()) |> result.replace_error(Nil)
            })
          })
      }
  }
}

/// Run `prepare` (outside the sandbox) and parse its `KEY=VALUE` stdout lines.
/// The runner is injected so the pure wiring stays testable.
pub fn prepared(
  p: Provider,
  run: fn(String) -> Result(String, Nil),
) -> Dict(String, String) {
  case run(p.prepare) {
    Error(_) -> dict.new()
    Ok(out) -> parse_env(out)
  }
}

/// Parse `KEY=VALUE` lines (blank lines and lines without `=` are skipped).
pub fn parse_env(out: String) -> Dict(String, String) {
  out
  |> string.split("\n")
  |> list.fold(dict.new(), fn(acc, line) {
    case string.split_once(string.trim(line), "=") {
      Ok(#(k, v)) if k != "" -> dict.insert(acc, string.trim(k), v)
      _ -> acc
    }
  })
}

/// The profile pieces a set of enabled providers contributes: hosts to add to
/// the network allowlist, egress credential routes, and env var names to
/// forward into the sandbox.
pub type Applied {
  Applied(
    allow: List(String),
    reads: List(String),
    /// Managed-credential service names the mitmproxy injects on egress.
    services: List(String),
    env_allow: List(String),
  )
}

/// Run each enabled provider's `prepare` (outside the sandbox), stash any
/// secrets in bough's process env via `setenv` so the mitmproxy inherits them without
/// writing them to disk, and return the profile pieces. `run`/`setenv` are
/// injected so the wiring is testable without spawning shells or mutating env.
pub fn apply(
  names: List(String),
  run: fn(String) -> Result(String, Nil),
  setenv: fn(String, String) -> Nil,
) -> Applied {
  apply_list(list.filter_map(names, get), run, setenv)
}

pub fn apply_list(
  enabled: List(Provider),
  run: fn(String) -> Result(String, Nil),
  setenv: fn(String, String) -> Nil,
) -> Applied {
  enabled
  |> list.fold(Applied([], [], [], []), fn(acc, p: Provider) {
    let acc =
      Applied(
        ..acc,
        allow: list.append(acc.allow, p.allow),
        reads: list.append(acc.reads, p.reads),
      )
    case p.mode {
      EndpointOnly -> acc
      Env -> {
        let kv = prepared(p, run)
        list.each(dict.to_list(kv), fn(pair) { setenv(pair.0, pair.1) })
        Applied(..acc, env_allow: list.append(acc.env_allow, dict.keys(kv)))
      }
      Egress -> {
        // Set the real secret in bough's OWN env (the mitmproxy reads it outside
        // the sandbox); enabling the managed `service` makes the mitmproxy inject a phantom
        // into the sandbox and swap it on egress. The secret never enters.
        let kv = prepared(p, run)
        list.each(dict.to_list(kv), fn(pair) { setenv(pair.0, pair.1) })
        Applied(..acc, services: list.append(acc.services, [p.service]))
      }
    }
  })
}

pub fn to_json(p: Provider) -> json.Json {
  json.object([
    #("name", json.string(p.name)),
    #("description", json.string(p.description)),
    #("allow", json.array(p.allow, json.string)),
    #("mode", json.string(mode_to_string(p.mode))),
  ])
}

fn mode_to_string(m: Mode) -> String {
  case m {
    Egress -> "egress"
    Env -> "env"
    EndpointOnly -> "none"
  }
}

fn mode_decoder() -> decode.Decoder(Mode) {
  use s <- decode.then(decode.string)
  case s {
    "egress" -> decode.success(Egress)
    "env" -> decode.success(Env)
    "none" -> decode.success(EndpointOnly)
    _ -> decode.failure(Egress, "Mode")
  }
}

pub fn decoder() -> decode.Decoder(Provider) {
  use name <- decode.field("name", decode.string)
  use description <- decode.optional_field("description", "", decode.string)
  use allow <- decode.optional_field("allow", [], decode.list(decode.string))
  use passthrough <- decode.optional_field(
    "passthrough",
    [],
    decode.list(decode.string),
  )
  use reads <- decode.optional_field("reads", [], decode.list(decode.string))
  use prepare <- decode.optional_field("prepare", "", decode.string)
  use mode <- decode.optional_field("mode", EndpointOnly, mode_decoder())
  use service <- decode.optional_field("service", "", decode.string)
  decode.success(Provider(
    name:,
    description:,
    allow:,
    passthrough:,
    reads:,
    prepare:,
    mode:,
    service:,
  ))
}
