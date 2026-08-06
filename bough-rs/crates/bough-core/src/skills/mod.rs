//! Skills: the `/name` instruction bundles a message pulls into one run (port
//! of `src/skills/skills.ts`).
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

use crate::errors::BoughError;
use crate::paths::{bough_home, user_skills_dir};
use crate::prompt::assemble::PromptSkill;
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
}

/// One place skills are discovered from, in precedence order.
#[derive(Clone, Debug)]
pub struct SkillSource {
    pub source: SkillSourceName,
    /// The directory that CONTAINS skill folders, not a skill folder itself.
    pub dir: PathBuf,
}

/// The bundled skill folders, embedded in the binary (ARCHITECTURE §2 /
/// spec small.md §4: `${SKILL_DIR}` must resolve to a REAL on-disk path
/// because bodies reference sidecar files the model runs shell commands
/// against, so the bundle is materialized to disk rather than served from
/// memory). The embedded tree is the TS module's own folder — the canonical
/// bundle location until cutover; non-directory entries (the .ts sources) are
/// skipped at materialization.
static BUNDLED: include_dir::Dir<'_> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/../../../src/skills");

/// Where the bundle materializes: versioned so an upgraded binary never
/// serves a stale body.
pub fn bundled_skills_dir() -> PathBuf {
    bough_home().join("bundled-skills").join(env!("CARGO_PKG_VERSION"))
}

/// Write the bundled skill folders into `dest` (one folder per skill,
/// SKILL.md + sidecars). Overwrites in place — the embedded bytes are the
/// source of truth for the bundle, unlike everything else here.
pub fn materialize_bundled_skills(dest: &Path) -> std::io::Result<()> {
    for entry in BUNDLED.dirs() {
        // Only folders that actually are skills ship; a stray file at the
        // bundle root (the TS sources, today) is not a skill and never lands.
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
pub fn default_sources() -> Vec<SkillSource> {
    vec![
        SkillSource { source: SkillSourceName::Bundled, dir: ensure_bundled_skills() },
        SkillSource { source: SkillSourceName::User, dir: user_skills_dir() },
    ]
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
        self.fields.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
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
    let text = raw.strip_prefix('\u{FEFF}').unwrap_or(raw).replace("\r\n", "\n");
    let lines: Vec<&str> = text.split('\n').collect();

    let mut open = 0;
    while open < lines.len() && lines[open].trim().is_empty() {
        open += 1;
    }
    if open >= lines.len() || lines[open].trim() != FENCE {
        return Frontmatter { fields: vec![], body: text.trim().to_string(), error: None };
    }

    let close = lines[open + 1..].iter().position(|l| l.trim() == FENCE).map(|i| open + 1 + i);
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
        let Some(colon) = trimmed.find(':') else { continue };
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
    Frontmatter { fields, body: lines[close + 1..].join("\n").trim().to_string(), error: None }
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
    let mut out: Vec<Skill> = vec![];
    let mut taken: Vec<String> = vec![];
    for SkillSource { source, dir } in sources {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        let mut names: Vec<(String, bool)> = entries
            .flatten()
            .map(|e| {
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                (e.file_name().to_string_lossy().to_string(), is_dir)
            })
            .collect();
        names.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, is_dir) in names {
            if !is_dir || taken.iter().any(|t| *t == name) {
                continue;
            }
            let Some(skill) = read_skill(*source, dir, &name) else { continue };
            taken.push(skill.name.clone());
            out.push(skill);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// One skill by name, resolved in source order. `None` = no such skill.
pub fn load_skill(name: &str, sources: &[SkillSource]) -> Option<Skill> {
    if !name_ok(name) {
        return None;
    }
    sources.iter().find_map(|s| read_skill(s.source, &s.dir, name))
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
        let before_ok =
            at == 0 || message[..at].chars().next_back().is_some_and(|c| c.is_whitespace());
        let after = message[at + needle.len()..].chars().next();
        // The TS class `[\w./-]`: a word char, dot, slash or hyphen continues
        // the token and disqualifies the mention.
        let after_ok = !after
            .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-'));
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
        out.skills.push(PromptSkill { name: skill.name, body: skill.body });
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
    Ok(active_skills(&invoking_text(&db.messages_for(session_id)?), sources))
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
            SkillSource { source: self.source, dir: self.dir.clone() }
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
        assert!(fm.error.as_deref().unwrap().contains("opens with `---` and never closes"));
        assert_eq!(fm.body, "");
        assert!(fm.fields.is_empty());
    }

    #[test]
    fn frontmatter_a_rule_inside_the_body_does_not_truncate_it() {
        // The old implementation split the whole file on "---", so a horizontal
        // rule or a fenced block containing one silently ate the rest.
        let fm =
            parse_frontmatter(&skill_file("description: d", "before\n\n---\n\nafter the rule"));
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
        bundled.write("history", &skill_file("description: the bundled one", "BUNDLED BODY"));
        user.write("history", &skill_file("description: the shadow", "USER BODY"));
        user.write("mine", &skill_file("description: only the user has this", "MINE"));

        let sources = [bundled.as_source(), user.as_source()];
        let listed = list_skills(&sources);
        assert_eq!(listed.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["history", "mine"]);

        let history = listed.iter().find(|s| s.name == "history").unwrap();
        assert_eq!(history.source, SkillSourceName::Bundled);
        assert_eq!(history.description, "the bundled one");
        assert_eq!(history.body, "BUNDLED BODY");
        // Exactly one row per name: the shadowed copy is resolved away.
        assert_eq!(listed.iter().filter(|s| s.name == "history").count(), 1);

        assert_eq!(load_skill("history", &sources).unwrap().body, "BUNDLED BODY");
        assert_eq!(load_skill("mine", &sources).unwrap().source, SkillSourceName::User);
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
            },
            user.as_source(),
        ];
        assert_eq!(
            list_skills(&sources).iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["real"]
        );
    }

    #[test]
    fn a_traversing_name_never_becomes_a_path() {
        let user = TempSource::new(SkillSourceName::User);
        let sources = [user.as_source()];
        assert_eq!(load_skill("../../etc", &sources), None);
        assert_eq!(load_skill("a/b", &sources), None);
        assert_eq!(load_skill(".hidden", &sources), None);
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
        assert_eq!(skill.body, format!("Run `python3 {folder}/run.py` then read {folder}/notes.md"));
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
        user.write("alpha", &skill_file("description: a\nmcp: linear, github", "ALPHA BODY"));
        user.write("beta", &skill_file("description: b\nmcp: github", "BETA BODY"));
        user.write("gamma", &skill_file("description: c", "GAMMA BODY"));
        let sources = [user.as_source()];

        let active = active_skills("first /beta then /alpha", &sources);
        assert_eq!(active.names, ["beta", "alpha"]);
        assert_eq!(
            active.skills.iter().map(|s| s.body.as_str()).collect::<Vec<_>>(),
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
        user.write("broken", "---\nname: broken\ndescription: unterminated\n\nThe instructions.\n");
        let sources = [user.as_source()];
        let active = active_skills("please /broken this", &sources);
        assert!(active.skills.is_empty());
        assert!(active.names.is_empty());
        assert_eq!(active.notes.len(), 1);
        assert!(active.notes[0].starts_with("## Skill /broken could not be loaded"));
        assert!(active.notes[0].contains("never closes"));
        // The frontmatter itself must not have leaked into what the model is told.
        assert!(!active.notes[0].contains("The instructions."), "{}", active.notes[0]);
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
            parts: vec![Part::Text { text: "use /alpha please".into() }],
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
        let sources = [SkillSource { source: SkillSourceName::Bundled, dir: dest.clone() }];

        let skill = load_skill("history", &sources).expect("the history skill ships bundled");
        assert_eq!(skill.source, SkillSourceName::Bundled);
        assert!(!skill.description.is_empty());
        assert_eq!(skill.error, None);
        // It documents the CURRENT schema — the tables it names must be real.
        let schema = include_str!("../db/schema.sql");
        for table in ["messages_fts", "sessions", "messages", "turns"] {
            assert!(skill.body.contains(table), "history skill should mention {table}");
            assert!(schema.contains(table), "{table} should exist in the schema");
        }
        // And nothing that no longer exists (spec §17: no semantic recall).
        for gone in ["recall(", "message_embeddings", "archived_at", "deprecated_at"] {
            assert!(!skill.body.contains(gone), "history skill must not mention {gone}");
        }
        // Frontmatter is stripped, not appended to the prompt.
        assert!(!skill.body.contains("description:"), "{}", &skill.body[..200.min(skill.body.len())]);
        // The TS sources that share the bundle's folder are not skills and
        // never materialize.
        assert!(!dest.join("skills.ts").exists());

        let _ = std::fs::remove_dir_all(&dest);
    }

    // ---- the body reaches the prompt ----------------------------------------

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
        assert!(prompt.system_volatile.contains("## Skill: alpha"), "{}", prompt.system_volatile);
        assert!(prompt.system_volatile.contains("ALPHA INSTRUCTIONS"));
        // The stable tier stays byte-identical to a turn with no skills — one
        // volatile byte in the shared prefix would cost every other session
        // the prompt cache.
        let bare = assemble_prompt(&PromptInput::new(SessionKind::Root, [HostFnName::Bash]));
        assert_eq!(prompt.system, bare.system);
    }
}
