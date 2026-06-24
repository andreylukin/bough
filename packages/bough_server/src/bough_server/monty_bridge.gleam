//// Bridge to the `bough-monty` code-mode sidecar (SPEC.md §5.2). The supervisor
//// emits a Python program per round; this runs it inside a monty sandbox where
//// the only doors out are the host functions `bash`/`read`/`write`/`edit`
//// (`bash` shells through nono). The sidecar is a Rust binary embedding the
//// monty interpreter — the BEAM can't host monty in-process, so bough drives it
//// the same way it drives nono: one execve per round via `shellout`.
////
//// The program travels as a single argv element (`--code-str`), so there is no
//// shell and nothing to escape. The sidecar always exits 0 and reports
//// success/failure in its JSON, so a Python error is never confused with a
//// process failure.

import bough_server/nono_bridge
import envoy
import gleam/dynamic/decode
import gleam/json
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/result
import gleam/string
import shellout

/// Run one Python program in the monty sandbox over `workspace`. Returns an
/// exit-code-style pair (`0` on success, `1` on a Python/sidecar error) plus the
/// captured stdout (or the error text), matching the engine's `Exec` shape.
///
/// `BOUGH_MONTY_SANDBOXED=1` runs the *whole sidecar* inside one nono cell, so
/// `read`/`write`/`edit` (in-process) and any `bash` child inherit the
/// workspace + net + secret-deny policy at the kernel — closing the gap where
/// only `bash` was sandboxed (SPEC §6). Default (unset) is the legacy path: the
/// sidecar runs unsandboxed and only `bash` opens its own nono cell.
pub fn run_code(
  workspace: String,
  code: String,
  profile: Option(String),
) -> #(Int, String) {
  case sandboxed() {
    True -> run_sandboxed(workspace, code, profile)
    False -> run_bare(workspace, code, profile)
  }
}

fn sandboxed() -> Bool {
  case envoy.get("BOUGH_MONTY_SANDBOXED") {
    Ok("1") | Ok("true") -> True
    _ -> False
  }
}

/// Sandboxed: wrap the sidecar in one nono cell (via the shared `run_celled`
/// arg builder) and tell it `--bash-inherit`, so `bash` is a plain child that
/// inherits the cell rather than spawning its own.
fn run_sandboxed(
  workspace: String,
  code: String,
  profile: Option(String),
) -> #(Int, String) {
  let command = [
    resolve_binary(binary_path()), "--workspace", workspace, "--bash-inherit",
    "--code-str", code,
  ]
  let #(_exit, raw) = nono_bridge.run_celled(workspace, profile, [], command)
  parse_result(raw)
}

/// nono can't exec a symlink as its target command (it tries to `nono learn` and
/// runs nothing), so resolve to the real path. `make sidecar` installs the
/// binary as an absolute one-level symlink under `~/.bough/bin`; `readlink`
/// yields its target. A non-symlink (e.g. `BOUGH_MONTY_BIN`) returns nonzero, so
/// fall back to the path as given.
fn resolve_binary(path: String) -> String {
  case shellout.command("readlink", [path], ".", []) {
    Ok(target) -> string.trim(target)
    Error(_) -> path
  }
}

/// Legacy: exec the sidecar bare; its `bash` opens its own nono cell and
/// `read`/`write`/`edit` are scoped lexically inside the unsandboxed sidecar.
fn run_bare(
  workspace: String,
  code: String,
  profile: Option(String),
) -> #(Int, String) {
  let profile_args = case profile {
    Some(path) -> ["--nono-profile", path]
    None -> []
  }
  case
    shellout.command(
      binary_path(),
      list.append(["--workspace", workspace, "--code-str", code], profile_args),
      workspace,
      [],
    )
  {
    Ok(out) -> parse_result(out)
    Error(#(_code, out)) ->
      #(1, "monty sidecar failed to run (is bough-monty installed?): " <> out)
  }
}

/// The sidecar binary: `BOUGH_MONTY_BIN`, else the installed `~/.bough/bin`
/// copy, else bare `bough-monty` on PATH.
fn binary_path() -> String {
  case envoy.get("BOUGH_MONTY_BIN") {
    Ok(path) -> path
    Error(_) ->
      case envoy.get("HOME") {
        Ok(home) -> home <> "/.bough/bin/bough-monty"
        Error(_) -> "bough-monty"
      }
  }
}

fn parse_result(raw: String) -> #(Int, String) {
  let decoder = {
    use ok <- decode.field("ok", decode.bool)
    use output <- decode.field("output", decode.string)
    use error <- decode.field("error", decode.string)
    decode.success(#(ok, output, error))
  }
  case json.parse(result_line(raw), decoder) {
    Ok(#(True, output, _)) -> #(0, output)
    Ok(#(False, output, error)) -> #(1, join(output, error))
    Error(_) -> #(1, "could not parse monty result: " <> raw)
  }
}

/// The sidecar emits its result as a single JSON line. When wrapped in nono the
/// output can be flanked by nono's banner/audit chatter, so pick the lone line
/// that is a JSON object rather than trusting the whole capture.
fn result_line(raw: String) -> String {
  raw
  |> string.split("\n")
  |> list.map(string.trim)
  |> list.filter(fn(l) {
    string.starts_with(l, "{") && string.ends_with(l, "}")
  })
  |> list.last
  |> result.unwrap(string.trim(raw))
}

/// Combine captured stdout with the error message — the model wants both the
/// output it printed before failing and the failure itself.
fn join(output: String, error: String) -> String {
  case string.trim(output) {
    "" -> error
    _ -> output <> "\n" <> error
  }
}
