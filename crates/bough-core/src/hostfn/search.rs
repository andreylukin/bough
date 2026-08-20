//! `search()` — the web lookup, as a global rather than a command to remember.
//!
//! bough could already search: `parallel-cli` is a program and `bash()` runs
//! programs. It just did not. Given the tool installed, authenticated and
//! described in the system prompt, the model reached for it in 3 of 12 trials
//! where a looked-up convention was exactly what it got wrong. A capability
//! the model has to remember to shell out to is one it mostly does not use,
//! so this puts it where it already looks: next to `bash`, `view` and
//! `patch`, pre-injected and callable with no ceremony.
//!
//! Thin on purpose. It shells out to `parallel-cli --json`, so the CLI stays
//! the one place that knows the API, and `search()` is the affordance.

use std::process::Stdio;
use std::sync::Arc;

use serde_json::Value;
use tokio::process::Command;

use crate::errors::BoughError;
use crate::types::{HostFn, TurnCtx};

/// How many results a bare `search(objective)` returns. Enough to triangulate
/// a fact, few enough that the program's output stays readable.
const DEFAULT_MAX_RESULTS: usize = 5;

fn search_error(message: impl Into<String>) -> BoughError {
    BoughError::bad_request(message.into())
}

pub fn create_search_host_fn(ctx: &TurnCtx) -> HostFn {
    let workspace = ctx.workspace.clone();
    Arc::new(move |args: Vec<String>| {
        let workspace = workspace.clone();
        let objective = args.first().cloned().unwrap_or_default();
        let options = args.get(1).cloned().unwrap_or_default();
        Box::pin(async move {
            if objective.trim().is_empty() {
                return Err(search_error(
                    "search(objective) needs something to look for: \
                     search(\"default salt concentration for primer3 oligotm\").",
                ));
            }
            let opts: Value = if options.is_empty() {
                Value::Null
            } else {
                serde_json::from_str(&options)
                    .map_err(|_| search_error("search: the second argument was not valid JSON"))?
            };
            let max_results = opts
                .get("maxResults")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_MAX_RESULTS as u64);
            let mode = opts
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or("fast")
                .to_string();

            let mut command = Command::new("parallel-cli");
            command
                .arg("search")
                .arg(&objective)
                .arg("--mode")
                .arg(&mode)
                .arg("--max-results")
                .arg(max_results.to_string())
                .arg("--json")
                .current_dir(&workspace)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if let Some(domains) = opts.get("excludeDomains").and_then(Value::as_array) {
                for domain in domains.iter().filter_map(Value::as_str) {
                    command.arg("--exclude-domains").arg(domain);
                }
            }

            let output = command.output().await.map_err(|err| {
                if err.kind() == std::io::ErrorKind::NotFound {
                    // The one failure worth teaching: the tool is absent, and
                    // the fix is a command the user runs, not a retry.
                    search_error(
                        "search() needs parallel-cli, which is not installed. \
                         Install it with `curl -fsSL https://parallel.ai/install.sh | bash` \
                         and authenticate with `parallel-cli login` or PARALLEL_API_KEY.",
                    )
                } else {
                    search_error(format!("search: could not run parallel-cli: {err}"))
                }
            })?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let detail = stderr.trim();
                return Err(search_error(format!(
                    "search failed: {}",
                    if detail.is_empty() {
                        "parallel-cli exited non-zero with no message"
                    } else {
                        detail
                    }
                )));
            }
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        })
    })
}

/// Whether `parallel-cli` is on PATH.
///
/// The gate for registering `search()` at all: a host function that always
/// fails is worse than one the model was never told about, because the prompt
/// section that describes it is written as a promise.
pub fn parallel_cli_available() -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join("parallel-cli").is_file())
}
