//! Invariant: one hook is ONE bounded `sh -c` — JSON on stdin, capped stdout/stderr back, killed
//! at its deadline. A hook that overruns is reported as `timed_out`, never awaited forever.

use std::path::Path;
use std::process::Stdio;

/// What one hook execution came back with.
#[derive(Clone, Debug, PartialEq)]
pub struct HookRun {
    /// The exit code, when the process exited normally.
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// Run one hook command in `cwd` with `payload` on stdin. Spawn failures come back as a run with
/// no status and the error in `stderr`, so the caller has one shape to interpret.
pub async fn run_hook(
    command: &str,
    cwd: &Path,
    payload: &serde_json::Value,
    timeout_ms: u64,
    max_output_bytes: usize,
) -> HookRun {
    let failed = |detail: String| HookRun {
        status: None,
        stdout: String::new(),
        stderr: detail,
        timed_out: false,
    };
    let mut child = match tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return failed(format!("hook did not spawn: {e}")),
    };
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let _ = stdin.write_all(payload.to_string().as_bytes()).await;
        let _ = stdin.write_all(b"\n").await;
        // Dropped here: the hook sees EOF, the way both CLIs feed their hooks.
    }
    let cap = |mut s: String| {
        if s.len() > max_output_bytes {
            let mut cut = max_output_bytes;
            while cut > 0 && !s.is_char_boundary(cut) {
                cut -= 1;
            }
            s.truncate(cut);
        }
        s
    };
    match tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms),
        child.wait_with_output(),
    )
    .await
    {
        Ok(Ok(out)) => HookRun {
            status: out.status.code(),
            stdout: cap(String::from_utf8_lossy(&out.stdout).to_string()),
            stderr: cap(String::from_utf8_lossy(&out.stderr).to_string()),
            timed_out: false,
        },
        Ok(Err(e)) => failed(format!("hook did not finish: {e}")),
        // `wait_with_output` consumed the child; `kill_on_drop` reaps it on this drop.
        Err(_) => HookRun {
            status: None,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: true,
        },
    }
}
