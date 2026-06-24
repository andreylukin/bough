//! bough-monty — the code-mode sidecar (SPEC.md §5.2).
//!
//! bough's supervisor no longer emits a typed batch of file/shell actions; it
//! writes one Python program per round, and this binary runs it inside a
//! [monty](https://github.com/pydantic/monty) sandbox. monty is a Rust Python
//! interpreter that can touch *nothing* on the host except the host functions
//! we hand it — so the agent's only doors out are the four functions below.
//!
//! Two nested sandboxes meet here (SPEC.md §6): monty confines the
//! agent-authored Python (no imports of the OS, no sockets, resource-limited);
//! `bash` — the one door that runs real processes — opens into a `nono` cell
//! (kernel-enforced workspace + network allowlist). `read`/`write`/`edit` are
//! trusted host code, so this binary itself scopes their paths to the
//! workspace.
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
    path::{Component, Path, PathBuf},
    process::Command,
};

use monty::{
    ExtFunctionResult, MontyException, MontyObject, MontyRun, NameLookupResult, NoLimitTracker, PrintWriter,
    RunProgress,
};

fn main() {
    let (workspace, code) = match parse_args() {
        Ok(parsed) => parsed,
        Err(e) => {
            emit(false, "", &e);
            return;
        }
    };

    match run(&code, &workspace) {
        Ok(output) => emit(true, &output, ""),
        Err((output, error)) => emit(false, &output, &error),
    }
}

/// Minimal flag parsing — `--workspace <dir>` plus the program inline via
/// `--code-str <program>` or from a file via `--code <file>`. We avoid a CLI
/// crate to keep the binary tiny (monty's selling point is startup speed).
fn parse_args() -> Result<(String, String), String> {
    let mut workspace = String::new();
    let mut code: Option<String> = None;
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--workspace" => workspace = args.next().unwrap_or_default(),
            "--code-str" => code = Some(args.next().unwrap_or_default()),
            "--code" => {
                let file = args.next().unwrap_or_default();
                code = Some(fs::read_to_string(&file).map_err(|e| format!("cannot read {file}: {e}"))?);
            }
            _ => {}
        }
    }
    match code {
        Some(c) => Ok((workspace, c)),
        None => Err("no program given (--code-str or --code)".to_owned()),
    }
}

/// Run one Python program to completion, driving monty's suspend/resume loop:
/// every call to an undefined-but-called name (`bash`, `read`, ...) suspends as
/// a `FunctionCall`, which we service and resume. Returns the captured stdout,
/// or `(partial stdout, error message)`.
fn run(code: &str, workspace: &str) -> Result<String, (String, String)> {
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
                let result = dispatch(&call.function_name, &call.args, workspace);
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
fn dispatch(name: &str, args: &[MontyObject], workspace: &str) -> ExtFunctionResult {
    let arg = |i: usize| -> String {
        match args.get(i) {
            Some(MontyObject::String(s)) => s.clone(),
            Some(other) => format!("{other:?}"),
            None => String::new(),
        }
    };
    let ret = |s: String| ExtFunctionResult::Return(MontyObject::String(s));

    match name {
        "bash" => ret(bash(&arg(0), workspace)),
        "read" => ret(read(&arg(0), workspace)),
        "write" => ret(write(&arg(0), &arg(1), workspace)),
        "edit" => ret(edit(&arg(0), &arg(1), &arg(2), workspace)),
        other => ExtFunctionResult::NotFound(other.to_owned()),
    }
}

// --- Host functions -------------------------------------------------------

/// `bash(cmd) -> str`: run a shell command inside a nono sandbox (workspace
/// read/write, network default-deny) and return its combined output. This is
/// the one host function that runs native processes, so it is the seam where
/// monty's language sandbox hands off to nono's kernel sandbox.
fn bash(cmd: &str, workspace: &str) -> String {
    let mut args: Vec<String> = vec![
        "run".into(),
        "-s".into(),
        "--allow".into(),
        workspace.into(),
        "--allow-cwd".into(),
        "--no-rollback".into(),
    ];
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
