//! Invariant: this crate is the ONLY place in the tree that spawns `gh` (§13: no octocrab), and it
//! never passes `--jq`. Parsing happens in Rust, so the recording shim in the tests sees ONE stable
//! argv per call and an unplanned `gh` call is a red test rather than a network request.
//!
//! The second invariant is the classification: **uncertain is human.** [`Actor`] has no third state
//! at the decision point on purpose; the uncertainty survives in [`classify_reason`], which is what
//! a refusal quotes.
//!
//! NO ROW: a library both `collector-github` and `actions-github` depend on.
//!
//! No runtime invariant: no row, and the two claims above are structural (one spawn site, a total
//! classification function), so they are unit tests rather than a check over a stream. The stream
//! consequences belong to `collector-github` and `actions-github`, which own them.

use std::path::PathBuf;
use std::time::Duration;

/// A configured `gh` invoker.
#[derive(Clone)]
pub struct Gh {
    bin: PathBuf,
    timeout: Duration,
    env: Vec<(String, String)>,
}

/// One `gh` process's result.
#[derive(Clone, Debug, PartialEq)]
pub struct GhOutput {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

impl Gh {
    /// A `gh` bound to a binary name/path and a per-call timeout.
    pub fn new(bin: impl Into<PathBuf>, timeout: Duration) -> Gh {
        Gh {
            bin: bin.into(),
            timeout,
            env: Vec::new(),
        }
    }

    /// Extra environment for every call (the tests' shim uses it).
    pub fn with_env(mut self, env: Vec<(String, String)>) -> Gh {
        self.env = env;
        self
    }

    /// The binary this will spawn. Named so a test can assert the shim is what runs.
    pub fn bin(&self) -> &PathBuf {
        &self.bin
    }

    /// `gh api <path> [-f k=v]…` → parsed JSON. NEVER `--jq`: parsing happens in Rust, so the
    /// shim sees ONE stable argv per call.
    pub async fn api(
        &self,
        path: &str,
        fields: &[(&str, &str)],
    ) -> Result<serde_json::Value, GhError> {
        let mut args: Vec<String> = vec!["api".to_string(), path.to_string()];
        for (k, v) in fields {
            args.push("-f".to_string());
            args.push(format!("{k}={v}"));
        }
        let argv: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let out = self.run(&argv, None).await?;
        serde_json::from_str(&out.stdout).map_err(|e| GhError::BadJson {
            args: args.join(" "),
            detail: e.to_string(),
        })
    }

    /// `gh pr list --repo R --json … --limit N`.
    pub async fn pr_list(
        &self,
        repo: &str,
        fields: &[&str],
        limit: usize,
    ) -> Result<Vec<serde_json::Value>, GhError> {
        let limit = limit.to_string();
        let joined = fields.join(",");
        let args = vec![
            "pr",
            "list",
            "--repo",
            repo,
            "--json",
            joined.as_str(),
            "--limit",
            limit.as_str(),
        ];
        let out = self.run(&args, None).await?;
        let value: serde_json::Value =
            serde_json::from_str(&out.stdout).map_err(|e| GhError::BadJson {
                args: args.join(" "),
                detail: e.to_string(),
            })?;
        match value {
            serde_json::Value::Array(rows) => Ok(rows),
            other => Err(GhError::BadJson {
                args: args.join(" "),
                detail: format!("expected a JSON array, got {}", kind_of(&other)),
            }),
        }
    }

    /// `gh pr create` / `gh pr comment` / `gh api -X PATCH …`. EVERY write goes through here, so
    /// the shim's argv log is a complete record of what this build did to the world.
    pub async fn run(&self, args: &[&str], stdin: Option<&str>) -> Result<GhOutput, GhError> {
        let joined = args.join(" ");
        let mut cmd = tokio::process::Command::new(&self.bin);
        cmd.args(args)
            .stdin(if stdin.is_some() {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            })
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().map_err(|e| GhError::Spawn {
            bin: self.bin.display().to_string(),
            detail: e.to_string(),
        })?;
        if let Some(text) = stdin {
            use tokio::io::AsyncWriteExt;
            let mut pipe = child.stdin.take().expect("stdin was piped");
            pipe.write_all(text.as_bytes())
                .await
                .map_err(|e| GhError::Spawn {
                    bin: self.bin.display().to_string(),
                    detail: format!("writing stdin: {e}"),
                })?;
            drop(pipe);
        }
        let finished = tokio::time::timeout(self.timeout, child.wait_with_output()).await;
        let out = match finished {
            Err(_) => {
                return Err(GhError::Timeout {
                    args: joined,
                    ms: self.timeout.as_millis() as u64,
                })
            }
            Ok(Err(e)) => {
                return Err(GhError::Spawn {
                    bin: self.bin.display().to_string(),
                    detail: e.to_string(),
                })
            }
            Ok(Ok(out)) => out,
        };
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let code = out.status.code().unwrap_or(-1);
        if code != 0 {
            return Err(GhError::Exit {
                args: joined,
                code,
                stderr,
            });
        }
        Ok(GhOutput {
            stdout,
            stderr,
            code,
        })
    }

    /// `gh api user` → the authenticated login. Cached per row activation by the caller.
    pub async fn whoami(&self) -> Result<String, GhError> {
        let value = self.api("user", &[]).await?;
        value
            .get("login")
            .and_then(|l| l.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| GhError::BadJson {
                args: "api user".to_string(),
                detail: "no `login` field".to_string(),
            })
    }
}

/// PURE: what a JSON value IS, for an error message that does not quote a payload.
fn kind_of(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a bool",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}

/// The recording shim's contract, in ONE place: an argv becomes exactly one fixture file name.
/// The shim script (`scripts/fixtures/gh/gh`) computes the same string in bash; a test writes its
/// fixtures through this function so the two cannot drift.
pub mod shim {
    /// PURE: `["pr","list","--repo","o/r"] → "pr_list_--repo_o_r"`. Every character outside
    /// `[A-Za-z0-9._-]` becomes `_`.
    pub fn fixture_name(args: &[&str]) -> String {
        args.join(" ")
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }
}

/// What a `gh` call goes wrong as.
#[derive(Debug, thiserror::Error)]
pub enum GhError {
    #[error("`{bin}` could not be spawned: {detail}")]
    Spawn { bin: String, detail: String },
    #[error("`gh {args}` exited {code}: {stderr}")]
    Exit {
        args: String,
        code: i32,
        stderr: String,
    },
    #[error("`gh {args}` timed out after {ms}ms")]
    Timeout { args: String, ms: u64 },
    #[error("`gh {args}` produced unparseable JSON: {detail}")]
    BadJson { args: String, detail: String },
}

/// §7's bot-thread classification. UNCERTAIN IS HUMAN — there is no third state at the decision
/// point on purpose.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Actor {
    Bot,
    Human,
}

/// PURE. `account_type` is GitHub's `User`/`Bot`/`Organization`/`""`; an empty or unknown value
/// with a login not in the allowlist is [`Actor::Human`].
pub fn classify(account_type: &str, login: &str, allowlist: &[String]) -> Actor {
    if account_type.eq_ignore_ascii_case("Bot") {
        return Actor::Bot;
    }
    if allowlist.iter().any(|a| a == login) {
        return Actor::Bot;
    }
    // UNCERTAIN IS HUMAN: an empty or unrecognised account type is not evidence of a bot.
    Actor::Human
}

/// PURE: the reason string a refusal carries, so "uncertain" is visible in the error even though
/// the verdict is [`Actor::Human`].
pub fn classify_reason(account_type: &str, login: &str, allowlist: &[String]) -> &'static str {
    if account_type.eq_ignore_ascii_case("Bot") {
        return "GitHub account type is Bot";
    }
    if allowlist.iter().any(|a| a == login) {
        return "login is in the known-bot allowlist";
    }
    if account_type.trim().is_empty() {
        return "no GitHub account type: uncertain, so treated as human";
    }
    if account_type.eq_ignore_ascii_case("User") {
        return "GitHub account type is User";
    }
    "unrecognised GitHub account type: uncertain, so treated as human"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bots() -> Vec<String> {
        vec![
            "dependabot[bot]".to_string(),
            "github-actions[bot]".to_string(),
        ]
    }

    #[test]
    fn a_bot_account_type_is_a_bot() {
        assert_eq!(classify("Bot", "someone", &[]), Actor::Bot);
        assert_eq!(
            classify_reason("Bot", "someone", &[]),
            "GitHub account type is Bot"
        );
    }

    #[test]
    fn an_allowlisted_login_is_a_bot_even_with_a_user_type() {
        assert_eq!(classify("User", "dependabot[bot]", &bots()), Actor::Bot);
        assert_eq!(
            classify_reason("User", "dependabot[bot]", &bots()),
            "login is in the known-bot allowlist"
        );
    }

    #[test]
    fn an_empty_account_type_is_human_and_says_it_is_uncertain() {
        assert_eq!(classify("", "someone", &bots()), Actor::Human);
        assert!(classify_reason("", "someone", &bots()).contains("uncertain"));
    }

    #[test]
    fn an_unknown_login_is_human() {
        assert_eq!(classify("User", "a-teammate", &bots()), Actor::Human);
        assert_eq!(
            classify_reason("User", "a-teammate", &bots()),
            "GitHub account type is User"
        );
    }

    #[test]
    fn an_unrecognised_account_type_is_human_and_says_it_is_uncertain() {
        assert_eq!(classify("Organization", "an-org", &bots()), Actor::Human);
        assert!(classify_reason("Organization", "an-org", &bots()).contains("uncertain"));
    }

    #[test]
    fn one_argv_is_one_fixture_name() {
        assert_eq!(
            shim::fixture_name(&["pr", "list", "--repo", "o/r", "--limit", "50"]),
            "pr_list_--repo_o_r_--limit_50"
        );
    }
}
