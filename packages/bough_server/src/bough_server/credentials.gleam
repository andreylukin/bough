//// Credential capabilities: named, opt-in grants that let the agent use a
//// specific service's local credentials (e.g. `github` → the gh CLI token,
//// `aws` → `~/.aws`). Unlike nono's locked `deny_credentials` floor — which
//// blanket-blocks ssh keys, cloud creds, and tokens — a credential capability
//// punches one narrow, human-approved hole: it grants read of just that
//// service's paths (via `filesystem.read` + `bypass_protection`, which override
//// even a required deny group) and allow-lists just that service's domains.
////
//// A capability is enabled through the same gate as nono groups: a sandboxed
//// step denied a credential path pauses for the human (status "awaiting_group"),
//// who approves to enable it and retry. Enabled names persist on the session
//// alongside `groups`, so the grant survives across runs.

import gleam/list
import gleam/string

pub type Credential {
  Credential(
    name: String,
    description: String,
    /// Host paths to grant read (and bypass any deny group covering them).
    paths: List(String),
    /// Network domains to allow-list while this capability is enabled.
    domains: List(String),
  )
}

/// The built-in catalog. Names here must not collide with nono group names —
/// they share the session's capability list and are partitioned by lookup.
pub fn catalog() -> List(Credential) {
  [
    Credential(
      name: "github",
      description: "GitHub CLI auth (gh push, PRs) — reads ~/.config/gh",
      paths: ["~/.config/gh"],
      domains: ["github.com", "api.github.com"],
    ),
    Credential(
      name: "aws",
      description: "AWS credentials — reads ~/.aws",
      paths: ["~/.aws"],
      // AWS endpoints are sprawling; allow STS up front and let the net gate
      // approve further hosts on demand.
      domains: ["sts.amazonaws.com"],
    ),
  ]
}

pub fn get(name: String) -> Result(Credential, Nil) {
  catalog() |> list.find(fn(c) { c.name == name })
}

/// Split a session's capability names into nono group names and credential
/// capabilities (the ones the catalog knows).
pub fn partition(names: List(String)) -> #(List(String), List(Credential)) {
  list.fold(names, #([], []), fn(acc, name) {
    let #(groups, creds) = acc
    case get(name) {
      Ok(c) -> #(groups, [c, ..creds])
      Error(_) -> #([name, ..groups], creds)
    }
  })
}

/// Credential capabilities whose paths cover one of the denied `targets` — the
/// candidates a credential denial should offer at the gate. `home` expands `~`.
pub fn for_paths(targets: List(String), home: String) -> List(String) {
  let wanted = list.map(targets, fn(t) { expand(t, home) })
  catalog()
  |> list.filter_map(fn(c) {
    let grants = list.map(c.paths, fn(p) { expand(p, home) })
    case list.any(wanted, fn(t) { list.any(grants, fn(g) { covers(g, t) }) }) {
      True -> Ok(c.name)
      False -> Error(Nil)
    }
  })
}

fn expand(raw: String, home: String) -> String {
  raw
  |> string.replace("$HOME", home)
  |> string.replace("~", home)
}

/// True when `target` is `grant` itself or sits under it.
fn covers(grant: String, target: String) -> Bool {
  grant != ""
  && { target == grant || string.starts_with(target, grant <> "/") }
}
