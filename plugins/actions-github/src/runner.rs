//! Invariant: every GitHub call this crate makes goes through ONE object, so a test can hold the
//! complete argv log of what the build did to the world and assert that a read path wrote nothing.
//!
//! DEVIATION (WP-3): the plan has the Provider call `bough_plugin_gh_cli::Gh` directly. `Gh` is
//! WP-2's and is `todo!()` while the packages are built in parallel, so the Provider depends on
//! this narrow TRAIT instead and [`GhCli`] is the one production implementation, delegating to
//! `Gh` unchanged. Injecting the transport is also what AGENTS.md asks for; the merge note records
//! it.

use bough_plugin_gh_cli::{Gh, GhError, GhOutput};

/// What the Provider needs of `gh`.
#[async_trait::async_trait]
pub trait GhRunner: Send + Sync + 'static {
    /// A READ that parses JSON (`gh api …`, `gh pr view --json …`). Never `--jq`.
    async fn json(&self, args: &[&str]) -> Result<serde_json::Value, GhError>;
    /// A WRITE. Every mutation of the world is one of these, and the shim's log is complete.
    async fn run(&self, args: &[&str], stdin: Option<&str>) -> Result<GhOutput, GhError>;
    /// The authenticated login.
    async fn whoami(&self) -> Result<String, GhError>;
}

/// The production runner: `gh-cli`'s `Gh`, unchanged.
pub struct GhCli(pub Gh);

#[async_trait::async_trait]
impl GhRunner for GhCli {
    async fn json(&self, args: &[&str]) -> Result<serde_json::Value, GhError> {
        let out = self.0.run(args, None).await?;
        serde_json::from_str(&out.stdout).map_err(|e| GhError::BadJson {
            args: args.join(" "),
            detail: e.to_string(),
        })
    }
    async fn run(&self, args: &[&str], stdin: Option<&str>) -> Result<GhOutput, GhError> {
        self.0.run(args, stdin).await
    }
    async fn whoami(&self) -> Result<String, GhError> {
        self.0.whoami().await
    }
}

/// What the Provider needs of `git` — the ONE act `gh` cannot do: uploading local objects. A
/// ref moved through `gh api` can only name commits GitHub already has, which is why the push
/// used to reject every local-only commit (the primary case). §13's "no second transport" is
/// about HTTP clients; the git protocol has no `gh` spelling at all.
#[async_trait::async_trait]
pub trait GitRunner: Send + Sync + 'static {
    /// `git -C <dir> <args…>`. `Ok(stdout)`; `Err` carries git's own stderr, verbatim.
    async fn git(&self, dir: &std::path::Path, args: &[&str]) -> Result<String, String>;
}

/// The production runner: the configured `git` binary, bounded by the row's timeout.
pub struct GitCli {
    pub bin: String,
    pub timeout: std::time::Duration,
}

#[async_trait::async_trait]
impl GitRunner for GitCli {
    async fn git(&self, dir: &std::path::Path, args: &[&str]) -> Result<String, String> {
        let mut cmd = tokio::process::Command::new(&self.bin);
        cmd.arg("-C")
            .arg(dir)
            .args(args)
            .stdin(std::process::Stdio::null());
        let out = tokio::time::timeout(self.timeout, cmd.output())
            .await
            .map_err(|_| format!("git {} timed out after {:?}", args.join(" "), self.timeout))?
            .map_err(|e| format!("could not run `{}`: {e}", self.bin))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).to_string())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }
}
