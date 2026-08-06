//! SKILL.md discovery (port of `src/skills/`): walk bundled + user skills
//! dirs; a broken listed skill is never omitted. STUB (wave 2, row 2.18) —
//! the honest v1 answer is the empty list.

/// One discovered skill.
#[derive(Clone, Debug, PartialEq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: String,
}

/// Discover skills. Stub: none discovered yet.
pub fn discover_skills() -> Vec<Skill> {
    Vec::new()
}
