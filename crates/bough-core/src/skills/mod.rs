//! Skills: the `/name` instruction bundles a message pulls into one run.
//!
//! A skill is a folder — `<dir>/<name>/SKILL.md` — with YAML-ish frontmatter
//! (`name`, `description`, optional `mcp:` server list) and a markdown body.
//! When a message names one, the body is appended to that turn's system prompt
//! and the servers it lists are granted to the turn. The folder name IS the
//! invocation token: `/x` loads `x/`, and a `name:` field that disagrees with
//! it is ignored.
//!
//! THE INVARIANT THIS HOLDS: **a skill either arrives intact or is reported as
//! broken — never half-parsed into the prompt.** An unterminated frontmatter
//! fence withholds the body entirely and produces a prompt `note` telling the
//! model the skill could not load and why. A prompt that is WRONG is worse
//! than one that is missing (`prompt/assemble.rs` holds the same rule about
//! its sections).
//!
//! SOURCES, FIRST NAME WINS: bundled (shipped with bough, materialized under
//! `~/.bough/bundled-skills/<version>/` at first use) then `~/.bough/skills`
//! (the user's). Bundled first is deliberate in the direction people find
//! surprising — a user folder cannot shadow `history`, so the one skill the
//! harness documents always means what the documentation says.
//!
//! NOTHING IS CACHED. Every listing re-reads the directories and every load
//! re-reads the file, so a SKILL.md edited on disk takes effect on the very
//! next turn with nothing to invalidate.
//!
//! DI OVER GLOBALS: every entry point takes `sources` — a list of
//! `{source, dir}` pairs — and [`default_sources`] builds the real ones. A
//! test passes two temp directories and never touches `~/.bough`.
//!
//! PURE CORE: [`parse_frontmatter`], [`mention_index`] and [`active_skills`]'s
//! selection are pure over strings; only [`load_skill`]/[`list_skills`] touch
//! the filesystem, through sync reads. `widenGrant` is NOT here — the MCP
//! grant arrives with the mcp subsystem (wave 3), and its Rust shape is an
//! enum over live-vs-inherited grants rather than a property getter.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Serialize;

pub mod foreign;

use crate::errors::BoughError;
use crate::paths::{bough_home, user_skills_dir};
use crate::prompt::assemble::{PromptSkill, PromptSkillEntry};
use crate::schema::parts::{Message, Part, Role};
use crate::types::Db;

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

/// Where a skill came from. The panel shows it, so a user copy is never
/// mistaken for the bundled one.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SkillSourceName {
    Bundled,
    User,
    /// A directory inside the workspace: `.agents/skills` (Codex) or
    /// `.claude/skills` (Claude Code), from the git root down.
    Project,
    /// An installed Claude Code or Codex plugin's `skills/`.
    Plugin,
    /// `~/.claude/skills` or `~/.agents/skills` — the foreign user tier.
    Foreign,
}

/// One place skills are discovered from, in precedence order.
#[derive(Clone, Debug)]
pub struct SkillSource {
    pub source: SkillSourceName,
    /// The directory that CONTAINS skill folders, not a skill folder itself.
    pub dir: PathBuf,
    /// The bough plugin this rung belongs to, when it is one.
    ///
    /// CARRIED RATHER THAN DERIVED, because `SkillSourceName::Plugin` is not
    /// specific enough to switch on: a Claude Code plugin's `skills/` and a
    /// Codex marketplace's are that source too, and neither is governed by
    /// bough's switchboard — they belong to the harness that installed them.
    /// This field is set by the one rung `compose_sources` builds from
    /// `~/.bough/plugins`, so a switch can only ever reach what bough owns.
    pub plugin: Option<String>,
}

/// The bundled skill folders, embedded in the binary (ARCHITECTURE §2 /
/// spec small.md §4: `${SKILL_DIR}` must resolve to a REAL on-disk path
/// because bodies reference sidecar files the model runs shell commands
/// against, so the bundle is materialized to disk rather than served from
/// memory). One folder per skill; anything at the bundle root that is not a
/// folder with a SKILL.md is skipped at materialization.
static BUNDLED: include_dir::Dir<'_> = include_dir::include_dir!("$CARGO_MANIFEST_DIR/skills");

/// Where the bundle materializes: versioned so an upgraded binary never
/// serves a stale body.
pub fn bundled_skills_dir() -> PathBuf {
    bough_home()
        .join("bundled-skills")
        .join(env!("CARGO_PKG_VERSION"))
}

/// Write the bundled skill folders into `dest` (one folder per skill,
/// SKILL.md + sidecars). Overwrites in place — the embedded bytes are the
/// source of truth for the bundle, unlike everything else here.
pub fn materialize_bundled_skills(dest: &Path) -> std::io::Result<()> {
    for entry in BUNDLED.dirs() {
        // Only folders that actually are skills ship; a stray file at the
        // bundle root is not a skill and never lands.
        if entry.get_file(entry.path().join("SKILL.md")).is_none() {
            continue;
        }
        for file in walk_files(entry) {
            let target = dest.join(file.path());
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&target, file.contents())?;
        }
    }
    Ok(())
}

fn walk_files<'a>(dir: &'a include_dir::Dir<'a>) -> Vec<&'a include_dir::File<'a>> {
    let mut out: Vec<&include_dir::File> = dir.files().collect();
    for sub in dir.dirs() {
        out.extend(walk_files(sub));
    }
    out
}

/// Materialize once per process, best-effort. A bundle that cannot be written
/// is a missing source directory, which discovery already treats as "nothing
/// installed there" — never a reason to fail a turn.
fn ensure_bundled_skills() -> PathBuf {
    static ONCE: OnceLock<PathBuf> = OnceLock::new();
    ONCE.get_or_init(|| {
        let dest = bundled_skills_dir();
        let _ = materialize_bundled_skills(&dest);
        dest
    })
    .clone()
}

/// Bundled, then the user's. First name wins (spec §16).
///
/// The workspace-independent sources only. Prefer [`sources_for`] anywhere a
/// workspace is in hand — this one cannot see a project's checked-in skills,
/// so it answers a narrower question than most callers mean to ask.
pub fn default_sources() -> Vec<SkillSource> {
    vec![
        SkillSource {
            source: SkillSourceName::Bundled,
            dir: ensure_bundled_skills(),
            plugin: None,
        },
        SkillSource {
            source: SkillSourceName::User,
            dir: user_skills_dir(),
            plugin: None,
        },
    ]
}

/// Where Claude Code keeps its user-level state. Not `paths.rs`'s business:
/// nothing about it moves with `$BOUGH_HOME`.
fn claude_home() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude"))
}

/// Every source that applies to a session working in `workspace`, in
/// precedence order — FIRST WINS, as [`list_skills`] resolves it.
///
/// The order, and the argument for each rung:
///
/// 1. **bundled** — unchanged and deliberately first (spec §16). Nothing a
///    project or a stranger's plugin ships can shadow `history`, so the one
///    skill the harness documents always means what the docs say. This is the
///    rung that makes the rest of the list safe to open up.
/// 2. **project** — `.agents/skills` / `.claude/skills`, nearest directory
///    first. Above the user's own because a skill checked into the repo is the
///    one the repo's work should use; it is also the rung a teammate can
///    change without touching your machine, which is the point of checking it
///    in.
/// 3. **user** — `~/.bough/skills`, the files you wrote for bough.
/// 4. **bough plugins** — `~/.bough/plugins/<name>/skills`. Below the files
///    you wrote and above every foreign directory: a plugin was installed on
///    purpose, which is more intent than a directory that merely exists, and
///    less than a skill you authored.
/// 5. **foreign user** — `~/.claude/skills`, `~/.agents/skills`.
/// 6. **foreign plugins** — installed Claude Code plugins, then Codex marketplaces
///    (repo-scoped before personal). Last because it is the rung with the most
///    directories and the least intent behind any one of them: a plugin was
///    installed for what it does in another harness, not to win a name here.
///
/// NAMES ARE NOT NAMESPACED. Claude Code invokes a plugin skill as
/// `/plugin:skill`; bough's invocation token is the bare folder name, and a
/// colon cannot be one (`name_ok`, and `mention_index` would mis-anchor on
/// it). So a collision is resolved by this order and shown in the panel with
/// its source, rather than by giving the same skill two names.
///
/// NOTHING IS CACHED, here as everywhere else in this module: a plugin
/// installed mid-session is available on the next turn.
pub fn sources_for(workspace: &Path) -> Vec<SkillSource> {
    compose_sources(
        workspace,
        &ensure_bundled_skills(),
        &user_skills_dir(),
        &crate::paths::plugins_dir(),
        dirs::home_dir().as_deref(),
        claude_home().as_deref(),
    )
}

/// [`sources_for`] with every global it reads passed in.
///
/// THE ORDER LIVES HERE AND IS TESTED HERE. `sources_for` reads `BOUGH_HOME`
/// and `$HOME`, and a test that had to redirect those to assert a precedence
/// rule would be mutating process-global state for a question that is really
/// about five paths and their sequence. This is the module's own stated
/// principle (see the header: DI OVER GLOBALS) applied to the one function
/// that had grown a set of globals since.
pub fn compose_sources(
    workspace: &Path,
    bundled: &Path,
    user: &Path,
    plugins: &Path,
    home: Option<&Path>,
    claude_home: Option<&Path>,
) -> Vec<SkillSource> {
    let mut out = vec![SkillSource {
        source: SkillSourceName::Bundled,
        dir: bundled.to_path_buf(),
        plugin: None,
    }];
    out.extend(foreign::project_sources(workspace));
    out.push(SkillSource {
        source: SkillSourceName::User,
        dir: user.to_path_buf(),
        plugin: None,
    });
    // bough's own plugins, below the skills you wrote for bough and above
    // anything another harness put on disk: a plugin is deliberate
    // installation, which is more intent than a directory that happens to be
    // there, and less than a file you authored.
    out.extend(
        crate::paths::plugin_dirs_in(plugins)
            .into_iter()
            .filter_map(|p| {
                let dir = p.join("skills");
                let plugin = p.file_name().and_then(|n| n.to_str()).map(str::to_string);
                dir.is_dir().then_some(SkillSource {
                    source: SkillSourceName::Plugin,
                    dir,
                    plugin,
                })
            }),
    );
    if let Some(home) = home {
        out.extend(foreign::user_sources(home));
    }
    if let Some(claude) = claude_home {
        out.extend(foreign::claude_plugin_sources(claude, workspace));
    }
    // Repo-scoped marketplaces before the personal one: the repo is the more
    // specific statement about this workspace, matching the project rung.
    for market in codex_marketplaces(workspace, home) {
        out.extend(foreign::codex_marketplace_sources(&market));
    }
    out
}

/// The Codex marketplace files that apply: the workspace's own, then the
/// user's. Repo-scoped ones are looked for at the git root and at the
/// workspace itself, which is where Codex documents them.
fn codex_marketplaces(workspace: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut push = |dir: PathBuf| {
        let path = dir.join(".agents").join("plugins").join("marketplace.json");
        if !out.contains(&path) && path.is_file() {
            out.push(path);
        }
    };
    let start = std::path::absolute(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    push(start.clone());
    let mut dir = start;
    for _ in 0..24 {
        if std::fs::metadata(dir.join(".git")).is_ok() {
            push(dir.clone());
            break;
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => break,
        }
    }
    if let Some(home) = home {
        push(home.to_path_buf());
    }
    out
}

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/// One discovered skill, body included. Serialized verbatim by the skills
/// routes, so field names are wire format.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    /// The folder name — the token the user types after `/`.
    pub name: String,
    /// `description:` from the frontmatter, or `""` when it has none.
    pub description: String,
    /// `mcp:` servers this skill needs. Empty when it needs none.
    pub mcp: Vec<String>,
    pub source: SkillSourceName,
    /// The skill's own folder — what `${SKILL_DIR}` resolves to in the body.
    pub dir: String,
    /// The body, frontmatter stripped and `${SKILL_DIR}` resolved. **Empty
    /// when `error` is set** — a skill that could not be parsed contributes
    /// nothing to a prompt rather than contributing its own frontmatter.
    pub body: String,
    /// Why this skill cannot be loaded. Absent = it is fine.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// What one message's `/name` tokens activate.
#[derive(Clone, Debug, Default)]
pub struct ActiveSkills {
    /// Bodies for `PromptInput.skills`, in the order the message named them.
    pub skills: Vec<PromptSkill>,
    /// The union of the loaded skills' `mcp:` servers — the turn's added grant.
    pub servers: Vec<String>,
    /// The names that loaded, in invocation order. For the UI and the logs.
    pub names: Vec<String>,
    /// Volatile prompt notes for skills the message named but that could not
    /// be loaded. Each is a complete markdown section, as `PromptInput.notes`
    /// requires. A named-and-broken skill must not fail silently: the turn
    /// happens either way, and the model is the only thing that can tell the
    /// user their file is wrong.
    pub notes: Vec<String>,
}

/// The token a body may use to point at its own folder.
pub const SKILL_DIR_TOKEN: &str = "${SKILL_DIR}";

/// A loadable skill name: one path segment, no separators, no leading dot.
///
/// `GET /skills/:name` puts a request-supplied string into a path join, so
/// this is the guard on the server's own path construction (`paths::confine`
/// says why that is the case worth stopping). Names that come from a readdir
/// always pass.
fn name_ok(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

// ---------------------------------------------------------------------------
// Frontmatter (pure)
// ---------------------------------------------------------------------------

const FENCE: &str = "---";

/// The result of reading a SKILL.md's head. `error` set = the body is withheld.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frontmatter {
    /// `key: value` pairs from the fenced block. Empty when there is no block.
    pub fields: Vec<(String, String)>,
    /// Everything after the closing fence — or the whole file when there is
    /// no block.
    pub body: String,
    /// Set when the file opens a fence it never closes. `body` is then `""`.
    pub error: Option<String>,
}

impl Frontmatter {
    pub fn field(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Parse a SKILL.md into its frontmatter fields and its body.
///
/// Deliberately NOT a YAML parser: the frontmatter is three flat scalar
/// fields, and a real YAML dependency would accept nested documents this
/// format has no meaning for. Line-based and total —
///
///   - no opening fence at all → the whole file is the body. A SKILL.md that
///     is just instructions is a valid skill; its name comes from its folder
///     either way.
///   - an opening fence with no closing one → `error`, and NO body. This is
///     the case the module header is about.
///   - `key: value` lines, `#` comments and blank lines inside the block;
///     anything else in there is skipped rather than fatal, because a stray
///     line is not worth refusing a skill over.
///
/// The old TS version did `text.split("---")`, which mis-parsed any body
/// containing a horizontal rule — only a line that trimmed equals `---`
/// closes the block.
pub fn parse_frontmatter(raw: &str) -> Frontmatter {
    let text = raw
        .strip_prefix('\u{FEFF}')
        .unwrap_or(raw)
        .replace("\r\n", "\n");
    let lines: Vec<&str> = text.split('\n').collect();

    let mut open = 0;
    while open < lines.len() && lines[open].trim().is_empty() {
        open += 1;
    }
    if open >= lines.len() || lines[open].trim() != FENCE {
        return Frontmatter {
            fields: vec![],
            body: text.trim().to_string(),
            error: None,
        };
    }

    let close = lines[open + 1..]
        .iter()
        .position(|l| l.trim() == FENCE)
        .map(|i| open + 1 + i);
    let Some(close) = close else {
        return Frontmatter {
            fields: vec![],
            body: String::new(),
            error: Some(
                "its frontmatter opens with `---` and never closes. Add a `---` line \
                 after the last field, or delete the opening one — until then the file has \
                 no readable body and the skill cannot be loaded."
                    .to_string(),
            ),
        };
    };

    let mut fields: Vec<(String, String)> = vec![];
    for line in &lines[open + 1..close] {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some(colon) = trimmed.find(':') else {
            continue;
        };
        if colon == 0 {
            continue;
        }
        let key = trimmed[..colon].trim().to_string();
        // First wins, so a duplicated key reads the way the file does top-down.
        if fields.iter().any(|(k, _)| *k == key) {
            continue;
        }
        let value = unquote(trimmed[colon + 1..].trim()).to_string();
        fields.push((key, value));
    }
    Frontmatter {
        fields,
        body: lines[close + 1..].join("\n").trim().to_string(),
        error: None,
    }
}

/// Strip ONE matched pair of YAML quotes. Only a matched pair goes — an
/// apostrophe inside the text has to survive.
fn unquote(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let q = bytes[0];
        if (q == b'"' || q == b'\'') && bytes[bytes.len() - 1] == q {
            return &value[1..value.len() - 1];
        }
    }
    value
}

/// `mcp: chrome-devtools, linear` or `mcp: [a, b]` → `["chrome-devtools", "linear"]`.
pub fn parse_list(value: &str) -> Vec<String> {
    let inner = value.trim();
    let inner = inner.strip_prefix('[').unwrap_or(inner);
    let inner = inner.strip_suffix(']').unwrap_or(inner);
    inner
        .split(',')
        .map(|s| unquote(s.trim()).to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

// ---------------------------------------------------------------------------
// Discovery (filesystem)
// ---------------------------------------------------------------------------

/// Read one candidate folder. `None` = there is no SKILL.md, so it is not a
/// skill.
fn read_skill(source: SkillSourceName, root: &Path, name: &str) -> Option<Skill> {
    if !name_ok(name) {
        return None;
    }
    let dir = root.join(name);
    let text = std::fs::read_to_string(dir.join("SKILL.md")).ok()?;
    let fm = parse_frontmatter(&text);
    let dir_str = dir.to_string_lossy().to_string();
    Some(Skill {
        name: name.to_string(),
        description: fm.field("description").unwrap_or("").to_string(),
        mcp: parse_list(fm.field("mcp").unwrap_or("")),
        source,
        // `${SKILL_DIR}` resolves to the skill's OWN folder, so a body can
        // point at a helper script that lives next to its SKILL.md regardless
        // of the session's workspace (spec §16).
        body: fm.body.replace(SKILL_DIR_TOKEN, &dir_str),
        dir: dir_str,
        error: fm.error,
    })
}

/// Every installed skill, sorted by name, first source wins on a collision.
///
/// A directory that does not exist contributes nothing — a machine with no
/// `~/.bough/skills` is the normal case, not an error. Entries are walked in
/// sorted order so a listing does not depend on filesystem enumeration order.
/// Broken skills ARE listed (with `error`) — the panel shows them.
pub fn list_skills(sources: &[SkillSource]) -> Vec<Skill> {
    list_skills_over(sources, &crate::plugins::state())
}

/// [`list_skills`] against a given switchboard.
///
/// A SWITCHED-OFF SKILL IS NOT LISTED, AND SO DOES NOT WIN ITS NAME. The rung
/// order resolves a collision by first-wins, so a plugin's `review` shadows
/// every `review` below it; if turning it off left it in the list it would go
/// on shadowing them, and the switch would read as "break this name" rather
/// than "use the other one". Dropped before `taken` is consulted, so the next
/// rung's skill takes the name it would have had.
pub fn list_skills_over(
    sources: &[SkillSource],
    switches: &crate::plugins::PluginState,
) -> Vec<Skill> {
    let mut out: Vec<Skill> = vec![];
    let mut taken: Vec<String> = vec![];
    for SkillSource {
        source,
        dir,
        plugin,
    } in sources
    {
        if plugin.as_deref().is_some_and(|p| !switches.plugin_on(p)) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut names: Vec<(String, bool)> = entries
            .flatten()
            .map(|e| {
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                (e.file_name().to_string_lossy().to_string(), is_dir)
            })
            .collect();
        names.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, is_dir) in names {
            if !is_dir || taken.contains(&name) {
                continue;
            }
            if !skill_switched_on(plugin.as_deref(), &name, switches) {
                continue;
            }
            let Some(skill) = read_skill(*source, dir, &name) else {
                continue;
            };
            taken.push(skill.name.clone());
            out.push(skill);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The prompt's skill catalog: every installed skill except the ones whose
/// bodies are already in this turn, and except the ones that are broken.
///
/// A BROKEN SKILL IS NOT LISTED. `list_skills` keeps them so the panel can
/// show you what to fix, but a prompt entry is an instruction to go and read a
/// file, and the file is the thing that does not parse. A skill the user
/// actually NAMED gets a note saying so (`broken_skill_note`); one nobody
/// asked for is a repair job for the panel, not a detour for the turn.
pub fn catalog(sources: &[SkillSource], loaded: &[String]) -> Vec<PromptSkillEntry> {
    list_skills(sources)
        .into_iter()
        .filter(|s| s.error.is_none() && !s.body.trim().is_empty())
        .filter(|s| !loaded.contains(&s.name))
        .map(|s| PromptSkillEntry {
            name: s.name,
            description: s.description,
            dir: s.dir,
        })
        .collect()
}

/// Is this skill's switch on? `None` for a plugin means the rung is not one of
/// bough's plugins, and nothing there has a switch.
fn skill_switched_on(
    plugin: Option<&str>,
    name: &str,
    switches: &crate::plugins::PluginState,
) -> bool {
    match plugin {
        Some(p) => switches.item_on(p, &crate::plugins::skill_id(p, name)),
        None => true,
    }
}

/// One skill by name, resolved in source order. `None` = no such skill.
///
/// The switchboard is consulted HERE and not only in [`list_skills`]: `/name`
/// resolves through this function, so a skill filtered out of the panel but
/// still loadable by name would be a switch that turned off the listing and
/// nothing else.
pub fn load_skill(name: &str, sources: &[SkillSource]) -> Option<Skill> {
    load_skill_over(name, sources, &crate::plugins::state())
}

pub fn load_skill_over(
    name: &str,
    sources: &[SkillSource],
    switches: &crate::plugins::PluginState,
) -> Option<Skill> {
    if !name_ok(name) {
        return None;
    }
    sources
        .iter()
        .filter(|s| s.plugin.as_deref().is_none_or(|p| switches.plugin_on(p)))
        .filter(|s| skill_switched_on(s.plugin.as_deref(), name, switches))
        .find_map(|s| read_skill(s.source, &s.dir, name))
}

// ---------------------------------------------------------------------------
// Invocation (pure)
// ---------------------------------------------------------------------------

/// Where `message` names `/name`, or `None`.
///
/// Anchored on a whitespace boundary before and a non-name character after,
/// so `/history` matches at the start of a line or mid-sentence,
/// `/history-old` does not match `history`, and a path like `/usr/bin/env`
/// names nothing. The index is returned rather than a boolean because it
/// orders the activations: a message that says `/review then /commit` gets
/// them in that order, which is the order their instructions are meant to be
/// read in. (Hand-rolled: the TS regex uses a lookahead the `regex` crate
/// does not support.)
pub fn mention_index(message: &str, name: &str) -> Option<usize> {
    let needle = format!("/{name}");
    let mut from = 0;
    while let Some(pos) = message[from..].find(&needle) {
        let at = from + pos;
        let before_ok = at == 0
            || message[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_whitespace());
        let after = message[at + needle.len()..].chars().next();
        // The TS class `[\w./-]`: a word char, dot, slash or hyphen continues
        // the token and disqualifies the mention.
        let after_ok =
            !after.is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-'));
        if before_ok && after_ok {
            return Some(at);
        }
        from = at + needle.len();
    }
    None
}

/// Everything the message's `/name` tokens activate: the bodies for the
/// prompt, the union of their MCP servers, and a note for each named skill
/// that is broken.
///
/// The skill invocation IS the capability grant (spec §16): a skill that
/// lists `mcp: linear` gets that server connected for the turn without the
/// user enabling it separately, which is why `servers` is returned beside the
/// bodies rather than being something the caller has to dig out of the list.
pub fn active_skills(message: &str, sources: &[SkillSource]) -> ActiveSkills {
    let mut hits: Vec<(usize, Skill)> = list_skills(sources)
        .into_iter()
        .filter_map(|skill| mention_index(message, &skill.name).map(|at| (at, skill)))
        .collect();
    hits.sort_by_key(|(at, _)| *at);

    let mut out = ActiveSkills::default();
    for (_, skill) in hits {
        if skill.error.is_some() || skill.body.trim().is_empty() {
            out.notes.push(broken_skill_note(&skill));
            continue;
        }
        out.names.push(skill.name.clone());
        for server in &skill.mcp {
            if !out.servers.contains(server) {
                out.servers.push(server.clone());
            }
        }
        out.skills.push(PromptSkill {
            name: skill.name,
            body: skill.body,
        });
    }
    out
}

/// What the turn is told about a skill the user named and the harness could
/// not load.
///
/// Addressed to the model because the model is the only thing in the loop
/// that can reach the user mid-turn. It says what was asked for, what is
/// wrong with which file, and what to do anyway — a turn must not stall on
/// this, and it must not pretend the `/name` was never typed.
fn broken_skill_note(skill: &Skill) -> String {
    let why = skill
        .error
        .as_deref()
        .unwrap_or("its SKILL.md has no body below the frontmatter.");
    format!(
        "## Skill /{name} could not be loaded\n\
         The user's message named `/{name}` and a skill folder exists at {dir}, but {why}\n\n\
         Its instructions are NOT in this prompt, so do not act as if you have them. \
         Do the work the user asked for without it, and tell them the file needs fixing.",
        name = skill.name,
        dir = skill.dir,
    )
}

// ---------------------------------------------------------------------------
// The turn's skills
// ---------------------------------------------------------------------------

/// The text of the newest USER message — the one whose `/name` tokens this
/// turn honors.
///
/// Newest rather than "the message that started the turn" because a turn can
/// also begin from a queued drain or a system note (spec §5, §7), and in both
/// cases the user's latest instruction is still the one that decided which
/// skills apply. `system` notes are skipped deliberately: a subagent's report
/// or a job exit is the harness talking, and a `/name` quoted in one is not
/// an invocation.
pub fn invoking_text(messages: &[Message]) -> String {
    for message in messages.iter().rev() {
        if message.role != Role::User {
            continue;
        }
        return message
            .parts
            .iter()
            .filter_map(|p| match p {
                Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    String::new()
}

/// The skills active for a session's current turn, read fresh from the
/// database and the filesystem.
///
/// The session's OWN messages are what is read: a fork's seeded copies count
/// (they are that branch's history), an ancestor's do not (the skill applied
/// to the turn that named it, not forever).
pub fn turn_skills(
    db: &dyn Db,
    session_id: &str,
    sources: &[SkillSource],
) -> Result<ActiveSkills, BoughError> {
    Ok(active_skills(
        &invoking_text(&db.messages_for(session_id)?),
        sources,
    ))
}

// ---------------------------------------------------------------------------
// Tests — ported from src/skills/skills.test.ts
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp source directory, plus a helper that writes skills into it.
    struct TempSource {
        source: SkillSourceName,
        dir: PathBuf,
    }

    impl TempSource {
        fn new(source: SkillSourceName) -> TempSource {
            let dir = std::env::temp_dir().join(format!("bough-skills-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            TempSource { source, dir }
        }
        fn write(&self, name: &str, text: &str) -> PathBuf {
            let folder = self.dir.join(name);
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join("SKILL.md"), text).unwrap();
            folder
        }
        fn as_source(&self) -> SkillSource {
            SkillSource {
                source: self.source,
                dir: self.dir.clone(),
                plugin: None,
            }
        }
    }

    impl Drop for TempSource {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn skill_file(fields: &str, body: &str) -> String {
        format!("---\n{fields}\n---\n\n{body}\n")
    }

    // ---- frontmatter --------------------------------------------------------

    #[test]
    fn frontmatter_fields_read_quotes_stripped_body_starts_after_the_fence() {
        let fm = parse_frontmatter(&skill_file(
            "name: review\ndescription: \"Review a diff, carefully\"\nmcp: linear, github",
            "# Do this\n\nbody.",
        ));
        assert_eq!(fm.error, None);
        assert_eq!(fm.field("description"), Some("Review a diff, carefully"));
        assert_eq!(fm.field("mcp"), Some("linear, github"));
        assert_eq!(fm.body, "# Do this\n\nbody.");
        assert!(!fm.body.contains("description:"), "{}", fm.body);
    }

    #[test]
    fn frontmatter_a_file_with_no_fence_is_all_body() {
        let fm = parse_frontmatter("Just instructions.\n\nMore of them.\n");
        assert_eq!(fm.error, None);
        assert!(fm.fields.is_empty());
        assert_eq!(fm.body, "Just instructions.\n\nMore of them.");
    }

    #[test]
    fn frontmatter_an_unterminated_fence_is_an_error_and_withholds_the_body() {
        let fm =
            parse_frontmatter("---\nname: broken\ndescription: no closing fence\n\nThe body.\n");
        assert!(fm
            .error
            .as_deref()
            .unwrap()
            .contains("opens with `---` and never closes"));
        assert_eq!(fm.body, "");
        assert!(fm.fields.is_empty());
    }

    #[test]
    fn frontmatter_a_rule_inside_the_body_does_not_truncate_it() {
        // The old implementation split the whole file on "---", so a horizontal
        // rule or a fenced block containing one silently ate the rest.
        let fm = parse_frontmatter(&skill_file(
            "description: d",
            "before\n\n---\n\nafter the rule",
        ));
        assert!(fm.body.starts_with("before"), "{}", fm.body);
        assert!(fm.body.ends_with("after the rule"), "{}", fm.body);
    }

    #[test]
    fn frontmatter_comments_blanks_and_junk_tolerated_first_key_wins() {
        let fm = parse_frontmatter(
            "---\n# a comment\n\ndescription: first\ndescription: second\nnot a field\n---\nbody\n",
        );
        assert_eq!(fm.error, None);
        assert_eq!(fm.field("description"), Some("first"));
        assert_eq!(fm.body, "body");
    }

    #[test]
    fn frontmatter_crlf_line_endings_parse_the_same_as_lf() {
        let fm = parse_frontmatter("---\r\ndescription: windows\r\n---\r\nbody\r\n");
        assert_eq!(fm.error, None);
        assert_eq!(fm.field("description"), Some("windows"));
        assert_eq!(fm.body, "body");
    }

    #[test]
    fn mcp_lists_parse_as_a_comma_list_or_a_bracketed_one() {
        assert_eq!(parse_list("linear, github"), vec!["linear", "github"]);
        assert_eq!(parse_list("[a, b]"), vec!["a", "b"]);
        assert!(parse_list("").is_empty());
    }

    // ---- discovery and precedence -------------------------------------------

    #[test]
    fn a_name_in_two_sources_resolves_to_the_bundled_one_first_source_wins() {
        let bundled = TempSource::new(SkillSourceName::Bundled);
        let user = TempSource::new(SkillSourceName::User);
        bundled.write(
            "history",
            &skill_file("description: the bundled one", "BUNDLED BODY"),
        );
        user.write(
            "history",
            &skill_file("description: the shadow", "USER BODY"),
        );
        user.write(
            "mine",
            &skill_file("description: only the user has this", "MINE"),
        );

        let sources = [bundled.as_source(), user.as_source()];
        let listed = list_skills(&sources);
        assert_eq!(
            listed.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["history", "mine"]
        );

        let history = listed.iter().find(|s| s.name == "history").unwrap();
        assert_eq!(history.source, SkillSourceName::Bundled);
        assert_eq!(history.description, "the bundled one");
        assert_eq!(history.body, "BUNDLED BODY");
        // Exactly one row per name: the shadowed copy is resolved away.
        assert_eq!(listed.iter().filter(|s| s.name == "history").count(), 1);

        assert_eq!(
            load_skill("history", &sources).unwrap().body,
            "BUNDLED BODY"
        );
        assert_eq!(
            load_skill("mine", &sources).unwrap().source,
            SkillSourceName::User
        );
        assert_eq!(load_skill("absent", &sources), None);
    }

    #[test]
    fn a_folder_without_skill_md_is_not_a_skill_and_a_missing_source_dir_is_not_an_error() {
        let user = TempSource::new(SkillSourceName::User);
        std::fs::create_dir_all(user.dir.join("scratch")).unwrap();
        std::fs::write(user.dir.join("loose.md"), "not a skill").unwrap();
        user.write("real", &skill_file("description: d", "body"));
        let sources = [
            SkillSource {
                source: SkillSourceName::Bundled,
                dir: user.dir.join("does-not-exist"),
                plugin: None,
            },
            user.as_source(),
        ];
        assert_eq!(
            list_skills(&sources)
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            ["real"]
        );
    }

    /// The other half of the deduplication rule (`hooks::dedupe`): a skill
    /// named twice in one message must not paste its body twice.
    #[test]
    fn a_skill_named_twice_in_one_message_contributes_one_body() {
        let user = TempSource::new(SkillSourceName::User);
        user.write("history", &skill_file("description: d", "HISTORY BODY"));
        let sources = vec![user.as_source()];
        let active = active_skills("use /history, then /history again", &sources);
        assert_eq!(active.names, ["history"], "one hit, not two");
        assert_eq!(active.skills.len(), 1);
        // And two sources holding the same name is still one body — the
        // bundled-first rule already decides which.
        let bundled = TempSource::new(SkillSourceName::Bundled);
        bundled.write("history", &skill_file("description: d", "BUNDLED BODY"));
        let both = active_skills("/history", &[bundled.as_source(), user.as_source()]);
        assert_eq!(both.skills.len(), 1);
        assert_eq!(both.skills[0].body.trim(), "BUNDLED BODY");
    }

    #[test]
    fn a_traversing_name_never_becomes_a_path() {
        let user = TempSource::new(SkillSourceName::User);
        let sources = [user.as_source()];
        assert_eq!(load_skill("../../etc", &sources), None);
        assert_eq!(load_skill("a/b", &sources), None);
        assert_eq!(load_skill(".hidden", &sources), None);
    }

    // ---- the plugin switchboard ---------------------------------------------

    /// A plugin's skill can be switched off one at a time, and switching one
    /// off HANDS ITS NAME BACK: the rung below it wins, which is the difference
    /// between "use the other review" and "break review".
    #[test]
    fn a_switched_off_plugin_skill_gives_its_name_back_to_the_rung_below() {
        let root = std::env::temp_dir().join(format!("bough-sw-{}", uuid::Uuid::new_v4()));
        let put = |dir: PathBuf, name: &str, body: &str| {
            let folder = dir.join(name);
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(
                folder.join("SKILL.md"),
                format!("---\ndescription: d\n---\n{body}"),
            )
            .unwrap();
        };
        let plugin = root.join("plugins").join("acme").join("skills");
        put(plugin.clone(), "review", "PLUGIN");
        put(plugin, "draft", "PLUGIN");
        let foreign = root.join("foreign");
        put(foreign.clone(), "review", "FOREIGN");

        let sources = vec![
            SkillSource {
                source: SkillSourceName::Plugin,
                dir: root.join("plugins").join("acme").join("skills"),
                plugin: Some("acme".into()),
            },
            SkillSource {
                source: SkillSourceName::Foreign,
                dir: foreign,
                plugin: None,
            },
        ];
        let bodies = |state: &crate::plugins::PluginState| -> Vec<(String, String)> {
            list_skills_over(&sources, state)
                .into_iter()
                .map(|s| (s.name, s.body.trim().to_string()))
                .collect()
        };

        assert_eq!(
            bodies(&crate::plugins::PluginState::all_on()),
            [
                ("draft".to_string(), "PLUGIN".to_string()),
                ("review".to_string(), "PLUGIN".to_string()),
            ],
            "a plugin's skill is on until said otherwise"
        );
        let one_off = crate::plugins::PluginState {
            off: vec!["acme/skills/review".into()],
        };
        assert_eq!(
            bodies(&one_off),
            [
                ("draft".to_string(), "PLUGIN".to_string()),
                ("review".to_string(), "FOREIGN".to_string()),
            ],
            "the plugin's other skill is untouched and the name went to the next rung"
        );
        // `/review` resolves through `load_skill`, not the listing, so the
        // switch has to be honoured there too or it turns off only the panel.
        assert_eq!(
            load_skill_over("review", &sources, &one_off)
                .expect("the rung below still has one")
                .body
                .trim(),
            "FOREIGN"
        );
        let all_off = crate::plugins::PluginState {
            off: vec!["acme".into()],
        };
        assert_eq!(
            bodies(&all_off),
            [("review".to_string(), "FOREIGN".to_string())],
            "the plugin's switch takes every skill in it"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // ---- the composed source order ------------------------------------------

    /// The rule the whole design rests on, and until this test it was asserted
    /// nowhere: the PARTS each had coverage, the ORDER they compose into had
    /// none. Every rung is populated with a skill of the same name, so the one
    /// that wins names the rung that won.
    #[test]
    fn every_rung_is_consulted_and_the_precedence_between_them_is_fixed() {
        let root = std::env::temp_dir().join(format!("bough-compose-{}", uuid::Uuid::new_v4()));
        let bundled = root.join("bundled");
        let user = root.join("bough-skills");
        let home = root.join("home");
        let claude = root.join("home").join(".claude");
        let ws = root.join("repo");
        std::fs::create_dir_all(ws.join(".git")).unwrap();

        let put = |dir: PathBuf, body: &str| {
            let folder = dir.join("shared");
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(
                folder.join("SKILL.md"),
                format!("---\ndescription: d\n---\n{body}"),
            )
            .unwrap();
            dir
        };
        put(bundled.clone(), "BUNDLED");
        put(ws.join(".agents").join("skills"), "PROJECT");
        put(user.clone(), "USER");
        put(
            root.join("plugins").join("acme").join("skills"),
            "BOUGH_PLUGIN",
        );
        put(home.join(".claude").join("skills"), "FOREIGN");
        // A plugin, through the real registry shape.
        let install = put(root.join("plug").join("skills"), "PLUGIN");
        std::fs::create_dir_all(claude.join("plugins")).unwrap();
        std::fs::write(
            claude.join("plugins").join("installed_plugins.json"),
            serde_json::json!({"plugins": {"p@m": [{
                "installPath": install.parent().unwrap().to_string_lossy(),
                "scope": "user",
            }]}})
            .to_string(),
        )
        .unwrap();

        let sources = compose_sources(
            &ws,
            &bundled,
            &user,
            &root.join("plugins"),
            Some(&home),
            Some(&claude),
        );
        let kinds: Vec<SkillSourceName> = sources.iter().map(|s| s.source).collect();
        assert_eq!(
            kinds,
            [
                SkillSourceName::Bundled,
                SkillSourceName::Project,
                SkillSourceName::User,
                SkillSourceName::Plugin,
                SkillSourceName::Foreign,
                SkillSourceName::Plugin,
            ],
            "every rung must be present, in this order: {sources:?}"
        );

        // And first-wins actually resolves to the bundled body — the rung that
        // must never be shadowed (spec §16).
        let listed = list_skills(&sources);
        assert_eq!(listed.len(), 1, "one name, one row: {listed:?}");
        assert_eq!(listed[0].body, "BUNDLED");
        assert_eq!(listed[0].source, SkillSourceName::Bundled);

        // Remove the bundled copy and the next rung down takes over, which is
        // what proves the order is a cascade and not just a first entry.
        std::fs::remove_dir_all(bundled.join("shared")).unwrap();
        let listed = list_skills(&compose_sources(
            &ws,
            &bundled,
            &user,
            &root.join("plugins"),
            Some(&home),
            Some(&claude),
        ));
        assert_eq!(
            listed[0].body, "PROJECT",
            "project outranks user and plugin"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_codex_marketplace_is_found_at_the_repo_root_and_in_the_users_home() {
        let root = std::env::temp_dir().join(format!("bough-market-{}", uuid::Uuid::new_v4()));
        let ws = root.join("repo").join("pkg");
        let repo = root.join("repo");
        let home = root.join("home");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        for dir in [&repo, &home] {
            let market = dir.join(".agents").join("plugins");
            std::fs::create_dir_all(&market).unwrap();
            std::fs::write(market.join("marketplace.json"), "{}").unwrap();
        }
        let found = codex_marketplaces(&ws, Some(&home));
        assert_eq!(
            found,
            vec![
                repo.join(".agents/plugins/marketplace.json"),
                home.join(".agents/plugins/marketplace.json"),
            ],
            "repo-scoped first, personal last"
        );
        // A home that has none contributes none, and a workspace outside any
        // repo still answers.
        assert!(codex_marketplaces(&ws, None).len() == 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    // ---- ${SKILL_DIR} -------------------------------------------------------

    #[test]
    fn skill_dir_resolves_to_the_skills_own_folder_everywhere_it_appears() {
        let user = TempSource::new(SkillSourceName::User);
        let folder = user.write(
            "helper",
            &skill_file(
                "description: d",
                "Run `python3 ${SKILL_DIR}/run.py` then read ${SKILL_DIR}/notes.md",
            ),
        );
        let skill = load_skill("helper", &[user.as_source()]).unwrap();
        let folder = folder.to_string_lossy();
        assert_eq!(skill.dir, folder);
        assert_eq!(
            skill.body,
            format!("Run `python3 {folder}/run.py` then read {folder}/notes.md")
        );
        assert!(!skill.body.contains("SKILL_DIR"), "{}", skill.body);
    }

    // ---- invocation ---------------------------------------------------------

    #[test]
    fn a_skill_is_named_at_a_word_boundary_and_only_there() {
        assert!(mention_index("/history what did I do", "history").is_some());
        assert!(mention_index("please /history now", "history").unwrap() > 0);
        assert!(mention_index("look it up with /history", "history").unwrap() > 0);
        assert!(mention_index("/history, then summarize", "history").is_some());
        // Not an invocation: a longer token, a path, or a bare word.
        assert_eq!(mention_index("/history-old", "history"), None);
        assert_eq!(mention_index("/usr/bin/history", "history"), None);
        assert_eq!(mention_index("history of the repo", "history"), None);
        assert_eq!(mention_index("x/history", "history"), None);
    }

    #[test]
    fn named_skills_load_in_invocation_order_with_their_servers_unioned() {
        let user = TempSource::new(SkillSourceName::User);
        user.write(
            "alpha",
            &skill_file("description: a\nmcp: linear, github", "ALPHA BODY"),
        );
        user.write(
            "beta",
            &skill_file("description: b\nmcp: github", "BETA BODY"),
        );
        user.write("gamma", &skill_file("description: c", "GAMMA BODY"));
        let sources = [user.as_source()];

        let active = active_skills("first /beta then /alpha", &sources);
        assert_eq!(active.names, ["beta", "alpha"]);
        assert_eq!(
            active
                .skills
                .iter()
                .map(|s| s.body.as_str())
                .collect::<Vec<_>>(),
            ["BETA BODY", "ALPHA BODY"]
        );
        let mut servers = active.servers.clone();
        servers.sort();
        assert_eq!(servers, ["github", "linear"]);
        assert!(active.notes.is_empty());

        // Nothing named = nothing loaded, and gamma stays out of it.
        assert!(active_skills("no skills here", &sources).skills.is_empty());
    }

    #[test]
    fn a_named_skill_that_cannot_be_parsed_contributes_a_note_never_a_body() {
        let user = TempSource::new(SkillSourceName::User);
        user.write(
            "broken",
            "---\nname: broken\ndescription: unterminated\n\nThe instructions.\n",
        );
        let sources = [user.as_source()];
        let active = active_skills("please /broken this", &sources);
        assert!(active.skills.is_empty());
        assert!(active.names.is_empty());
        assert_eq!(active.notes.len(), 1);
        assert!(active.notes[0].starts_with("## Skill /broken could not be loaded"));
        assert!(active.notes[0].contains("never closes"));
        // The frontmatter itself must not have leaked into what the model is told.
        assert!(
            !active.notes[0].contains("The instructions."),
            "{}",
            active.notes[0]
        );
        // It is still LISTED, with its error — the panel is where the user
        // finds out (the broken-listed-never-omitted gate).
        let listed = list_skills(&sources);
        assert_eq!(listed.len(), 1);
        assert!(listed[0].error.as_deref().unwrap().contains("never closes"));
    }

    #[test]
    fn the_invoking_text_is_the_newest_user_message_not_a_system_note() {
        let message = |role: Role, text: &str, at: i64| Message {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: "s".into(),
            role,
            parts: vec![Part::Text { text: text.into() }],
            pending: false,
            created_at: at,
        };
        assert_eq!(
            invoking_text(&[
                message(Role::User, "old /alpha", 1),
                message(Role::User, "new /beta", 2),
                message(Role::System, "[subagent finished] mentioned /alpha", 3),
            ]),
            "new /beta"
        );
        assert_eq!(invoking_text(&[]), "");
    }

    #[test]
    fn turn_skills_reads_the_sessions_own_newest_user_message() {
        use crate::db::sqlite_db::{DbOptions, SqliteDb};
        use crate::schema::parts::{Session, SessionKind};

        let user = TempSource::new(SkillSourceName::User);
        user.write("alpha", &skill_file("description: a", "ALPHA BODY"));
        let db = SqliteDb::new(":memory:", DbOptions::default()).unwrap();
        let session_id = uuid::Uuid::new_v4().to_string();
        db.create_session(Session {
            id: session_id.clone(),
            title: "t".into(),
            kind: SessionKind::Root,
            created_at: 1,
            parent_id: None,
            origin_id: None,
            origin_message_id: None,
            workspace: None,
            origin_dir: None,
            base: None,
            model: None,
            effort: None,
            draft: None,
            context_tokens: None,
            cached_tokens: None,
            last_llm_at: None,
            outcome_ok: None,
        })
        .unwrap();
        db.create_message(Message {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.clone(),
            role: Role::User,
            parts: vec![Part::Text {
                text: "use /alpha please".into(),
            }],
            pending: false,
            created_at: 2,
        })
        .unwrap();
        let active = turn_skills(&db, &session_id, &[user.as_source()]).unwrap();
        assert_eq!(active.names, ["alpha"]);
    }

    // ---- the bundled skill --------------------------------------------------

    #[test]
    fn the_bundled_history_skill_materializes_and_is_discoverable() {
        let dest = std::env::temp_dir().join(format!("bough-bundle-{}", uuid::Uuid::new_v4()));
        materialize_bundled_skills(&dest).unwrap();
        let sources = [SkillSource {
            source: SkillSourceName::Bundled,
            dir: dest.clone(),
            plugin: None,
        }];

        let skill = load_skill("history", &sources).expect("the history skill ships bundled");
        assert_eq!(skill.source, SkillSourceName::Bundled);
        assert!(!skill.description.is_empty());
        assert_eq!(skill.error, None);
        // It documents the CURRENT schema — the tables it names must be real.
        let schema = include_str!("../db/schema.sql");
        for table in ["messages_fts", "sessions", "messages", "turns"] {
            assert!(
                skill.body.contains(table),
                "history skill should mention {table}"
            );
            assert!(schema.contains(table), "{table} should exist in the schema");
        }
        // And nothing that no longer exists (spec §17: no semantic recall).
        for gone in [
            "recall(",
            "message_embeddings",
            "archived_at",
            "deprecated_at",
        ] {
            assert!(
                !skill.body.contains(gone),
                "history skill must not mention {gone}"
            );
        }
        // Frontmatter is stripped, not appended to the prompt.
        assert!(
            !skill.body.contains("description:"),
            "{}",
            &skill.body[..200.min(skill.body.len())]
        );
        // The TS sources that share the bundle's folder are not skills and
        // never materialize.
        assert!(!dest.join("skills.ts").exists());

        let _ = std::fs::remove_dir_all(&dest);
    }

    // ---- the body reaches the prompt ----------------------------------------

    /// The bug this feature fixes: a skill nobody typed `/name` for was
    /// invisible, so it only ever ran when the user remembered it existed.
    #[test]
    fn every_installed_skill_is_listed_to_the_model_without_being_invoked() {
        use crate::harness::protocol::HostFnName;
        use crate::prompt::assemble::{assemble_prompt, PromptInput, SectionId};
        use crate::schema::parts::SessionKind;

        let user = TempSource::new(SkillSourceName::User);
        user.write(
            "alpha",
            &skill_file("description: does alpha", "ALPHA BODY"),
        );
        user.write("beta", &skill_file("description: does beta", "BETA BODY"));

        let entries = catalog(&[user.as_source()], &[]);
        assert_eq!(
            entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            ["alpha", "beta"]
        );
        assert_eq!(entries[0].description, "does alpha");

        // Name, description and path reach the prompt; the BODY does not —
        // a catalog that carried bodies would be the whole tree every turn.
        let mut input = PromptInput::new(SessionKind::Root, [HostFnName::Bash]);
        input.skill_catalog = entries;
        let prompt = assemble_prompt(&input);
        assert!(prompt.sections.contains(&SectionId::SkillCatalog));
        assert!(prompt.system_volatile.contains("does alpha"));
        assert!(prompt.system_volatile.contains("/alpha/SKILL.md"));
        assert!(!prompt.system_volatile.contains("ALPHA BODY"));
        // The stable prefix is untouched, or every session pays for this.
        let bare = assemble_prompt(&PromptInput::new(SessionKind::Root, [HostFnName::Bash]));
        assert_eq!(prompt.system, bare.system);
    }

    /// Two exclusions, each for its own reason: a loaded skill is already in
    /// the prompt, and a broken one cannot be followed by reading it.
    #[test]
    fn the_catalog_omits_what_is_already_loaded_and_what_cannot_be_read() {
        let user = TempSource::new(SkillSourceName::User);
        user.write("alpha", &skill_file("description: a", "ALPHA BODY"));
        user.write("beta", &skill_file("description: b", "BETA BODY"));
        // Unterminated frontmatter — the module's broken case.
        user.write("busted", "---\ndescription: c\nNO CLOSING FENCE\n");

        let names: Vec<String> = catalog(&[user.as_source()], &["alpha".to_string()])
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["beta"]);
    }

    #[test]
    fn active_skills_bodies_land_in_the_assembled_prompts_volatile_tier() {
        use crate::harness::protocol::HostFnName;
        use crate::prompt::assemble::{assemble_prompt, PromptInput, SectionId};
        use crate::schema::parts::SessionKind;

        let user = TempSource::new(SkillSourceName::User);
        user.write("alpha", &skill_file("description: a", "ALPHA INSTRUCTIONS"));
        let active = active_skills("go /alpha", &[user.as_source()]);

        let mut input = PromptInput::new(SessionKind::Root, [HostFnName::Bash]);
        input.skills = active.skills;
        let prompt = assemble_prompt(&input);
        assert!(prompt.sections.contains(&SectionId::Skills));
        assert!(
            prompt.system_volatile.contains("## Skill: alpha"),
            "{}",
            prompt.system_volatile
        );
        assert!(prompt.system_volatile.contains("ALPHA INSTRUCTIONS"));
        // The stable tier stays byte-identical to a turn with no skills — one
        // volatile byte in the shared prefix would cost every other session
        // the prompt cache.
        let bare = assemble_prompt(&PromptInput::new(SessionKind::Root, [HostFnName::Bash]));
        assert_eq!(prompt.system, bare.system);
    }
}
