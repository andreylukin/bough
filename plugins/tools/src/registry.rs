//! Invariant (§5, §9): `tools.restrict` is an INTERSECTION filter over the global set, never a
//! way to add a tool, and an agent-scoped tool shadows its same-named global twin for that agent
//! alone. `schemas()` is the single source of truth for what the prompt shows.

use std::collections::BTreeSet;

use bough_plugin_llm::ToolName;

/// One restriction. Registered in an agent's scope, so it unwinds with the agent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Restrict {
    /// `None` ⇒ everything the deny list admits. `Some` ⇒ only these.
    pub allow: Option<BTreeSet<ToolName>>,
    pub deny: BTreeSet<ToolName>,
}

impl Restrict {
    /// The composition rule (§5): two restrictions compose as an INTERSECTION, so a second one
    /// can only narrow.
    pub fn intersect(&self, other: &Restrict) -> Restrict {
        let allow = match (&self.allow, &other.allow) {
            (None, None) => None,
            (Some(a), None) => Some(a.clone()),
            (None, Some(b)) => Some(b.clone()),
            (Some(a), Some(b)) => Some(a.intersection(b).cloned().collect()),
        };
        let deny: BTreeSet<ToolName> = self.deny.union(&other.deny).cloned().collect();
        Restrict { allow, deny }
    }

    /// Whether `name` survives this restriction. The deny list wins: a name in both lists is
    /// denied, because composition may only narrow.
    pub fn admits(&self, name: &ToolName) -> bool {
        if self.deny.contains(name) {
            return false;
        }
        match &self.allow {
            None => true,
            Some(allow) => allow.contains(name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(s: &str) -> ToolName {
        ToolName::new(s)
    }
    fn set(names: &[&str]) -> BTreeSet<ToolName> {
        names.iter().map(|s| n(s)).collect()
    }

    #[test]
    fn intersect_narrows_the_allow_list_and_unions_the_denies() {
        let a = Restrict {
            allow: Some(set(&["bash", "read_file", "grep"])),
            deny: set(&["bash"]),
        };
        let b = Restrict {
            allow: Some(set(&["read_file", "grep", "glob"])),
            deny: set(&["grep"]),
        };
        let c = a.intersect(&b);
        assert_eq!(c.allow, Some(set(&["read_file", "grep"])));
        assert_eq!(c.deny, set(&["bash", "grep"]));
        assert!(c.admits(&n("read_file")));
        assert!(!c.admits(&n("grep")), "a denial cannot be re-admitted");
        assert!(!c.admits(&n("glob")), "outside the intersected allow list");
    }

    #[test]
    fn an_open_restriction_never_widens_a_closed_one() {
        let closed = Restrict {
            allow: Some(set(&["read_file"])),
            deny: BTreeSet::new(),
        };
        let open = Restrict::default();
        assert_eq!(closed.intersect(&open).allow, Some(set(&["read_file"])));
        assert_eq!(open.intersect(&closed).allow, Some(set(&["read_file"])));
    }
}
