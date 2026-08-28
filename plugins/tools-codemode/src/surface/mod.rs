//! Invariant: the surface is documented ONCE. One projection section, assembled from the files
//! beside this one, rendered from the LIVE registry — so the function list the model reads and
//! the globals the sandbox injects cannot drift apart.
//!
//! The anti-drift property is structural, not a convention: the section does not carry its own
//! roster of names. It is handed a [`SurfaceSource`], and the row wires that source to the SAME
//! binding derivation the sandbox uses ([`crate::bind::bindings`] over the snapshot the mirror
//! was built from). A tool that is restricted away is absent from both lists because there is
//! only one list.

use std::sync::Arc;

use bough_plugin_ledger::AgentName;
use bough_plugin_projection::{
    DropPriority, Place, Position, ProjectionError, SectionBody, SectionCites, SectionId,
    SectionRender, SectionRequest, SectionScope, SectionSpec, Slot,
};

use crate::bind::Binding;

/// The section id, and the tie-break key of the section order (P1-D8).
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

/// The section's title.
pub const TITLE: &str = "Code mode";

/// §2.3 item 4: before `identity`, and never dropped — a program surface that degrades is a
/// program that cannot be written.
pub const POSITION: Position = Position {
    slot: Slot::Identity,
    place: Place::Before,
};

/// The id, as a [`SectionId`].
pub fn section_id() -> SectionId {
    SectionId::new(SECTION_ID)
}

/// Where the section gets its roster of injected globals.
///
/// This is the whole anti-drift seam. The row implements it over the same snapshot + binding
/// derivation the sandbox injects from; tests implement it over a fixed list. Nothing else in
/// this module knows any tool name.
pub trait SurfaceSource: Send + Sync + 'static {
    /// The globals injected for `agent`, in the order they should be documented.
    fn bindings(&self, agent: &AgentName) -> Vec<Binding>;

    /// Whether this agent can see the `run` tool at all. `false` ⇒ the section contributes
    /// NOTHING: an agent driving typed tools must not be handed a program surface, and mounting
    /// the row without concealment must not double-document.
    fn sees_run(&self, agent: &AgentName) -> bool;
}

/// The `codemode.surface` renderer.
pub struct Surface {
    /// The live registry, behind the one seam that keeps the docs and the globals identical.
    pub source: Arc<dyn SurfaceSource>,
}

impl Surface {
    /// A renderer over `source`.
    pub fn new(source: Arc<dyn SurfaceSource>) -> Surface {
        Surface { source }
    }

    /// The `SectionSpec` the row contributes through `ctx.projection.section()`.
    pub fn spec(source: Arc<dyn SurfaceSource>) -> SectionSpec {
        SectionSpec {
            id: section_id(),
            position: POSITION,
            scope: SectionScope::Global,
            agent: None,
            priority: DropPriority::Never,
            render: Arc::new(Surface::new(source)),
        }
    }
}

#[async_trait::async_trait]
impl SectionRender for Surface {
    async fn render(&self, req: &SectionRequest) -> Result<Option<SectionBody>, ProjectionError> {
        if !self.source.sees_run(&req.agent) {
            return Ok(None);
        }
        Ok(Some(SectionBody {
            title: TITLE.to_string(),
            body: assemble(&self.source.bindings(&req.agent)),
            // The section is a pure read of the tool REGISTRY, not of the ledger: it cites
            // nothing, and it honours `as_of` vacuously because it reads no row.
            cites: SectionCites::default(),
        }))
    }
}

/// The opening paragraphs: what one round IS under code mode.
const PREAMBLE: &str = "\
You act by writing ONE JavaScript program per round and calling `run` with it. The program runs in
a small embedded sandbox with no filesystem, no network and no module loader of its own: the only
things it can reach are the functions listed below, each of which is a recorded tool call. There
are no other tools and no per-call schemas — this section is the whole surface.

The program is `await`-ed at the top level, so write it straight through. Sequential calls in one
program are FREE: they are the same round. Independent calls run concurrently with `Promise.all`
(or `sh()` for shell legs) — but a call that is not concurrency-safe takes the program's barrier,
so ordering is preserved where it matters without you thinking about it.

A call that fails REJECTS its promise with an `Error` carrying a `kind`
(`not_found`, `denied`, `blocked`, `timeout`, `cancelled`, `error`). Catch it and decide; an
uncaught rejection ends the program and the round, and everything you had not printed is lost.

Everything the program does is on the record — every call is a step, and the console output that
comes back to you is a step too.";

/// Assemble the whole section body from a live binding list.
///
/// Pure and total: the generated roster first (so the model reads what it actually has), then the
/// prose that explains the verbs, then how a turn ends.
pub fn assemble(bindings: &[Binding]) -> String {
    let mut out = String::with_capacity(20 * 1024);
    out.push_str(PREAMBLE);
    out.push_str("\n\n## The functions\n\n");
    out.push_str(&function_table(bindings));
    for section in [SHELL, FILES, PATCH_GRAMMAR, LEDGER, WORK, PRINTING, ENDING] {
        out.push('\n');
        out.push_str(section.trim_end());
        out.push('\n');
    }
    out
}

/// The documented spelling of one well-known verb: the call signature, and the one line that says
/// what it is for. The prose sections carry everything else.
///
/// A binding with no entry here is still listed — the roster is the registry's, not this table's —
/// with a generic spelling. That is the direction the drift must fall: an undocumented tool is
/// visible and slightly under-explained, never a documented tool that does not exist.
const SIGNATURES: &[(&str, &str, &str)] = &[
    (
        "bash",
        "cmd, tags",
        "one shell command in the workspace; returns its combined output",
    ),
    (
        "sh",
        "[{cmd, tag}, …]",
        "several shell commands concurrently; returns [{code, out}, …]",
    ),
    (
        "bg",
        "name, cmd",
        "a background shell that outlives the turn",
    ),
    (
        "bg.output",
        "id",
        "what a background job has printed since you last asked",
    ),
    ("bg.kill", "id", "SIGTERM a background job"),
    (
        "view",
        "path",
        "the file as `[path#TAG]` plus numbered lines",
    ),
    (
        "patch",
        "input",
        "hash-anchored line edits; echoes each file's new tag",
    ),
    (
        "write",
        "path, content",
        "create or rewrite a file; echoes the new tag",
    ),
    (
        "ledger.search",
        "q",
        "steps matching `q` across your connected trajectories",
    ),
    (
        "ledger.steps",
        "range",
        "a specific range of steps, by seq or id",
    ),
    ("ledger.tail", "n", "the last `n` steps of your own chain"),
    ("inbox", "", "the mail this wake has not consumed yet"),
    (
        "claim",
        "{kind, title, body, cites}",
        "write a claim into the shared record",
    ),
    (
        "act",
        "kind, target, payload",
        "an outward act: open_pr | push_to_pr | bot_thread_op | linear_write",
    ),
    ("agent", "prompt, opts", "spawn a worker on separable work"),
    ("fork", "opts", "continue this trajectory in a second agent"),
    ("ask", "q", "ask the human and wait for their answer"),
    (
        "schedule",
        "at, intent",
        "send yourself an intent at a later time",
    ),
];

/// The generated half: one row per injected global, built from the same snapshot the sandbox
/// injects for the agent.
///
/// Deterministic for a given binding list — the roster is rendered in the order the source gives,
/// which is the order the globals are installed.
pub fn function_table(bindings: &[Binding]) -> String {
    if bindings.is_empty() {
        return "No functions are injected for you this round.\n".to_string();
    }
    let mut out = String::new();
    for b in bindings {
        match SIGNATURES.iter().find(|(js, _, _)| *js == b.js) {
            Some((_, args, note)) => {
                out.push_str(&format!("- `await {}({args})` — {note}\n", b.js));
            }
            // An `mcp.<server>.<tool>` or a row-contributed tool with no entry above.
            None => out.push_str(&format!("- `await {}(args)`\n", b.js)),
        }
    }
    out.push_str("\nplus `console.log(...)`, which is the ONLY thing that comes back to you.\n");
    out
}
