//! System-prompt assembly: which markdown sections a given turn gets, and in
//! what order (port of `src/prompt/assemble.ts`).
//!
//! THE INVARIANT THIS HOLDS: **the prompt IS the capability grant.** A section
//! that documents a host function is included only when that host function is
//! actually bridged for this turn, and a bridged function always has its
//! section. Get it wrong in one direction and the model calls a verb that
//! rejects with "unknown host function"; get it wrong in the other and a
//! granted capability is invisible and never used.
//!
//! The corollary, which is why the section list is data and not a template:
//! adding a host function means adding a section AND its condition, in one
//! table, next to every other one.
//!
//! WHY MARKDOWN FILES. `sections/<name>.md` IS the prompt — the single source
//! of truth, and the thing a human edits when the model misbehaves. There is
//! deliberately no inlined Rust copy of any section: the old TS tree kept one
//! as a fallback, the two copies drifted, and a deleted paragraph survived in
//! the builtin long after the .md was corrected. A prompt that is WRONG is
//! worse than one that is missing, so a missing section is fatal — here the
//! files are `include_str!`-ed, which makes a missing file fail the BUILD (the
//! strongest form of "fatal at boot"), and [`read_section_file`] keeps the
//! runtime half of the contract for a name outside the table.
//!
//! THE TWO TIERS. `system` is the stable prefix: byte-identical across
//! sessions and turns for a given (kind, capability) shape, so the provider's
//! prompt cache can share it. `system_volatile` carries everything that
//! interpolates a per-session fact — the MCP catalog, skill bodies, and
//! whatever notes the caller resolved. One volatile byte early in the prefix
//! defeats cross-session cache sharing, which is the whole reason for the
//! split.
//!
//! This module is PURE except for its own embedded section text: no db, no
//! clock, no network. The caller resolves the runtime facts and passes them
//! in; that is what makes prompt assembly testable without a turn.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::harness::protocol::HostFnName;
use crate::schema::parts::SessionKind;

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

/// One skill the user's message named, with `${SKILL_DIR}` already resolved.
#[derive(Clone, Debug)]
pub struct PromptSkill {
    pub name: String,
    /// The SKILL.md body, verbatim.
    pub body: String,
}

/// One skill as the catalog lists it — everything needed to decide whether to
/// open it, and nothing that costs a body's worth of tokens.
#[derive(Clone, Debug)]
pub struct PromptSkillEntry {
    pub name: String,
    /// `description:` from the frontmatter. Empty when the skill has none.
    pub description: String,
    /// The skill's folder. The catalog prints `<dir>/SKILL.md`, because a
    /// model told a skill exists and not where it is has to go and search for
    /// it, and the search is the part that gets skipped.
    pub dir: String,
}

/// One tool on a connected MCP server, rendered as a single catalog line.
#[derive(Clone, Debug, Default)]
pub struct PromptMcpTool {
    pub name: String,
    /// Parameter shape, e.g. `({path, limit?})`. Defaults to `()`.
    pub signature: Option<String>,
    /// First line of the tool's description; longer text is the caller's to trim.
    pub description: Option<String>,
}

/// One MCP server as the catalog renders it. A server that failed to connect
/// is listed WITH its error rather than omitted — silence invites the model to
/// invent tools that never came up.
#[derive(Clone, Debug, Default)]
pub struct PromptMcpServer {
    pub name: String,
    pub tools: Vec<PromptMcpTool>,
    pub error: Option<String>,
    /// Why the tool list is unknown, when it is. A granted server is not
    /// connected until something calls it, and rendering that as `(0 tools)`
    /// says the opposite of what is true.
    pub note: Option<String>,
}

/// Everything assembly needs to know about a turn. All of it is resolved by
/// the caller: this module never asks the world anything.
#[derive(Clone, Debug)]
pub struct PromptInput {
    /// Decides the delegation tier and the subagent framing.
    pub kind: SessionKind,
    /// The host functions actually bridged for this turn. This is the
    /// capability grant — every section that documents a verb is gated on its
    /// presence here.
    pub granted: Vec<HostFnName>,
    /// Connected (or failed-to-connect) MCP servers. Empty = no MCP section.
    pub mcp_servers: Vec<PromptMcpServer>,
    /// Functions this workspace's extensions bound into the program's scope
    /// (`crate::extensions`). Not a capability grant to gate — the worker has
    /// already bound them by the time a program runs, so an unlisted one
    /// would be a function the model has and is not told about.
    pub extensions: Vec<crate::harness::protocol::ExtensionFn>,
    /// Skills the user's message named.
    pub skills: Vec<PromptSkill>,
    /// Every skill discoverable from this workspace, as a one-line-each
    /// catalog. Separate from `skills` because they answer different
    /// questions: `skills` is what was invoked, this is what EXISTS. Without
    /// it a skill only ever runs when the user remembers to type `/name`,
    /// which is the failure this field was added for.
    pub skill_catalog: Vec<PromptSkillEntry>,
    /// Per-session notes the caller resolved — the workspace path, background
    /// subagents still running, project rules. Appended verbatim to the
    /// VOLATILE tier so they never poison the shared stable prefix. Each note
    /// is expected to be a complete markdown section with its own heading.
    pub notes: Vec<String>,
}

impl PromptInput {
    /// The common case: a kind and a grant, nothing session-specific.
    pub fn new(kind: SessionKind, granted: impl IntoIterator<Item = HostFnName>) -> Self {
        PromptInput {
            kind,
            granted: granted.into_iter().collect(),
            mcp_servers: Vec::new(),
            extensions: Vec::new(),
            skills: Vec::new(),
            skill_catalog: Vec::new(),
            notes: Vec::new(),
        }
    }
}

/// What the turn runner hands to `LlmClient::run`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssembledPrompt {
    /// The cacheable prefix.
    pub system: String,
    /// The per-session suffix; `""` when there is nothing session-specific.
    pub system_volatile: String,
    /// The ids included, in order — stable tier then volatile. Exposed because
    /// "which sections did this turn get" is the thing tests and the UI want
    /// to assert on.
    pub sections: Vec<SectionId>,
    /// Each included section's id paired with the sha of the exact text that
    /// went into the prefix, in the same order as `sections`. Exists for
    /// prompt attribution: "the file was edited" and "the turn ran with the
    /// edit" are different facts.
    pub shas: Vec<SectionSha>,
}

/// One included section's identity: what it was, the exact bytes it
/// contributed, and how many of them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionSha {
    pub id: SectionId,
    /// sha256 of the section text, truncated — collision-free at this scale,
    /// readable in a log.
    pub sha: String,
    /// Length of the section's text. Assembly is the only place this is
    /// knowable without re-deriving the prompt, and it is what answers "what
    /// am I paying for" — a sha says a section changed, never what it costs.
    #[serde(default)]
    pub bytes: usize,
}

// ---------------------------------------------------------------------------
// The section table
// ---------------------------------------------------------------------------

/// Ids of the sections, in prompt order — the 18 file-backed (stable-tier)
/// ones plus the three volatile ids rendered rather than read from a file.
/// `kebab-case` on the wire so a serialized id is byte-identical to
/// [`SectionId::as_str`] — the form traces, tests and the UI already show.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SectionId {
    Identity,
    Shell,
    History,
    Files,
    PatchGrammar,
    Ask,
    State,
    Milestone,
    Schedule,
    UsingSkills,
    Search,
    Artifact,
    Delegation,
    DelegationNested,
    Workflow,
    Subagent,
    Printing,
    Searching,
    Network,
    Ending,
    // volatile, rendered rather than read from a file
    McpTools,
    Extensions,
    SkillCatalog,
    Skills,
    Notes,
}

impl SectionId {
    /// The TS id string — what tests, traces and the UI show.
    pub fn as_str(&self) -> &'static str {
        match self {
            SectionId::Identity => "identity",
            SectionId::Search => "search",
            SectionId::Shell => "shell",
            SectionId::History => "history",
            SectionId::Files => "files",
            SectionId::PatchGrammar => "patch-grammar",
            SectionId::Ask => "ask",
            SectionId::State => "state",
            SectionId::Milestone => "milestone",
            SectionId::Schedule => "schedule",
            SectionId::UsingSkills => "using-skills",
            SectionId::Artifact => "artifact",
            SectionId::Delegation => "delegation",
            SectionId::DelegationNested => "delegation-nested",
            SectionId::Workflow => "workflow",
            SectionId::Subagent => "subagent",
            SectionId::Printing => "printing",
            SectionId::Searching => "searching",
            SectionId::Network => "network",
            SectionId::Ending => "ending",
            SectionId::McpTools => "mcp-tools",
            SectionId::Extensions => "extensions",
            SectionId::SkillCatalog => "skill-catalog",
            SectionId::Skills => "skills",
            SectionId::Notes => "notes",
        }
    }

    /// Is this section rendered per session rather than read from a file?
    ///
    /// THE TIER IS THE COST MODEL, which is why this is worth asking. A stable
    /// section is byte-identical across sessions and shared in the provider's
    /// prompt cache, so its size is paid once; a volatile one is this
    /// session's alone and paid on every uncached turn. A reader shown one
    /// undifferentiated list of sizes would draw exactly the wrong conclusion
    /// about which ones to shorten.
    pub fn is_volatile(&self) -> bool {
        matches!(
            self,
            SectionId::McpTools
                | SectionId::Extensions
                | SectionId::SkillCatalog
                | SectionId::Skills
                | SectionId::Notes
        )
    }
}

/// The resolved facts a condition asks about.
struct Facts<'a> {
    kind: SessionKind,
    granted: &'a HashSet<HostFnName>,
}

impl Facts<'_> {
    fn has(&self, f: HostFnName) -> bool {
        self.granted.contains(&f)
    }
}

struct SectionSpec {
    id: SectionId,
    file: &'static str,
    /// The embedded file bytes — `include_str!` makes a missing file a build
    /// failure, the strongest form of "fatal at boot".
    raw: &'static str,
    /// Included when this returns true.
    when: fn(&Facts) -> bool,
}

/// A session that may `spawn()` and start workflows: everything but a delegate.
const TOP_LEVEL_KINDS: [SessionKind; 3] = [
    SessionKind::Root,
    SessionKind::Fork,
    SessionKind::Compaction,
];

fn is_top_level(kind: SessionKind) -> bool {
    TOP_LEVEL_KINDS.contains(&kind)
}

fn always(_: &Facts) -> bool {
    true
}

/// The stable tier, in prompt order. This table IS the spec's inclusion table.
///
/// Note what the conditions are made of: a session kind, or a bridged host
/// function — never a flag someone remembered to set. `delegation` is gated on
/// `spawn` because top-level delegation is precisely the tier where detaching
/// is legal, and `delegation-nested` on `agent` because a depth-2 subagent
/// (still kind `subagent`) is bridged nothing and must therefore be told
/// nothing.
static SECTIONS: [SectionSpec; 20] = [
    SectionSpec {
        id: SectionId::Identity,
        file: "identity.md",
        raw: include_str!("sections/identity.md"),
        when: always,
    },
    SectionSpec {
        id: SectionId::Shell,
        file: "shell.md",
        raw: include_str!("sections/shell.md"),
        when: |f| f.has(HostFnName::Bash),
    },
    // Right after shell: the tags bash() requires are what this section makes
    // worth writing, and the two read as one contract. Gated on `bash`, like
    // the MCP catalog: the memory is reached by running `bough tags`, so a
    // turn that cannot run a command cannot reach it.
    SectionSpec {
        id: SectionId::History,
        file: "history.md",
        raw: include_str!("sections/history.md"),
        when: |f| f.has(HostFnName::Bash),
    },
    SectionSpec {
        id: SectionId::Files,
        file: "files.md",
        raw: include_str!("sections/files.md"),
        when: |f| f.has(HostFnName::View),
    },
    SectionSpec {
        id: SectionId::PatchGrammar,
        file: "patch-grammar.md",
        raw: include_str!("sections/patch-grammar.md"),
        when: |f| f.has(HostFnName::Patch),
    },
    SectionSpec {
        id: SectionId::Ask,
        file: "ask.md",
        raw: include_str!("sections/ask.md"),
        when: |f| f.has(HostFnName::Ask),
    },
    SectionSpec {
        id: SectionId::State,
        file: "state.md",
        raw: include_str!("sections/state.md"),
        when: |f| f.has(HostFnName::State),
    },
    // Right after state: both are the session's own bookkeeping. The log is
    // what the sidebar and the summaries read, so it is gated on nothing but
    // the bridge itself.
    SectionSpec {
        id: SectionId::Milestone,
        file: "milestone.md",
        raw: include_str!("sections/milestone.md"),
        when: |f| f.has(HostFnName::Milestone),
    },
    SectionSpec {
        id: SectionId::Schedule,
        file: "schedule.md",
        raw: include_str!("sections/schedule.md"),
        when: |f| f.has(HostFnName::Schedule),
    },
    // Directly above `artifact`, which is the section that sends the model off
    // to read one (`flint`, for charts). Gated on `view`: a turn that cannot
    // open a file cannot act on a skill it was told the path to, and the
    // locations would be noise.
    SectionSpec {
        id: SectionId::UsingSkills,
        file: "using-skills.md",
        raw: include_str!("sections/using-skills.md"),
        when: |f| f.has(HostFnName::View),
    },
    SectionSpec {
        id: SectionId::Search,
        file: "search.md",
        raw: include_str!("sections/search.md"),
        when: |f| f.has(HostFnName::Search),
    },
    SectionSpec {
        id: SectionId::Artifact,
        file: "artifact.md",
        raw: include_str!("sections/artifact.md"),
        when: |f| f.has(HostFnName::Artifact),
    },
    SectionSpec {
        id: SectionId::Delegation,
        file: "delegation.md",
        raw: include_str!("sections/delegation.md"),
        when: |f| is_top_level(f.kind) && f.has(HostFnName::Spawn),
    },
    SectionSpec {
        id: SectionId::DelegationNested,
        file: "delegation-nested.md",
        raw: include_str!("sections/delegation-nested.md"),
        when: |f| f.kind == SessionKind::Subagent && f.has(HostFnName::Agent),
    },
    SectionSpec {
        id: SectionId::Workflow,
        file: "workflow.md",
        raw: include_str!("sections/workflow.md"),
        when: |f| is_top_level(f.kind) && f.has(HostFnName::Workflow),
    },
    SectionSpec {
        id: SectionId::Subagent,
        file: "subagent.md",
        raw: include_str!("sections/subagent.md"),
        when: |f| f.kind == SessionKind::Subagent || f.kind == SessionKind::WorkflowAgent,
    },
    SectionSpec {
        id: SectionId::Printing,
        file: "printing.md",
        raw: include_str!("sections/printing.md"),
        when: always,
    },
    SectionSpec {
        id: SectionId::Searching,
        file: "searching.md",
        raw: include_str!("sections/searching.md"),
        when: always,
    },
    SectionSpec {
        id: SectionId::Network,
        file: "network.md",
        raw: include_str!("sections/network.md"),
        when: always,
    },
    SectionSpec {
        id: SectionId::Ending,
        file: "ending.md",
        raw: include_str!("sections/ending.md"),
        when: always,
    },
];

/// Every stable section's id and file — exported so a test can walk the whole
/// set.
pub fn section_files() -> Vec<(SectionId, &'static str)> {
    SECTIONS.iter().map(|s| (s.id, s.file)).collect()
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// The section text for one file name, trimmed.
///
/// A missing or empty section is FATAL and says so: bough cannot run on a
/// partial prompt, and the failure mode of guessing (a model told about a verb
/// it does not have, or not told about one it does) is worse than not
/// starting. The files are embedded at compile time, so "missing" here means a
/// name outside the table — a broken caller or an incomplete port, and the
/// same non-recoverable condition the TS read failure was.
pub fn read_section_file(file: &str) -> &'static str {
    let Some(spec) = SECTIONS.iter().find(|s| s.file == file) else {
        panic!(
            "cannot read the prompt section {file}: it is not in the embedded section \
             table. bough cannot run without its prompt — this is a broken install or \
             an incomplete checkout, not a recoverable condition."
        );
    };
    let text = spec.raw.trim();
    if text.is_empty() {
        panic!(
            "the prompt section {file} is empty. An empty section silently drops a \
             capability grant from the prompt — restore the file or delete its entry \
             from the section table."
        );
    }
    text
}

/// Fingerprint one section's text. Truncated sha256: 16 hex chars is 64 bits,
/// so a collision across the few hundred distinct section texts a campaign
/// ever sees is not a thing that happens, and the value stays readable in a
/// trace line. The same function the trace writer uses for the prefix shas,
/// so a manifest's section shas and a trace's prefix shas line up.
pub use bough_llm::trace::section_sha;

// ---------------------------------------------------------------------------
// Volatile rendering
// ---------------------------------------------------------------------------

/// Per-server budget for the rendered tool list, so a chatty server can't
/// crowd out the task.
const SERVER_CHARS: usize = 4_000;

fn mcp_tool_line(tool: &PromptMcpTool) -> String {
    let desc = tool
        .description
        .as_deref()
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("")
        .trim();
    let sig = tool.signature.as_deref().unwrap_or("()");
    if desc.is_empty() {
        format!("- {}{}", tool.name, sig)
    } else {
        format!("- {}{} — {}", tool.name, sig, desc)
    }
}

fn mcp_server_block(server: &PromptMcpServer) -> String {
    if let Some(error) = &server.error {
        return format!("server \"{}\": UNAVAILABLE — {}", server.name, error);
    }
    if let Some(note) = &server.note {
        return format!("server \"{}\": {}", server.name, note);
    }
    let mut lines = vec![format!(
        "server \"{}\" ({} tools):",
        server.name,
        server.tools.len()
    )];
    let mut used = 0usize;
    let mut shown = 0usize;
    for tool in &server.tools {
        let line = mcp_tool_line(tool);
        let len = line.chars().count();
        if used + len > SERVER_CHARS {
            break;
        }
        lines.push(line);
        used += len;
        shown += 1;
    }
    let omitted = server.tools.len() - shown;
    if omitted > 0 {
        lines.push(format!("…({omitted} more tools omitted)"));
    }
    lines.join("\n")
}

/// The extensions section: what the user's own JavaScript added to the
/// program's scope.
///
/// The framing matters more than the list. An extension function is NOT an
/// MCP tool (no `bough mcp call`) and NOT a skill (nothing to invoke) — it is
/// a name already bound in the program, indistinguishable in use from
/// `bash()`. Saying so is what stops the model from inventing a calling
/// convention for it.
fn extensions_section(fns: &[crate::harness::protocol::ExtensionFn]) -> String {
    let lines: Vec<String> = fns
        .iter()
        .map(|f| {
            let doc = f
                .doc
                .as_deref()
                .unwrap_or("")
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            let head = format!("- {}{}", f.name, f.signature);
            if doc.is_empty() {
                format!("{head} — defined in {}", f.file)
            } else {
                format!("{head} — {doc} (defined in {})", f.file)
            }
        })
        .collect();
    format!(
        "## Extensions\n\nThis project's extensions bound these functions into your program's \
         scope. They are already in scope: call one directly, exactly as you call `bash()`. \
         They are not tools and not skills — there is no command to invoke them with, and \
         `await` them like any other async call.\n\n{}\n\nOnly the functions listed here \
         exist. If one misbehaves, its source file is named above and you can `view()` it.",
        lines.join("\n")
    )
}

/// The MCP tools section: the calling convention, then a compact per-server
/// catalog. "Only the servers and tools listed here exist" is the load-bearing
/// sentence: the catalog is this turn's grant, and it changes between turns.
fn mcp_tools_section(servers: &[PromptMcpServer]) -> String {
    let blocks: Vec<String> = servers.iter().map(mcp_server_block).collect();
    format!(
        "## MCP tools\n\
         This turn has MCP servers granted. Call one directly — no shell:\n\n\
         ```\n\
         const result = await mcp.call(\"SERVER\", \"TOOL\", {{arg: \"value\"}});\n\
         ```\n\n\
         The third argument is a real object, passed as an object: it never becomes a\n\
         shell word, so quotes, newlines, `$`, backticks and large payloads in it need\n\
         no escaping and cannot corrupt the call. Omit it for a tool that takes no\n\
         parameters. The result comes back parsed. A failure throws, carrying the\n\
         server's own error text, and is catchable like any other host-function\n\
         failure.\n\n\
         `await mcp.list()` is the live catalog — every granted server with its tools,\n\
         and a named error for any that will not connect. You rarely need it: the list\n\
         below is what it would tell you. `bough mcp doctor` says why a server is not\n\
         working. Registering, granting and authorizing are the human's to do — tell\n\
         them to run `bough mcp` or type /mcp rather than improvising a config edit.\n\n\
         The servers below are ready to call. Do not test, probe or otherwise verify one\n\
         before using it: calling a tool is what connects the server, and a call that\n\
         cannot connect tells you so with a better error than a probe would.\n\n\
         Only the servers and tools listed here exist; a tool you do not see is not one\n\
         to guess at.\n\n{}",
        blocks.join("\n\n")
    )
}

/// Skill bodies, appended verbatim under a heading naming the skill.
fn skills_section(skills: &[PromptSkill]) -> String {
    skills
        .iter()
        .map(|s| format!("## Skill: {}\n\n{}", s.name, s.body.trim()))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The one-line-each catalog of skills that exist but were not invoked.
///
/// NAME, DESCRIPTION, PATH — no body. The body is the expensive part and is
/// exactly what the model does not need in order to decide whether to open it,
/// and a catalog that grew bodies would be the whole skills tree in every
/// prompt. The path is included because the alternative is the model listing
/// six directories to find one file, which is the step that gets skipped.
///
/// A skill with no `description:` is still listed: a name alone is a weaker
/// signal than a description, but it is a much stronger one than silence, and
/// omitting it makes an installed skill unreachable without `/name`.
fn skill_catalog_section(entries: &[PromptSkillEntry]) -> String {
    let rows: Vec<String> = entries
        .iter()
        .map(|e| {
            let what = if e.description.trim().is_empty() {
                String::new()
            } else {
                format!(" — {}", e.description.trim())
            };
            format!("- **{}**{} (`{}/SKILL.md`)", e.name, what, e.dir)
        })
        .collect();
    format!(
        "## Skills available\n\n\
         These exist on this machine and apply to this workspace. None of their \
         instructions are in this prompt. When one covers the work you are about to \
         do, READ its `SKILL.md` first and follow it — its pack is more specific \
         than anything here, and skipping it is how a task gets done the wrong way \
         twice. Writing `/name` yourself does nothing; opening the file is the \
         whole mechanism.\n\n{}",
        rows.join("\n")
    )
}

/// Where this turn's verbs actually operate — the one note every turn gets.
///
/// Rendered here rather than kept as a `.md` because it interpolates a
/// per-session path, which is exactly what the volatile tier is for. Two
/// things it has to say: the workspace path at all (without it the model
/// invents a container layout), and that the PROGRAM's own cwd is NOT the
/// workspace (the runtime inherits the server's cwd, so `Bun.file("x")` and
/// `view("x")` in one program name two different files).
pub fn workspace_note(workspace: &str) -> String {
    format!(
        "## Workspace\n\
         The workspace is {workspace} — the user's REAL checkout. bash(), sh() and the\n\
         file verbs (view/patch/write) all start there, and a relative path you give\n\
         THEM resolves against it.\n\n\
         Your program's own working directory is NOT the workspace: the runtime inherits\n\
         the server's directory, so a raw `Bun.file(\"src/x.ts\").text()`,\n\
         `readdir(\".\")` or `process.cwd()` reads somewhere else entirely. When you\n\
         reach past the host functions to the runtime, pass an ABSOLUTE path — join it\n\
         onto the workspace above — or go through bash(), which is already there.\n\n\
         Your edits are immediately real: nothing is copied, staged or confined, and git\n\
         is the source of truth for what changed (`git status`, `git diff`). Deliver work\n\
         with plain git through bash — `git commit`, `git push` — but ONLY when the user\n\
         asks; never as a routine end-of-task step."
    )
}

/// Where temporary files go. NAMED, ABSOLUTE, AND PER SESSION — told only "use
/// a scratch directory", a model keeps reaching for `/tmp`, because that is
/// advice and not an address. The permission sentence matters as much as the
/// path: "write there freely" is a statement about which writes are
/// NOISE-FREE.
pub fn scratch_note(dir: &str) -> String {
    format!(
        "## Scratchpad\n\
         Temporary files go in {dir} — this session's own directory, outside the\n\
         workspace. Intermediate results, debug dumps, a script you are about to run\n\
         once, anything you would otherwise put in /tmp.\n\n\
         Write there freely: nothing in it is reviewed, diffed or reverted. A temp file\n\
         written into the workspace instead is one the human has to read in the changes\n\
         rail and decide about, which is a cost you are imposing on them for your own\n\
         convenience. Use /tmp only if the user asks for it."
    )
}

// ---------------------------------------------------------------------------
// Assembly
// ---------------------------------------------------------------------------

/// Build a turn's system prompt.
///
/// Sections join with a blank line, each already carrying its own `##`
/// heading, so the result is one flat markdown document and adding a section
/// never requires touching the joiner.
pub fn assemble_prompt(input: &PromptInput) -> AssembledPrompt {
    let granted: HashSet<HostFnName> = input.granted.iter().copied().collect();
    let facts = Facts {
        kind: input.kind,
        granted: &granted,
    };

    let mut sections: Vec<SectionId> = Vec::new();
    let mut shas: Vec<SectionSha> = Vec::new();
    let mut stable: Vec<&str> = Vec::new();
    let mut volatile: Vec<String> = Vec::new();

    // Record one section in all three parallel outputs, so they cannot drift
    // apart.
    fn note(sections: &mut Vec<SectionId>, shas: &mut Vec<SectionSha>, id: SectionId, text: &str) {
        sections.push(id);
        shas.push(SectionSha {
            id,
            sha: section_sha(text),
            bytes: text.len(),
        });
    }

    for spec in SECTIONS.iter() {
        if !(spec.when)(&facts) {
            continue;
        }
        let text = read_section_file(spec.file);
        note(&mut sections, &mut shas, spec.id, text);
        stable.push(text);
    }

    // Gated on `bash`, because that is how a tool is called — `bough mcp call`
    // through the shell. A catalog listed to a turn that cannot run a command
    // would be a list of things it cannot reach.
    if !input.mcp_servers.is_empty() && granted.contains(&HostFnName::Bash) {
        let text = mcp_tools_section(&input.mcp_servers);
        note(&mut sections, &mut shas, SectionId::McpTools, &text);
        volatile.push(text);
    }
    // Ungated: unlike MCP, reaching an extension function needs no other
    // capability — the worker bound it into the scope before the program ran.
    if !input.extensions.is_empty() {
        let text = extensions_section(&input.extensions);
        note(&mut sections, &mut shas, SectionId::Extensions, &text);
        volatile.push(text);
    }
    // BEFORE the bodies: a skill whose body is already loaded is not in the
    // catalog (the caller filters it), so the catalog reads as "and these are
    // the ones you would have to go and open" right above the ones you don't.
    if !input.skill_catalog.is_empty() {
        let text = skill_catalog_section(&input.skill_catalog);
        note(&mut sections, &mut shas, SectionId::SkillCatalog, &text);
        volatile.push(text);
    }
    if !input.skills.is_empty() {
        let text = skills_section(&input.skills);
        note(&mut sections, &mut shas, SectionId::Skills, &text);
        volatile.push(text);
    }
    let notes: Vec<&str> = input
        .notes
        .iter()
        .map(|n| n.trim())
        .filter(|n| !n.is_empty())
        .collect();
    if !notes.is_empty() {
        // The notes join into ONE section: they are separate strings only
        // because the caller resolves them separately, and a per-note id would
        // not name anything an experiment can edit.
        let text = notes.join("\n\n");
        note(&mut sections, &mut shas, SectionId::Notes, &text);
        volatile.push(text);
    }

    AssembledPrompt {
        system: stable.join("\n\n"),
        system_volatile: volatile.join("\n\n"),
        sections,
        shas,
    }
}

// ---------------------------------------------------------------------------
// Tests — ported from src/prompt/assemble.test.ts
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::protocol::HOST_FN_NAMES;

    fn all() -> Vec<HostFnName> {
        HOST_FN_NAMES
            .iter()
            .map(|n| HostFnName::parse(n).unwrap())
            .collect()
    }

    /// What every turn bridges: shell + the one editing idiom.
    fn core() -> Vec<HostFnName> {
        vec![
            HostFnName::Bash,
            HostFnName::Sh,
            HostFnName::BashBg,
            HostFnName::BashOutput,
            HostFnName::BashWait,
            HostFnName::BashKill,
            HostFnName::View,
            HostFnName::Patch,
            HostFnName::Write,
        ]
    }

    fn without(drop: &[HostFnName]) -> Vec<HostFnName> {
        all().into_iter().filter(|n| !drop.contains(n)).collect()
    }

    fn build(kind: SessionKind) -> AssembledPrompt {
        assemble_prompt(&PromptInput::new(kind, all()))
    }

    fn build_root() -> AssembledPrompt {
        build(SessionKind::Root)
    }

    /// Whole prompt as one string — for asserting a phrase appears nowhere.
    fn whole(p: &AssembledPrompt) -> String {
        format!("{}\n\n{}", p.system, p.system_volatile)
    }

    /// Whitespace-collapsed and lowercased, for asserting on PROSE: the
    /// sections are hard-wrapped, so a sentence-length phrase straddles a
    /// newline.
    fn flat(text: &str) -> String {
        text.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    }

    fn has(p: &AssembledPrompt, id: SectionId) -> bool {
        p.sections.contains(&id)
    }

    // ---- delegation tier ---------------------------------------------------

    #[test]
    fn a_subagent_gets_the_nested_delegation_section_and_not_the_top_level_one() {
        let sub = build(SessionKind::Subagent);
        assert!(has(&sub, SectionId::DelegationNested));
        assert!(!has(&sub, SectionId::Delegation));
        assert!(sub.system.contains("## Delegation (nested)"));
        assert!(!sub.system.contains("## Delegation to subagents"));
        // Blocking only: a subagent is never told about the detached verbs.
        assert!(!sub.system.contains("await spawn("));
        assert!(!sub.system.contains("await join("));
    }

    #[test]
    fn a_top_level_session_gets_the_top_level_section_and_not_the_nested_one() {
        for kind in [
            SessionKind::Root,
            SessionKind::Fork,
            SessionKind::Compaction,
        ] {
            let p = build(kind);
            assert!(has(&p, SectionId::Delegation), "{kind:?} should delegate");
            assert!(
                !has(&p, SectionId::DelegationNested),
                "{kind:?} is not nested"
            );
            assert!(p.system.contains("await spawn(task, {name})"));
        }
    }

    #[test]
    fn subagent_framing_rides_on_kind_delegation_on_the_grant() {
        let sub = build(SessionKind::Subagent);
        assert!(sub.system.contains("## You are a subagent"));

        let wf_agent = build(SessionKind::WorkflowAgent);
        assert!(wf_agent.system.contains("## You are a subagent"));
        // A workflow agent delegates nothing at all.
        assert!(!has(&wf_agent, SectionId::Delegation));
        assert!(!has(&wf_agent, SectionId::DelegationNested));

        // Depth 2: still kind subagent, but nothing is bridged — so nothing is
        // granted.
        let deepest = assemble_prompt(&PromptInput::new(SessionKind::Subagent, core()));
        assert!(!has(&deepest, SectionId::DelegationNested));
        assert!(deepest.system.contains("## You are a subagent"));

        let root = build_root();
        assert!(!root.system.contains("## You are a subagent"));
    }

    #[test]
    fn workflows_are_offered_only_to_a_session_that_may_start_one() {
        assert!(has(&build_root(), SectionId::Workflow));
        let revoked = assemble_prompt(&PromptInput::new(
            SessionKind::Root,
            without(&[HostFnName::Workflow]),
        ));
        assert!(!has(&revoked, SectionId::Workflow));
        assert!(!has(&build(SessionKind::Subagent), SectionId::Workflow));
    }

    // ---- the capability grant ----------------------------------------------

    /// Section → the host function it grants, and a phrase only that section
    /// carries.
    const GRANTS: [(SectionId, HostFnName, &str); 10] = [
        (
            SectionId::Shell,
            HostFnName::Bash,
            "await bashBg(name, cmd)",
        ),
        (SectionId::Files, HostFnName::View, "await view(path)"),
        (SectionId::PatchGrammar, HostFnName::Patch, "INS.HEAD:"),
        (SectionId::Ask, HostFnName::Ask, "await ask(question"),
        (SectionId::State, HostFnName::State, "await state.get(key)"),
        (
            SectionId::Milestone,
            HostFnName::Milestone,
            "await milestone(text)",
        ),
        (
            SectionId::Schedule,
            HostFnName::Schedule,
            "await schedule.list()",
        ),
        (
            SectionId::Artifact,
            HostFnName::Artifact,
            "await artifact(name, content)",
        ),
        (
            SectionId::Delegation,
            HostFnName::Spawn,
            "await spawn(task, {name})",
        ),
        (
            SectionId::Workflow,
            HostFnName::Workflow,
            "await workflow.start(",
        ),
    ];

    #[test]
    fn a_section_granting_a_host_function_is_absent_when_the_capability_is_absent() {
        for (id, func, phrase) in GRANTS {
            let granted = build_root();
            assert!(
                has(&granted, id),
                "{id:?} should be present when {func:?} is granted"
            );
            assert!(granted.system.contains(phrase));

            let revoked = assemble_prompt(&PromptInput::new(SessionKind::Root, without(&[func])));
            assert!(
                !has(&revoked, id),
                "{id:?} must be absent when {func:?} is not granted"
            );
            assert!(
                !whole(&revoked).contains(phrase),
                "no section may document {func:?} when it is not granted (found {phrase:?})"
            );
        }
    }

    #[test]
    fn a_core_only_turn_gets_exactly_the_always_on_sections() {
        let p = assemble_prompt(&PromptInput::new(SessionKind::Root, core()));
        assert_eq!(
            p.sections,
            vec![
                SectionId::Identity,
                SectionId::Shell,
                // The memory is a `bough tags` invocation, not a host verb —
                // the section rides with `bash`.
                SectionId::History,
                SectionId::Files,
                SectionId::PatchGrammar,
                // Where skills live — reading one needs `view`, so it rides
                // with the file sections rather than with any one capability.
                SectionId::UsingSkills,
                SectionId::Printing,
                SectionId::Searching,
                SectionId::Network,
                SectionId::Ending,
            ]
        );
        assert_eq!(p.system_volatile, "");
    }

    // ---- the volatile tier -------------------------------------------------

    #[test]
    fn mcp_tools_appear_only_when_servers_are_connected_and_stay_out_of_the_stable_tier() {
        let none = build_root();
        assert!(!has(&none, SectionId::McpTools));
        assert_eq!(none.system_volatile, "");

        let mut input = PromptInput::new(SessionKind::Root, all());
        input.mcp_servers = vec![
            PromptMcpServer {
                name: "files".into(),
                tools: vec![PromptMcpTool {
                    name: "read_file".into(),
                    signature: Some("({path})".into()),
                    description: Some("Read a file\nmore".into()),
                }],
                ..Default::default()
            },
            PromptMcpServer {
                name: "broken".into(),
                error: Some("exited before handshake".into()),
                ..Default::default()
            },
        ];
        let p = assemble_prompt(&input);
        assert!(has(&p, SectionId::McpTools));
        assert!(p.system_volatile.contains("## MCP tools"));
        // The call idiom is the host function, not a shell word: the whole
        // point of `mcp.call` is that the arguments are never quoted twice.
        assert!(p
            .system_volatile
            .contains("await mcp.call(\"SERVER\", \"TOOL\", {arg: \"value\"})"));
        assert!(
            !p.system_volatile.contains("bough mcp call"),
            "the shell idiom must not still be taught alongside the host fn"
        );
        assert!(p
            .system_volatile
            .contains("- read_file({path}) — Read a file"));
        // A failed server is named with its error, not silently dropped.
        assert!(p
            .system_volatile
            .contains("server \"broken\": UNAVAILABLE — exited before handshake"));

        // Nor is a granted server whose tools are not known yet rendered as
        // `(0 tools)`, which reads as "this server has nothing".
        let mut pending_input = PromptInput::new(SessionKind::Root, all());
        pending_input.mcp_servers = vec![PromptMcpServer {
            name: "notion".into(),
            note: Some("granted, not connected yet — call it to connect".into()),
            ..Default::default()
        }];
        let pending = assemble_prompt(&pending_input);
        assert!(pending
            .system_volatile
            .contains("server \"notion\": granted, not connected yet"));
        assert!(!pending.system_volatile.contains("0 tools"));

        // The catalog is per-session; it must never reach the cacheable prefix.
        assert!(!p.system.contains("MCP tools"));

        // NO SHELL, NO CATALOG. A tool is called by running `bough mcp call`,
        // so a turn that cannot run a command cannot reach one.
        let mut no_bash = PromptInput::new(SessionKind::Root, without(&[HostFnName::Bash]));
        no_bash.mcp_servers = vec![PromptMcpServer {
            name: "files".into(),
            ..Default::default()
        }];
        assert!(!has(&assemble_prompt(&no_bash), SectionId::McpTools));
    }

    #[test]
    fn skills_and_caller_notes_land_in_the_volatile_tier_only() {
        let mut input = PromptInput::new(SessionKind::Root, all());
        input.skills = vec![PromptSkill {
            name: "history".into(),
            body: "Query ~/.bough/bough.db with sqlite3.".into(),
        }];
        input.notes = vec![
            "# Workspace\nbash starts in /repo.".into(),
            "   ".into(),
            "".into(),
        ];
        let p = assemble_prompt(&input);
        assert_eq!(
            &p.sections[p.sections.len() - 2..],
            &[SectionId::Skills, SectionId::Notes]
        );
        assert!(p.system_volatile.contains("## Skill: history"));
        assert!(p.system_volatile.contains("Query ~/.bough/bough.db"));
        assert!(p.system_volatile.contains("bash starts in /repo."));
        assert!(!p.system.contains("/repo"));
        // Blank notes are dropped rather than joined into stray separators.
        assert!(!p.system_volatile.contains("\n\n\n"));
    }

    /// Extensions are volatile-tier: they vary per workspace, and one
    /// varying byte in the STABLE prefix costs every session the shared
    /// prompt cache (`turn/runner.rs`). This is the assertion that keeps a
    /// per-workspace surface out of the cacheable half.
    #[test]
    fn extensions_render_into_the_volatile_tier_and_never_the_stable_one() {
        let mut input = PromptInput::new(SessionKind::Root, all());
        input.extensions = vec![
            crate::harness::protocol::ExtensionFn {
                name: "deploy".into(),
                signature: "(env)".into(),
                doc: Some("Ship to an environment".into()),
                file: "/repo/.agents/extensions/ops.js".into(),
            },
            crate::harness::protocol::ExtensionFn {
                name: "rollback".into(),
                signature: "()".into(),
                doc: None,
                file: "/repo/.agents/extensions/ops.js".into(),
            },
        ];
        let p = assemble_prompt(&input);

        assert!(has(&p, SectionId::Extensions));
        assert!(p
            .system_volatile
            .contains("- deploy(env) — Ship to an environment"));
        // Undocumented is still listed: it is bound either way, and omitting
        // it would hide a function the model has.
        assert!(p.system_volatile.contains("- rollback()"));
        assert!(p
            .system_volatile
            .contains("/repo/.agents/extensions/ops.js"));
        // Nothing workspace-specific reaches the cacheable prefix. Asserted on
        // the heading and the file path rather than the bare word "deploy",
        // which the stable sections legitimately use in their own prose.
        assert!(
            !p.system.contains("## Extensions") && !p.system.contains("ops.js"),
            "the cacheable prefix must not vary per workspace"
        );
    }

    /// No extensions, no section — an empty heading would invite the model to
    /// invent functions to fill it.
    #[test]
    fn no_extensions_renders_no_section() {
        let p = assemble_prompt(&PromptInput::new(SessionKind::Root, all()));
        assert!(!has(&p, SectionId::Extensions));
    }

    #[test]
    fn the_stable_tier_is_byte_identical_for_the_same_shape() {
        let mut a_input = PromptInput::new(SessionKind::Root, all());
        a_input.mcp_servers = vec![PromptMcpServer {
            name: "x".into(),
            ..Default::default()
        }];
        a_input.notes = vec!["# A\nfirst".into()];
        let mut b_input = PromptInput::new(SessionKind::Root, all());
        b_input.mcp_servers = vec![PromptMcpServer {
            name: "y".into(),
            ..Default::default()
        }];
        b_input.notes = vec!["# B\nsecond".into()];
        let a = assemble_prompt(&a_input);
        let b = assemble_prompt(&b_input);
        assert_eq!(a.system, b.system);
        assert_ne!(a.system_volatile, b.system_volatile);
    }

    // ---- content: the prompt has to match THIS spec ------------------------

    #[test]
    fn the_prompt_grants_view_patch_write_and_nothing_else_for_files() {
        let text = whole(&build_root());
        for gone in [
            "await read(",
            "await edit(",
            "await extract(",
            "await recall(",
        ] {
            assert!(!text.contains(gone), "{gone} was removed from the spec");
        }
        assert!(flat(&text).contains("there is no read() and no edit()."));
        assert!(text.contains("await write(path, content)"));
    }

    #[test]
    fn there_is_no_done_gate_and_no_committed_check() {
        let text = flat(&whole(&build_root()));
        for gone in [
            "done-gate",
            "committed check",
            "checkpassed",
            "re-runs the committed",
        ] {
            assert!(
                !text.contains(gone),
                "{gone:?} belongs to the old acceptance gate"
            );
        }
        assert!(text.contains("there is no acceptance gate in this harness"));
        // `done` survives as a report, and stop is what ends a turn.
        assert!(text.contains("it is a report, not a gate"));
        assert!(text.contains("call the stop tool"));
    }

    #[test]
    fn the_network_section_states_plainly_that_nothing_filters_egress() {
        let p = build_root();
        assert!(has(&p, SectionId::Network));
        assert!(p.system.contains("## Network"));
        let text = flat(&p.system);
        assert!(text.contains("you have network access, and nothing filters it"));
        assert!(text.contains(
            "there is no egress proxy, no allowlist, no credential gate, and no review step"
        ));
        assert!(
            !text.contains("egress gate"),
            "there is no egress gate to describe"
        );
    }

    // ---- the section files themselves --------------------------------------

    #[test]
    fn every_section_in_the_table_has_a_readable_headed_file() {
        for (id, file) in section_files() {
            let text = read_section_file(file);
            assert!(
                text.starts_with("## "),
                "{id:?} ({file}) must start with its own \"## \" heading"
            );
            assert!(text.len() > 100, "{id:?} ({file}) looks truncated");
        }
    }

    #[test]
    fn a_missing_section_file_is_fatal_and_says_why() {
        let err = std::panic::catch_unwind(|| read_section_file("no-such-section.md"))
            .expect_err("a name outside the table must be fatal");
        let message = err
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default();
        assert!(message.contains("no-such-section.md"), "message: {message}");
        assert!(
            message.contains("not a recoverable condition"),
            "message: {message}"
        );
    }

    // ---- the workspace note ------------------------------------------------

    #[test]
    fn the_workspace_note_names_the_path_and_rides_the_volatile_tier() {
        let note = workspace_note("/home/u/proj");
        assert!(
            note.starts_with("## Workspace"),
            "a note is a complete section with its own heading"
        );
        assert!(note.contains("/home/u/proj"));

        let mut input = PromptInput::new(SessionKind::Root, all());
        input.notes = vec![note];
        let p = assemble_prompt(&input);
        assert!(has(&p, SectionId::Notes));
        // The stable prefix is shared across sessions and cached by the
        // provider; one session's workspace path in it would defeat that for
        // every other session.
        assert!(
            !p.system.contains("/home/u/proj"),
            "a per-session path must never enter the stable tier"
        );
        assert!(p.system_volatile.contains("/home/u/proj"));
    }

    #[test]
    fn the_scratchpad_note_names_an_absolute_path_and_stays_out_of_the_stable_tier() {
        // Told only "use a scratch directory", a model keeps reaching for
        // /tmp, because that is advice and not an address. So the assertion is
        // that the path itself is in the text.
        let note = scratch_note("/home/u/.bough/scratch/abc123");
        assert!(note.starts_with("## Scratchpad"));
        assert!(note.contains("/home/u/.bough/scratch/abc123"));
        let text = flat(&note);
        assert!(text.contains("/tmp")); // …and says what it replaces
                                        // The reason, in the form that transfers: a temp file in the checkout
                                        // is work the human has to review.
        assert!(text.contains("changes"));

        let mut input = PromptInput::new(SessionKind::Root, all());
        input.notes = vec![scratch_note("/home/u/.bough/scratch/abc123")];
        let p = assemble_prompt(&input);
        // Per-session, so it must never enter the prefix every other session
        // shares.
        assert!(
            !p.system.contains("abc123"),
            "a per-session path must never enter the stable tier"
        );
        assert!(p.system_volatile.contains("abc123"));
    }

    #[test]
    fn the_workspace_note_warns_that_the_programs_own_cwd_is_not_the_workspace() {
        let text = flat(&workspace_note("/w"));
        assert!(text.contains("your program's own working directory is not the workspace"));
        assert!(text.contains("bun.file"));
        assert!(text.contains("absolute"));
    }

    #[test]
    fn the_workspace_note_is_not_gated_on_a_capability() {
        // Every kind edits a real checkout.
        for kind in [
            SessionKind::Root,
            SessionKind::Fork,
            SessionKind::Compaction,
            SessionKind::Subagent,
            SessionKind::WorkflowAgent,
        ] {
            let ws = format!(
                "/w/{}",
                match kind {
                    SessionKind::Root => "root",
                    SessionKind::Fork => "fork",
                    SessionKind::Compaction => "compaction",
                    SessionKind::Subagent => "subagent",
                    SessionKind::WorkflowAgent => "workflow_agent",
                    _ => unreachable!(),
                }
            );
            let mut input = PromptInput::new(kind, core());
            input.notes = vec![workspace_note(&ws)];
            let p = assemble_prompt(&input);
            assert!(p.system_volatile.contains(&ws));
        }
    }

    // ---- section fingerprints ----------------------------------------------

    #[test]
    fn every_included_section_is_fingerprinted_in_prompt_order() {
        let mut input = PromptInput::new(SessionKind::Root, all());
        input.skills = vec![PromptSkill {
            name: "s".into(),
            body: "B".into(),
        }];
        input.notes = vec!["## N\nnote".into()];
        let p = assemble_prompt(&input);
        assert_eq!(
            p.shas.iter().map(|s| s.id).collect::<Vec<_>>(),
            p.sections,
            "shas parallel sections exactly"
        );
        assert!(
            p.shas
                .iter()
                .all(|s| s.sha.len() == 16 && s.sha.chars().all(|c| c.is_ascii_hexdigit())),
            "each sha is truncated sha256"
        );
    }

    #[test]
    fn a_sections_sha_is_over_the_text_that_actually_went_into_the_prefix() {
        let p = build_root();
        let identity = p.shas.iter().find(|s| s.id == SectionId::Identity).unwrap();
        assert_eq!(identity.sha, section_sha(read_section_file("identity.md")));
        // The point of the exercise: an edit to one .md moves exactly one sha,
        // so a flipped task can be attributed to a file rather than to "the
        // prompt".
        let shell = p.shas.iter().find(|s| s.id == SectionId::Shell).unwrap();
        assert_ne!(shell.sha, identity.sha);
    }

    #[test]
    fn a_turn_without_a_capability_carries_no_fingerprint_for_its_section() {
        let p = assemble_prompt(&PromptInput::new(
            SessionKind::Root,
            without(&[HostFnName::Artifact]),
        ));
        assert!(!p.shas.iter().any(|s| s.id == SectionId::Artifact));
        // An experiment editing artifact.md must not count this turn as
        // exposed to it.
    }
}
