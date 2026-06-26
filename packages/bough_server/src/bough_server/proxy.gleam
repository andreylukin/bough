//// Per-workspace mitmproxy lifecycle. For each code-mode workspace, bough runs
//// a `mitmdump` (with the bough_proxy addon) that gates egress and injects
//// credentials OUTSIDE the agent's sandbox. State lives under
//// `~/.bough/proxy/<key>/` (config.json, pid, log); the proxy is reused while
//// alive. macOS/dev-oriented: spawned detached via the shell (not linked to the
//// BEAM), so `stop` or a restart sweeps it.

import envoy
import gleam/int
import gleam/list
import gleam/result
import gleam/string
import shellout
import simplifile

/// Ensure a proxy is running for `workspace`, configured by `config_json`
/// (`{allow, inject}`) with `secrets` in its env. Returns the loopback port the
/// sandbox should use as its proxy, or Error if it can't be started.
pub fn ensure(
  workspace: String,
  config_json: String,
  secrets: List(#(String, String)),
) -> Result(Int, Nil) {
  let dir = key_dir(workspace)
  let _ = simplifile.create_directory_all(dir)
  // Write the config first so a reused proxy picks up allowlist changes (the
  // addon re-reads on mtime change).
  let _ = simplifile.write(dir <> "/config.json", config_json)
  let port = port_for(workspace)
  case alive(dir) && listening(port) {
    True -> Ok(port)
    False -> start(dir, port, secrets)
  }
}

/// Stop the proxy for `workspace` (best-effort).
pub fn stop(workspace: String) -> Nil {
  stop_dir(key_dir(workspace))
}

fn stop_dir(dir: String) -> Nil {
  case simplifile.read(dir <> "/pid") {
    Ok(pid) -> {
      let _ = shellout.command("kill", [string.trim(pid)], ".", [])
      Nil
    }
    Error(_) -> Nil
  }
}

/// Sweep any proxies left over from a previous bough process (crash or restart):
/// kill each tracked pid and clear the state dir. Call once at server start —
/// fresh runs re-spawn their proxies on demand, so nothing should still be live.
pub fn cleanup_all() -> Nil {
  let root = root_dir()
  case simplifile.read_directory(root) {
    Ok(keys) -> list.each(keys, fn(k) { stop_dir(root <> "/" <> k) })
    Error(_) -> Nil
  }
  let _ = simplifile.delete(root)
  Nil
}

fn root_dir() -> String {
  let home = envoy.get("HOME") |> result.unwrap("/tmp")
  home <> "/.bough/proxy"
}

fn start(
  dir: String,
  port: Int,
  secrets: List(#(String, String)),
) -> Result(Int, Nil) {
  // Clear any stale/dead pid on this slot.
  let _ =
    shellout.command(
      "sh",
      ["-c", "kill $(cat " <> dir <> "/pid 2>/dev/null) 2>/dev/null; true"],
      ".",
      [],
    )
  // Secrets go into bough's own env so the spawned mitmdump inherits them
  // without exposing them on the command line — the sandbox never sees them.
  list.each(secrets, fn(s) { envoy.set(s.0, s.1) })
  let cmd =
    "BOUGH_PROXY_CONFIG="
    <> dir
    <> "/config.json mitmdump --listen-port "
    <> int.to_string(port)
    <> " -s "
    <> addon_path()
    <> " >"
    <> dir
    <> "/log 2>&1 & echo $!"
  case shellout.command("sh", ["-c", cmd], ".", []) {
    Ok(pid) -> {
      let _ = simplifile.write(dir <> "/pid", string.trim(pid))
      case wait_listening(port, 40) {
        True -> Ok(port)
        False -> Error(Nil)
      }
    }
    Error(_) -> Error(Nil)
  }
}

/// The bough mitmproxy addon. Override with `BOUGH_PROXY_ADDON`; defaults to the
/// package's priv copy (relative to the server's cwd).
fn addon_path() -> String {
  case envoy.get("BOUGH_PROXY_ADDON") {
    Ok(p) -> p
    Error(_) -> "priv/proxy/bough_proxy.py"
  }
}

fn key_dir(workspace: String) -> String {
  root_dir() <> "/" <> int.to_string(hash(workspace))
}

/// A stable loopback port per workspace (9000–9999).
fn port_for(workspace: String) -> Int {
  9000 + hash(workspace) % 1000
}

/// Small deterministic hash of the workspace path.
fn hash(s: String) -> Int {
  s
  |> string.to_utf_codepoints
  |> list.fold(7, fn(acc, cp) { acc * 31 + string.utf_codepoint_to_int(cp) })
  |> int.absolute_value
}

fn alive(dir: String) -> Bool {
  case simplifile.read(dir <> "/pid") {
    Ok(pid) ->
      shellout.command("kill", ["-0", string.trim(pid)], ".", [])
      |> result.is_ok
    Error(_) -> False
  }
}

fn listening(port: Int) -> Bool {
  shellout.command(
    "sh",
    [
      "-c",
      "lsof -nP -iTCP:" <> int.to_string(port) <> " -sTCP:LISTEN >/dev/null 2>&1",
    ],
    ".",
    [],
  )
  |> result.is_ok
}

fn wait_listening(port: Int, tries: Int) -> Bool {
  case tries <= 0 {
    True -> False
    False ->
      case listening(port) {
        True -> True
        False -> {
          let _ = shellout.command("sleep", ["0.25"], ".", [])
          wait_listening(port, tries - 1)
        }
      }
  }
}
