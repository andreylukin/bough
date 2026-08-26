//! Invariant: this crate is the ONLY place in the tree that spawns `gh` (§13: no octocrab), and it
//! never passes `--jq`. Parsing happens in Rust, so the recording shim in the tests sees ONE stable
//! argv per call and an unplanned `gh` call is a red test rather than a network request.
//!
//! The second invariant is the classification: **uncertain is human.** [`Actor`] has no third state
//! at the decision point on purpose; the uncertainty survives in [`classify_reason`], which is what
//! a refusal quotes.
//!
//! NO ROW: a library both `collector-github` and `actions-github` depend on.

use std::path::PathBuf;
use std::time::Duration;

/// A configured `gh` invoker.
#[derive(Clone)]
#[allow(dead_code)] // scaffold: filled by the work package that owns this crate
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

    /// `gh api <path> [-f k=v]…` → parsed JSON. NEVER `--jq`. WP-2.
    pub async fn api(
        &self,
        path: &str,
        fields: &[(&str, &str)],
    ) -> Result<serde_json::Value, GhError> {
        let _ = (path, fields);
        todo!("WP-2")
    }

    /// `gh pr list --repo R --json … --limit N`. WP-2.
    pub async fn pr_list(
        &self,
        repo: &str,
        fields: &[&str],
        limit: usize,
    ) -> Result<Vec<serde_json::Value>, GhError> {
        let _ = (repo, fields, limit);
        todo!("WP-2")
    }

    /// `gh pr create` / `gh pr comment` / `gh api -X PATCH …`. EVERY write goes through here, so
    /// the shim's argv log is a complete record of what this build did to the world. WP-2.
    pub async fn run(&self, args: &[&str], stdin: Option<&str>) -> Result<GhOutput, GhError> {
        let _ = (args, stdin);
        todo!("WP-2")
    }

    /// `gh api user` → the authenticated login. Cached per row activation by the caller. WP-2.
    pub async fn whoami(&self) -> Result<String, GhError> {
        todo!("WP-2")
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
/// with a login not in the allowlist is [`Actor::Human`]. WP-2.
pub fn classify(account_type: &str, login: &str, allowlist: &[String]) -> Actor {
    let _ = (account_type, login, allowlist);
    todo!("WP-2")
}

/// PURE: the reason string a refusal carries, so "uncertain" is visible in the error even though
/// the verdict is [`Actor::Human`]. WP-2.
pub fn classify_reason(account_type: &str, login: &str, allowlist: &[String]) -> &'static str {
    let _ = (account_type, login, allowlist);
    todo!("WP-2")
}
