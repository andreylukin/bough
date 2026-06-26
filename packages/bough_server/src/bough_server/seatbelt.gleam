//// Generate a macOS Seatbelt (`sandbox-exec`) profile that is bough's
//// filesystem/process sandbox for code-mode `bash` (SPEC §6). macOS-only.
////
//// Policy: allow-default reads MINUS a curated credential/secret/private
//// denylist (ported verbatim from the prior sandbox's locked deny groups), and deny-default
//// writes EXCEPT the workspace plus a curated allowlist (temp, caches,
//// toolchain dirs). The network is intentionally NOT restricted here — egress
//// is owned by the mitmproxy layer (a later phase); this module is the
//// filesystem/process half only.

import envoy
import gleam/int
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/string
import simplifile

/// Credential / secret / private paths denied for reading — ported from the
/// prior sandbox's locked groups (deny_credentials, deny_keychains_macos, deny_browser_data,
/// deny_shell_configs, deny_shell_history, deny_macos_private). `~` expands to
/// `$HOME` at generation time; `/`-rooted entries are absolute.
const deny_reads = [
  // credentials
  "~/.ssh", "~/.gnupg", "~/.aws", "~/.azure", "~/.config/gcloud", "~/.gcloud",
  "~/.kube", "~/.docker", "~/.git-credentials", "~/.netrc", "~/.npmrc",
  "~/.vault-token", "~/.credentials", "~/.secrets", "~/.keys", "~/.pki",
  "~/.terraform.d", "~/.config/op", "~/.password-store", "~/.1password",
  // keychains / password stores
  "~/Library/Keychains", "/Library/Keychains",
  "~/Library/Containers/com.1password.1password",
  "~/Library/Group Containers/2BUA8C4S2C.com.1password",
  // shell configs (may embed secrets) + history
  "~/.zshrc", "~/.zshenv", "~/.zprofile", "~/.zlogin", "~/.zlogout", "~/.bashrc",
  "~/.bash_profile", "~/.bash_login", "~/.bash_logout", "~/.profile",
  "~/.config/fish", "~/.env", "~/.envrc", "~/.bash_history", "~/.zsh_history",
  "~/.history", "~/.python_history",
  // browser data
  "~/Library/Application Support/1Password",
  "~/Library/Application Support/Arc",
  "~/Library/Application Support/BraveSoftware",
  "~/Library/Application Support/Chromium",
  "~/Library/Application Support/com.operasoftware.Opera",
  "~/Library/Application Support/Firefox",
  "~/Library/Application Support/Google/Chrome",
  "~/Library/Application Support/Microsoft Edge",
  "~/Library/Application Support/MobileSync",
  "~/Library/Application Support/Vivaldi", "~/Library/Safari",
  "~/Library/Containers/com.apple.Safari",
  // macOS private data
  "~/Library/Messages", "~/Library/Mail", "~/Library/Cookies",
]

/// Dirs outside the workspace that toolchains legitimately write to (caches,
/// temp, per-language stores). Without these, cargo/npm/go/etc. break under
/// write-confinement. Extend at runtime with `BOUGH_WRITE_ALLOW` (comma-sep)
/// when a build needs a dir not listed here.
const write_allow = [
  // temp
  "/private/tmp", "/private/var/folders", "/tmp",
  // XDG + generic caches
  "~/.cache", "~/.local/share", "~/.local/state", "~/Library/Caches",
  // rust / node / python / go / ruby / java / .net toolchains
  "~/.cargo", "~/.rustup", "~/.npm", "~/.node-gyp", "~/.yarn", "~/.pnpm-store",
  "~/.deno", "~/.bun", "~/go", "~/.gem", "~/.bundle", "~/.gradle", "~/.m2",
  "~/.ivy2", "~/.sbt", "~/.nuget", "~/.dotnet", "~/.cocoapods",
]

/// Device files processes need to write (null sink, ptys, pipes).
const dev_writes = "(literal \"/dev/null\") (literal \"/dev/zero\")"
  <> " (literal \"/dev/random\") (literal \"/dev/urandom\")"
  <> " (regex #\"^/dev/tty\") (regex #\"^/dev/fd/\") (regex #\"^/dev/stdout\")"

/// Write the generated profile to `path`, returning it. `workspace` is the
/// read-write root; `home` expands `~`. `proxy_port`, when set, locks egress to
/// just that loopback port (the session's mitmproxy); `None` leaves network
/// open (transitional).
pub fn write(
  path: String,
  workspace: String,
  home: String,
  proxy_port: Option(Int),
  extra: List(String),
) -> Result(String, Nil) {
  let env_extras = case envoy.get("BOUGH_WRITE_ALLOW") {
    Ok(s) ->
      string.split(s, ",")
      |> list.map(string.trim)
      |> list.filter(fn(d) { d != "" })
    Error(_) -> []
  }
  let extras = list.append(extra, env_extras)
  case simplifile.write(path, build(workspace, home, proxy_port, extras)) {
    Ok(_) -> Ok(path)
    Error(_) -> Error(Nil)
  }
}

/// Pure: the Seatbelt profile text (SBPL). `extras` are additional write-allowed
/// dirs (from `BOUGH_WRITE_ALLOW`).
pub fn build(
  workspace: String,
  home: String,
  proxy_port: Option(Int),
  extras: List(String),
) -> String {
  let denies =
    deny_reads
    |> list.map(fn(p) { subpath(expand(p, home)) })
    |> string.join("\n  ")
  let allows =
    [workspace, ..list.append(write_allow, extras)]
    |> list.map(fn(p) { expand(p, home) })
    |> list.map(subpath)
    |> string.join("\n  ")
  "(version 1)\n(allow default)\n\n"
  <> ";; deny reads of credential/secret/private paths (ported from the prior sandbox)\n"
  <> "(deny file-read*\n  "
  <> denies
  <> ")\n\n"
  <> ";; confine writes to the workspace + a curated allowlist\n"
  <> "(deny file-write*)\n"
  <> "(allow file-write*\n  "
  <> allows
  <> "\n  "
  <> dev_writes
  <> ")\n"
  <> network(proxy_port)
}

/// Egress is owned by the session's mitmproxy: deny all network except the
/// loopback proxy port (the agent reaches it via HTTPS_PROXY). `None` leaves
/// the default (open) — used only in the transitional phase before the proxy.
fn network(proxy_port: Option(Int)) -> String {
  case proxy_port {
    None -> ""
    Some(port) ->
      "\n;; egress only via the session mitmproxy on loopback\n"
      <> "(deny network*)\n"
      <> "(allow network-outbound (remote ip \"localhost:"
      <> int.to_string(port)
      <> "\"))\n"
  }
}

fn subpath(p: String) -> String {
  "(subpath \"" <> p <> "\")"
}

fn expand(p: String, home: String) -> String {
  case string.starts_with(p, "~") {
    True -> home <> string.drop_start(p, 1)
    False -> p
  }
}
