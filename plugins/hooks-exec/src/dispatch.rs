//! Invariant: a hook is ONE PROCESS, ONE JSON LINE IN, ONE JSON OBJECT OUT, bounded on both axes.
//! A non-zero exit, a timeout, unparseable stdout and stdout over `max_output_bytes` are ALL ONE
//! THING — a [`HookFailure`] — so no failure mode gets its own quiet retry path.

use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;

use crate::{HookInput, HookOutput, HookPoint};

/// Why one invocation did not produce a usable [`HookOutput`]. Every variant is COUNTED the same.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum HookFailure {
    #[error("`{exec}` could not be started: {detail}")]
    Spawn { exec: String, detail: String },
    #[error("`{exec}` exited {code}: {stderr}")]
    Exit {
        exec: String,
        code: String,
        stderr: String,
    },
    #[error("`{exec}` did not finish within {ms}ms")]
    Timeout { exec: String, ms: u64 },
    #[error("`{exec}` wrote more than max_output_bytes ({max})")]
    TooMuchOutput { exec: String, max: usize },
    #[error("`{exec}` wrote stdout that is not one JSON object: {detail}")]
    Unparseable { exec: String, detail: String },
}

/// Run one hook executable to completion.
///
/// `input` goes in as ONE line of JSON; stdout must be ONE JSON object of at most
/// `max_output_bytes`. The child is killed on drop, so a timeout leaves no orphan.
pub async fn run_hook(
    point: &HookPoint,
    input: &HookInput,
    max_output_bytes: usize,
) -> Result<HookOutput, HookFailure> {
    let exec = point.exec.display().to_string();
    let mut cmd = tokio::process::Command::new(&point.exec);
    cmd.args(&point.args)
        .envs(&point.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| HookFailure::Spawn {
        exec: exec.clone(),
        detail: e.to_string(),
    })?;

    let line = serde_json::to_string(input).map_err(|e| HookFailure::Spawn {
        exec: exec.clone(),
        detail: format!("the input would not serialize: {e}"),
    })?;
    if let Some(mut stdin) = child.stdin.take() {
        // A hook that never reads stdin is not a failure: the write is best-effort and the
        // timeout, not a broken pipe, is what bounds it.
        let _ = stdin.write_all(line.as_bytes()).await;
        let _ = stdin.write_all(b"\n").await;
        let _ = stdin.shutdown().await;
    }

    let out = tokio::time::timeout(
        Duration::from_millis(point.timeout_ms),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| HookFailure::Timeout {
        exec: exec.clone(),
        ms: point.timeout_ms,
    })?
    .map_err(|e| HookFailure::Spawn {
        exec: exec.clone(),
        detail: e.to_string(),
    })?;

    if !out.status.success() {
        return Err(HookFailure::Exit {
            exec,
            code: out
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "by signal".to_string()),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    if out.stdout.len() > max_output_bytes {
        return Err(HookFailure::TooMuchOutput {
            exec,
            max: max_output_bytes,
        });
    }
    parse_output(&exec, &out.stdout)
}

/// PURE: stdout → [`HookOutput`]. Empty stdout is the EMPTY output — a hook that observed and
/// asked for nothing is not a failure.
pub fn parse_output(exec: &str, stdout: &[u8]) -> Result<HookOutput, HookFailure> {
    let text = std::str::from_utf8(stdout).map_err(|e| HookFailure::Unparseable {
        exec: exec.to_string(),
        detail: e.to_string(),
    })?;
    if text.trim().is_empty() {
        return Ok(HookOutput::default());
    }
    serde_json::from_str(text.trim()).map_err(|e| HookFailure::Unparseable {
        exec: exec.to_string(),
        detail: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_runtime_actions::RuntimeAction;

    #[test]
    fn empty_stdout_is_the_empty_output_not_a_failure() {
        assert_eq!(parse_output("h", b"").expect("ok"), HookOutput::default());
        assert_eq!(
            parse_output("h", b"  \n").expect("ok"),
            HookOutput::default()
        );
    }

    #[test]
    fn a_returned_action_parses_off_the_wire() {
        let out = parse_output(
            "h",
            br#"{"actions":[{"kind":"hint","agent":"sol","text":"look here"}],"note":"n"}"#,
        )
        .expect("parses");
        assert_eq!(
            out.actions,
            vec![RuntimeAction::Hint {
                agent: "sol".into(),
                text: "look here".into()
            }]
        );
        assert_eq!(out.note.as_deref(), Some("n"));
    }

    #[test]
    fn unparseable_stdout_names_the_executable() {
        let err = parse_output("h", b"not json").expect_err("refused");
        assert!(matches!(err, HookFailure::Unparseable { .. }), "{err}");
        assert!(err.to_string().contains("`h`"), "{err}");
    }
}
