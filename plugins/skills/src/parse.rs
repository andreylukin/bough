//! Invariant: parsing is PURE and refuses LOUDLY. A skill file with no `name` or an empty
//! `triggers` list is not a skill that never fires — it is a misconfiguration, and §0.2 makes a
//! misconfigured row a failure that NAMES THE FILE.

use std::path::Path;

use bough_plugin_projection::SectionId;

/// A skill file: YAML frontmatter + markdown body.
#[derive(Clone, Debug, PartialEq)]
pub struct Skill {
    pub id: SectionId,
    pub name: String,
    pub description: String,
    pub triggers: Vec<String>,
    pub body: String,
}

/// The frontmatter, exactly as it is spelled in the file.
#[derive(Debug, serde::Deserialize)]
struct Frontmatter {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    triggers: Vec<String>,
}

/// PURE: parse, and refuse loudly on a missing `name` or empty `triggers`.
///
/// The frontmatter is the leading `---` fence; everything after the closing fence is the body,
/// verbatim (leading blank lines trimmed, so a body always starts at its first real line).
pub fn parse_skill(path: &Path, text: &str) -> Result<Skill, SkillError> {
    let p = || path.display().to_string();
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
        .ok_or_else(|| SkillError::NoFrontmatter { path: p() })?;
    // The closing fence: a line that is exactly `---`.
    let mut front = String::new();
    let mut body = None;
    let mut consumed = 0usize;
    for line in rest.split_inclusive('\n') {
        consumed += line.len();
        if line.trim_end_matches(['\r', '\n']) == "---" {
            body = Some(&rest[consumed..]);
            break;
        }
        front.push_str(line);
    }
    let body = body.ok_or_else(|| SkillError::NoFrontmatter { path: p() })?;

    let fm: Frontmatter = serde_yaml::from_str(&front).map_err(|e| SkillError::BadFrontmatter {
        path: p(),
        detail: e.to_string(),
    })?;
    let name = fm
        .name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .ok_or_else(|| SkillError::NoName { path: p() })?;
    let triggers: Vec<String> = fm
        .triggers
        .iter()
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    if triggers.is_empty() {
        return Err(SkillError::NoTriggers { path: p() });
    }
    Ok(Skill {
        id: SectionId::new(format!("skill:{name}")),
        name,
        description: fm.description.unwrap_or_default().trim().to_string(),
        triggers,
        body: body.trim_start_matches(['\n', '\r']).to_string(),
    })
}

/// PURE: does this request mention the skill? Case-insensitive WHOLE-WORD match of any trigger
/// against the scanned text.
///
/// "Whole word" means the characters either side of the match are not alphanumeric and not `_`.
/// A multi-word trigger (`"code review"`) is matched as one span with the same rule at its edges,
/// so `recode reviewer` does not fire `code review`.
pub fn mentioned(skill: &Skill, scanned: &str) -> bool {
    let hay = scanned.to_lowercase();
    skill.triggers.iter().any(|t| whole_word(&hay, t))
}

/// PURE: is `needle` (already lowercase) present in `hay` (already lowercase) as a whole word?
fn whole_word(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let bytes = hay.as_bytes();
    let mut from = 0usize;
    while let Some(rel) = hay[from..].find(needle) {
        let start = from + rel;
        let end = start + needle.len();
        let before_ok = start == 0 || !is_word_byte(bytes[start - 1]);
        let after_ok = end == bytes.len() || !is_word_byte(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        // Advance by one char so a match inside a word does not hide a later real one.
        from = start + hay[start..].chars().next().map(char::len_utf8).unwrap_or(1);
        if from >= hay.len() {
            break;
        }
    }
    false
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// What a skill file goes wrong as. Every variant names the file.
#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("skill `{path}`: no YAML frontmatter")]
    NoFrontmatter { path: String },
    #[error("skill `{path}`: frontmatter is not valid YAML: {detail}")]
    BadFrontmatter { path: String, detail: String },
    #[error("skill `{path}`: a skill needs a `name`")]
    NoName { path: String },
    #[error("skill `{path}`: a skill needs at least one trigger, or it can never inject")]
    NoTriggers { path: String },
    #[error("skill `{path}`: {bytes} bytes exceeds `max_bytes` ({max})")]
    TooBig {
        path: String,
        bytes: usize,
        max: usize,
    },
    #[error("skill `{path}`: {detail}")]
    Io { path: String, detail: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn path() -> PathBuf {
        PathBuf::from("/skills/review.md")
    }

    fn ok_text() -> &'static str {
        "---\nname: review\ndescription: how to review\ntriggers: [\"code review\", PR]\n---\n\nStep one.\n"
    }

    #[test]
    fn a_well_formed_file_parses_into_id_name_triggers_and_body() {
        let s = parse_skill(&path(), ok_text()).expect("parses");
        assert_eq!(s.name, "review");
        assert_eq!(s.id.as_str(), "skill:review");
        assert_eq!(s.description, "how to review");
        assert_eq!(
            s.triggers,
            vec!["code review".to_string(), "pr".to_string()]
        );
        assert_eq!(s.body, "Step one.\n");
    }

    #[test]
    fn a_file_with_no_name_is_refused_naming_the_file() {
        let err = parse_skill(&path(), "---\ntriggers: [x]\n---\nbody\n").expect_err("refused");
        assert!(matches!(err, SkillError::NoName { .. }), "{err}");
        assert!(err.to_string().contains("/skills/review.md"), "{err}");
    }

    #[test]
    fn a_file_with_empty_triggers_is_refused_naming_the_file() {
        let err =
            parse_skill(&path(), "---\nname: n\ntriggers: []\n---\nbody\n").expect_err("refused");
        assert!(matches!(err, SkillError::NoTriggers { .. }), "{err}");
        assert!(err.to_string().contains("/skills/review.md"), "{err}");
        // A triggers list of blanks is the same thing: it can never inject.
        let err = parse_skill(&path(), "---\nname: n\ntriggers: [\"  \"]\n---\nb\n")
            .expect_err("refused");
        assert!(matches!(err, SkillError::NoTriggers { .. }), "{err}");
    }

    #[test]
    fn a_file_with_no_frontmatter_or_no_closing_fence_is_refused() {
        assert!(matches!(
            parse_skill(&path(), "just text\n").expect_err("refused"),
            SkillError::NoFrontmatter { .. }
        ));
        assert!(matches!(
            parse_skill(&path(), "---\nname: n\n").expect_err("refused"),
            SkillError::NoFrontmatter { .. }
        ));
    }

    #[test]
    fn bad_yaml_frontmatter_is_refused_naming_the_file() {
        let err = parse_skill(&path(), "---\nname: [unclosed\n---\nb\n").expect_err("refused");
        assert!(matches!(err, SkillError::BadFrontmatter { .. }), "{err}");
    }

    #[test]
    fn mentioned_is_case_insensitive() {
        let s = parse_skill(&path(), ok_text()).expect("parses");
        assert!(mentioned(&s, "please do a CODE Review"));
        assert!(mentioned(&s, "open a pr"));
        assert!(mentioned(&s, "open a PR."));
    }

    #[test]
    fn mentioned_is_whole_word() {
        let s = parse_skill(&path(), ok_text()).expect("parses");
        assert!(!mentioned(&s, "sprinting"), "`pr` inside `sprinting`");
        assert!(!mentioned(&s, "prs are fine"), "`pr` inside `prs`");
        assert!(!mentioned(&s, "recode reviewer"), "spans need clean edges");
        assert!(mentioned(&s, "(pr)"), "punctuation is not a word char");
    }

    #[test]
    fn a_match_inside_a_word_does_not_hide_a_later_real_one() {
        let s = parse_skill(&path(), ok_text()).expect("parses");
        assert!(mentioned(&s, "sprint then pr"));
    }
}
