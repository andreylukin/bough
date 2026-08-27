//! Invariant: the surface is documented ONCE. One projection section, assembled from the files
//! beside this one, rendered from the LIVE registry — so the function list the model reads and
//! the globals the sandbox injects cannot drift apart.

use bough_plugin_projection::{ProjectionError, SectionBody, SectionRender, SectionRequest};

/// The section id.
pub const SECTION_ID: &str = "codemode.surface";

/// The patch grammar, restored verbatim from
/// `git show main:crates/bough-core/src/prompt/sections/patch-grammar.md`.
pub const PATCH_GRAMMAR: &str = include_str!("patch-grammar.md");
/// The file verbs, from main's `files.md`, retargeted at QuickJS.
pub const FILES: &str = include_str!("files.md");
/// The shell verbs, from main's `shell.md`, retargeted at `bash`/`sh`/`bg`.
pub const SHELL: &str = include_str!("shell.md");
/// `console.log` is the only thing that comes back.
pub const PRINTING: &str = include_str!("printing.md");
/// How a turn ends: in text, calling nothing. There is no stop tool.
pub const ENDING: &str = include_str!("ending.md");
/// Drilling from a tier down to raw steps.
pub const LEDGER: &str = include_str!("ledger.md");
/// Claims, acts, workers, asks, forks, scheduled intents.
pub const WORK: &str = include_str!("work.md");

/// The `codemode.surface` renderer. Contributes NOTHING when the agent has no `run` in scope, so
/// mounting the row without concealment does not double-document.
pub struct Surface {
    /// WP-5 fills this with what it needs to read the live registry.
    pub _private: (),
}

#[async_trait::async_trait]
impl SectionRender for Surface {
    /// WP-5 owns the body.
    async fn render(&self, _req: &SectionRequest) -> Result<Option<SectionBody>, ProjectionError> {
        todo!("WP-5: assemble the generated function table + the restored main sections")
    }
}

/// The generated half: one row per injected global, built from the same snapshot the sandbox
/// injects for `agent`.
///
/// WP-5 owns the body.
pub fn function_table(_bindings: &[crate::bind::Binding]) -> String {
    todo!("WP-5: render the surface table from the live bindings")
}
