//! bough-monty — the code-mode sidecar (SPEC.md §5.2).
//!
//! bough's supervisor no longer emits a typed batch of file/shell actions; it
//! writes one Python program per round, and this binary runs it inside a
//! [monty](https://github.com/pydantic/monty) sandbox. monty is a Rust Python
//! interpreter that can touch *nothing* on the host except the host functions
//! we hand it — so the agent's only doors out are the four functions below.
//!
//! Two sandboxes meet here (SPEC.md §6): monty confines the agent-authored
//! Python (no imports of the OS, no sockets, resource-limited), and nono
//! confines what the *process* can touch at the kernel level. There are two
//! enforcement layouts:
//!
//!   * Sandboxed (`--bash-inherit`, driven by `BOUGH_MONTY_SANDBOXED=1`): the
//!     whole sidecar already runs inside one nono cell, so `read`/`write`/`edit`
//!     (in-process) *and* any `bash` child inherit the workspace + network +
//!     secret-deny policy. `bash` is then a plain subprocess — no nested nono.
//!   * Legacy: the sidecar runs unsandboxed, only `bash` opens its own nono
//!     cell, and `read`/`write`/`edit` are scoped lexically by `resolve()` here
//!     (weaker — not symlink-aware).
//!
//! Protocol (one shot per invocation, driven by `monty_bridge.gleam`):
//!   args:   --workspace <abs dir>  (--code-str <program> | --code <file>)
//!   stdout: a single JSON line  {"ok": bool, "output": str, "error": str}
//!   exit:   always 0 — success/failure lives in the JSON `ok`, so the caller
//!           never has to disambiguate a nonzero exit from real program output.
//!
//! Code arrives inline via `--code-str` (bough drives this over execve, so
//! there is no shell and nothing to escape); `--code <file>` stays for manual
//! runs.

use std::{
    env, fs,
    io::{self, BufRead, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

use monty::{
    ExtFunctionResult, MontyException, MontyObject, MontyRun, NameLookupResult, NoLimitTracker, PrintWriter,
    RunProgress,
};

fn main() {
    let (workspace, code, profile, bash_inherit) = match parse_args() {
        Ok(parsed) => parsed,
        Err(e) => {
            emit(false, "", &e);
            return;
        }
    };

    match run(&code, &workspace, profile.as_deref(), bash_inherit) {
        Ok(output) => emit(true, &output, ""),
        Err((output, error)) => emit(false, &output, &error),
    }
}

/// Minimal flag parsing — `--workspace <dir>` plus the program inline via
/// `--code-str <program>` or from a file via `--code <file>`. `--nono-profile
/// <path>` scopes legacy-mode `bash` (capability groups + net leash);
/// `--bash-inherit` switches to sandboxed mode where the surrounding nono cell
/// already confines everything, so `bash` runs as a plain subprocess that
/// inherits it. We avoid a CLI crate to keep the binary tiny (monty's selling
/// point is startup speed).
fn parse_args() -> Result<(String, String, Option<String>, bool), String> {
    let mut workspace = String::new();
    let mut code: Option<String> = None;
    let mut profile: Option<String> = None;
    let mut bash_inherit = false;
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--workspace" => workspace = args.next().unwrap_or_default(),
            "--code-str" => code = Some(args.next().unwrap_or_default()),
            "--nono-profile" => profile = args.next(),
            "--bash-inherit" => bash_inherit = true,
            "--code" => {
                let file = args.next().unwrap_or_default();
                code = Some(fs::read_to_string(&file).map_err(|e| format!("cannot read {file}: {e}"))?);
            }
            _ => {}
        }
    }
    match code {
        Some(c) => Ok((workspace, c, profile, bash_inherit)),
        None => Err("no program given (--code-str or --code)".to_owned()),
    }
}

/// Run one Python program to completion, driving monty's suspend/resume loop:
/// every call to an undefined-but-called name (`bash`, `read`, ...) suspends as
/// a `FunctionCall`, which we service and resume. Returns the captured stdout,
/// or `(partial stdout, error message)`.
fn run(code: &str, workspace: &str, profile: Option<&str>, bash_inherit: bool) -> Result<String, (String, String)> {
    let runner = MontyRun::new(code.to_owned(), "agent.py", vec![])
        .map_err(|e| (String::new(), format!("python compile error:\n{}", format_exc(&e))))?;

    let mut output = String::new();

    let mut progress = {
        let print = PrintWriter::CollectString(&mut output);
        runner
            .start(vec![], NoLimitTracker, print)
            .map_err(|e| (String::new(), format_exc(&e)))?
    };

    loop {
        progress = match progress {
            RunProgress::Complete(_) => return Ok(output),
            RunProgress::FunctionCall(call) => {
                let result = dispatch(&call.function_name, &call.args, workspace, profile, bash_inherit);
                let print = PrintWriter::CollectString(&mut output);
                match call.resume(result, print) {
                    Ok(next) => next,
                    Err(e) => return Err((output.clone(), format_exc(&e))),
                }
            }
            // A bare reference to an unknown name (not a call): let Python raise
            // its own NameError so the model sees a normal traceback.
            RunProgress::NameLookup(lookup) => {
                let print = PrintWriter::CollectString(&mut output);
                match lookup.resume(NameLookupResult::Undefined, print) {
                    Ok(next) => next,
                    Err(e) => return Err((output.clone(), format_exc(&e))),
                }
            }
            // monty's own filesystem/OS builtins (open(), os.*) are deliberately
            // off — the agent must go through read()/write()/edit()/bash().
            RunProgress::OsCall(_) => {
                return Err((
                    output,
                    "direct filesystem/OS access is disabled — use read()/write()/edit()/bash()".to_owned(),
                ))
            }
            RunProgress::ResolveFutures(_) => {
                return Err((output, "async code is blocked on unresolved futures".to_owned()))
            }
        };
    }
}

/// Service one host-function call. Unknown names become a Python `NameError`
/// (via `NotFound`); everything else returns a string the program can use.
fn dispatch(name: &str, args: &[MontyObject], workspace: &str, profile: Option<&str>, bash_inherit: bool) -> ExtFunctionResult {
    let arg = |i: usize| -> String {
        match args.get(i) {
            Some(MontyObject::String(s)) => s.clone(),
            Some(other) => format!("{other:?}"),
            None => String::new(),
        }
    };
    let ret = |s: String| ExtFunctionResult::Return(MontyObject::String(s));

    match name {
        "bash" => ret(bash(&arg(0), workspace, profile, bash_inherit)),
        "read" => ret(read(&arg(0), workspace)),
        "write" => ret(write(&arg(0), &arg(1), workspace)),
        "edit" => ret(edit(&arg(0), &arg(1), &arg(2), workspace)),
        "mcp" => ret(mcp(&arg(0), &arg(1), &arg(2))),
        other => ExtFunctionResult::NotFound(other.to_owned()),
    }
}

// --- Host functions -------------------------------------------------------

/// `bash(cmd) -> str`: run a shell command and return its combined output.
///
/// Sandboxed mode (`inherit`): the whole sidecar already runs inside a nono
/// cell, so a plain child process inherits the kernel sandbox (workspace +
/// network + secret denies) automatically — no nested nono. Legacy mode: the
/// sidecar is unsandboxed, so `bash` opens its own nono cell here.
fn bash(cmd: &str, workspace: &str, profile: Option<&str>, inherit: bool) -> String {
    if inherit {
        return match Command::new("sh").arg("-c").arg(cmd).current_dir(workspace).output() {
            Ok(o) => {
                let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
                s.push_str(&String::from_utf8_lossy(&o.stderr));
                s
            }
            Err(e) => format!("error: could not run command: {e}"),
        };
    }
    let mut args: Vec<String> = vec![
        "run".into(),
        "-s".into(),
        "--allow".into(),
        workspace.into(),
        "--allow-cwd".into(),
        "--no-rollback".into(),
    ];
    // A session profile scopes the sandbox with the capability groups + network
    // leash the human granted (SPEC §7). Without it `bash` only ever gets the
    // workspace, so enabling a group would silently do nothing in code-mode.
    if let Some(path) = profile {
        args.push("--profile".into());
        args.push(path.into());
    }
    // Read-only PATH access to the language toolchains so a sandboxed command
    // can find cargo/go/node/etc. (mirrors nono_bridge.toolchain_reads — SPEC §6).
    for dir in toolchain_reads() {
        args.push("--read".into());
        args.push(dir);
    }
    args.push("--".into());
    args.extend(["sh".into(), "-c".into(), cmd.into()]);

    match Command::new("nono").args(&args).current_dir(workspace).output() {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            s
        }
        Err(e) => format!("error: could not launch nono: {e}"),
    }
}

/// Existing language-toolchain dirs under `$HOME` (cargo/go/pyenv/node/…) — read,
/// not allow: enough for PATH lookup, no write access. Mirrors the Gleam
/// `nono_bridge.toolchain_reads` so code-mode `bash` matches the old RUN path.
fn toolchain_reads() -> Vec<String> {
    const DIRS: [&str; 11] = [
        ".cargo/bin",
        "go/bin",
        ".pyenv/shims",
        ".pyenv/bin",
        ".rbenv/shims",
        ".rbenv/bin",
        ".ghcup/bin",
        ".nvm",
        ".local/share/fnm",
        ".local/share/pnpm",
        ".local/bin",
    ];
    let Ok(home) = env::var("HOME") else {
        return Vec::new();
    };
    DIRS.iter()
        .map(|d| format!("{home}/{d}"))
        .filter(|p| Path::new(p).is_dir())
        .collect()
}

/// `read(path) -> str`: read a workspace file (path scoped to the workspace).
fn read(path: &str, workspace: &str) -> String {
    match resolve(path, workspace) {
        Err(e) => e,
        Ok(p) => match fs::read_to_string(&p) {
            Ok(c) => c,
            Err(e) => format!("error: {e}"),
        },
    }
}

/// `write(path, content) -> str`: create or overwrite a workspace file.
fn write(path: &str, content: &str, workspace: &str) -> String {
    match resolve(path, workspace) {
        Err(e) => e,
        Ok(p) => {
            if let Some(dir) = p.parent() {
                let _ = fs::create_dir_all(dir);
            }
            match fs::write(&p, content) {
                Ok(_) => format!("wrote {}", p.display()),
                Err(e) => format!("error: {e}"),
            }
        }
    }
}

/// `edit(path, old, new) -> str`: replace the single exact occurrence of `old`.
/// Fails (without writing) if `old` is missing or not unique — same contract as
/// the harness's old EDIT step, so checks stay surgical.
fn edit(path: &str, old: &str, new: &str, workspace: &str) -> String {
    match resolve(path, workspace) {
        Err(e) => e,
        Ok(p) => {
            let contents = match fs::read_to_string(&p) {
                Ok(c) => c,
                Err(e) => return format!("error: {e}"),
            };
            match contents.matches(old).count() {
                0 => format!("error: 'old' text not found in {path}"),
                1 => match fs::write(&p, contents.replacen(old, new, 1)) {
                    Ok(_) => format!("edited {}", p.display()),
                    Err(e) => format!("error: {e}"),
                },
                n => format!("error: 'old' text is not unique in {path} ({n} matches)"),
            }
        }
    }
}

/// `mcp(server_cmd, tool, args_json) -> str`: spawn an MCP stdio server via
/// `sh -c server_cmd`, speak JSON-RPC 2.0 (initialize then tools/call), and
/// return the tool result's text. On any failure return a string starting with
/// `error:`.
fn mcp(server_cmd: &str, tool: &str, args_json: &str) -> String {
    let mut child = match Command::new("sh")
        .arg("-c")
        .arg(server_cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return format!("error: failed to spawn MCP server: {e}"),
    };

    let mut stdin = child.stdin.take().expect("child stdin was piped");
    let stdout = child.stdout.take().expect("child stdout was piped");
    let mut reader = io::BufReader::new(stdout);

    let arguments: serde_json::Value = if args_json.trim().is_empty() {
        serde_json::json!({})
    } else {
        match serde_json::from_str(args_json) {
            Ok(v) => v,
            Err(e) => {
                let _ = child.kill();
                return format!("error: args_json is not valid JSON: {e}");
            }
        }
    };

    let send = |stdin: &mut std::process::ChildStdin,
                msg: &serde_json::Value|
     -> Result<(), String> {
        write!(stdin, "{msg}\n").map_err(|e| format!("error: write to MCP server: {e}"))?;
        stdin
            .flush()
            .map_err(|e| format!("error: flush MCP stdin: {e}"))
    };

    // 1. initialize
    let init = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "bough-monty", "version": "0.1.0"}
        }
    });
    if let Err(e) = send(&mut stdin, &init) {
        let _ = child.kill();
        return e;
    }
    let init_resp = match mcp_read_response(&mut reader, 1) {
        Ok(v) => v,
        Err(e) => {
            let _ = child.kill();
            return format!("error: {e}");
        }
    };
    if let Some(err) = init_resp.get("error") {
        let _ = child.kill();
        return format!("error: MCP initialize failed: {err}");
    }

    // 2. tools/call
    let call = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": tool,
            "arguments": arguments,
        }
    });
    if let Err(e) = send(&mut stdin, &call) {
        let _ = child.kill();
        return e;
    }
    let call_resp = match mcp_read_response(&mut reader, 2) {
        Ok(v) => v,
        Err(e) => {
            let _ = child.kill();
            return format!("error: {e}");
        }
    };
    let _ = child.kill();

    if let Some(err) = call_resp.get("error") {
        return format!("error: MCP tools/call failed: {err}");
    }
    let result = match call_resp.get("result") {
        Some(r) => r,
        None => return format!("error: MCP response has no result field"),
    };
    let texts: Vec<String> = result
        .get("content")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                        item.get("text").and_then(|t| t.as_str()).map(String::from)
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    if texts.is_empty() {
        format!("error: no text content in MCP result: {result}")
    } else {
        texts.join("\n")
    }
}

/// Read newline-delimited JSON-RPC messages from an MCP server's stdout until
/// one bears the matching `id`, skipping notifications.
fn mcp_read_response(
    reader: &mut io::BufReader<std::process::ChildStdout>,
    expected_id: i64,
) -> Result<serde_json::Value, String> {
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader
            .read_line(&mut line)
            .map_err(|e| format!("reading MCP stdout: {e}"))?;
        if n == 0 {
            return Err("MCP server closed stdout".to_string());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("id").and_then(|i| i.as_i64()) == Some(expected_id) {
            return Ok(v);
        }
    }
}

// --- Path scoping ---------------------------------------------------------

/// Resolve a (relative or absolute) path against the workspace and reject
/// anything that escapes it. Since read/write/edit run as trusted host code
/// outside nono, this lexical check is what keeps monty's confinement honest.
fn resolve(path: &str, workspace: &str) -> Result<PathBuf, String> {
    let raw = Path::new(path);
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        Path::new(workspace).join(raw)
    };
    let target = lexical_normalize(&joined);
    let root = lexical_normalize(Path::new(workspace));
    if target == root || target.starts_with(&root) {
        Ok(target)
    } else {
        Err(format!("error: path '{path}' is outside the workspace"))
    }
}

/// Resolve `.`/`..` lexically (without touching the filesystem, so it works for
/// not-yet-created files). Not symlink-aware — adequate for confining the
/// trusted host IO functions to the workspace subtree.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

// --- Errors ---------------------------------------------------------------

/// Render a monty exception as a compact, Python-ish traceback the agent can
/// debug from — `ExcType: message` plus each frame's `file:line:col  source`.
fn format_exc(e: &MontyException) -> String {
    let mut s = format!("{:?}", e.exc_type());
    if let Some(msg) = e.message() {
        s.push_str(": ");
        s.push_str(msg);
    }
    for frame in e.traceback() {
        s.push_str(&format!("\n  at {}:{}:{}", frame.filename, frame.start.line, frame.start.column));
        if let Some(line) = &frame.preview_line {
            s.push_str("   ");
            s.push_str(line.trim());
        }
    }
    s
}

// --- Output ---------------------------------------------------------------

/// Emit the single result line that `monty_bridge.gleam` parses.
fn emit(ok: bool, output: &str, error: &str) {
    let value = serde_json::json!({ "ok": ok, "output": output, "error": error });
    println!("{value}");
}
