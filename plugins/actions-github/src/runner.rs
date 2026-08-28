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
