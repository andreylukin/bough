//! Invariant: `max_injected` is decided over THE WHOLE POOL, not by whichever child fiber
//! rendered first. Each `skill` child registers itself in a per-pool registry keyed by the host's
//! directory, and every child's `render` asks the same pure function which ids are admitted. Ties
//! break by [`SectionId`] (P1-D8), never by load order.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bough_plugin_projection::SectionId;
use parking_lot::Mutex;

use crate::parse::{mentioned, Skill};

/// One pool of skills: everything mounted under one host directory.
#[derive(Default)]
pub struct Pool {
    skills: Mutex<BTreeMap<SectionId, Arc<Skill>>>,
}

impl Pool {
    /// Add one skill. The returned closure withdraws it, and it is the CHILD's disposer: a skill
    /// that unloads leaves the pool as if it had never mounted (§0.2).
    pub fn insert(self: &Arc<Self>, skill: Arc<Skill>) -> impl FnOnce() + Send + 'static {
        let id = skill.id.clone();
        self.skills.lock().insert(id.clone(), skill);
        let pool = Arc::clone(self);
        move || {
            pool.skills.lock().remove(&id);
        }
    }

    /// Every skill in the pool, ordered by [`SectionId`].
    pub fn snapshot(&self) -> Vec<Arc<Skill>> {
        self.skills.lock().values().cloned().collect()
    }
}

/// The process-wide map of pools. A pool is per HOST DIRECTORY, so two `skills` rows over two
/// directories cap independently, and a test's temp directory never sees another test's skills.
static POOLS: Mutex<BTreeMap<PathBuf, Arc<Pool>>> = Mutex::new(BTreeMap::new());

/// The pool for one host directory, created on first use.
pub fn pool(dir: &Path) -> Arc<Pool> {
    let mut pools = POOLS.lock();
    Arc::clone(
        pools
            .entry(dir.to_path_buf())
            .or_insert_with(|| Arc::new(Pool::default())),
    )
}

/// Every pool, keyed by host directory. The invariant runner's window into the registry.
pub fn all_pools() -> Vec<(PathBuf, Arc<Pool>)> {
    POOLS
        .lock()
        .iter()
        .map(|(k, v)| (k.clone(), Arc::clone(v)))
        .collect()
}

/// PURE: which of `all` inject into a request whose scanned text is `scanned`.
///
/// Mentioned skills only, at most `max` of them, chosen by [`SectionId`] order. Deterministic for
/// a given pool and a given scan, which is what makes the cap testable without a kernel.
pub fn admitted(all: &[Arc<Skill>], scanned: &str, max: usize) -> Vec<SectionId> {
    let mut ids: Vec<SectionId> = all
        .iter()
        .filter(|s| mentioned(s, scanned))
        .map(|s| s.id.clone())
        .collect();
    ids.sort();
    ids.truncate(max);
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str, trigger: &str) -> Arc<Skill> {
        Arc::new(Skill {
            id: SectionId::new(format!("skill:{name}")),
            name: name.into(),
            description: String::new(),
            triggers: vec![trigger.to_lowercase()],
            body: format!("body of {name}"),
        })
    }

    #[test]
    fn only_mentioned_skills_are_admitted() {
        let all = vec![skill("alpha", "alpha"), skill("beta", "beta")];
        assert_eq!(
            admitted(&all, "run alpha now", 3),
            vec![SectionId::new("skill:alpha")]
        );
        assert!(admitted(&all, "nothing here", 3).is_empty());
    }

    #[test]
    fn max_injected_caps_and_ties_break_by_section_id() {
        // Deliberately inserted in reverse id order: the cap must not follow insertion order.
        let all = vec![
            skill("delta", "go"),
            skill("charlie", "go"),
            skill("bravo", "go"),
            skill("alpha", "go"),
        ];
        assert_eq!(
            admitted(&all, "go", 2),
            vec![SectionId::new("skill:alpha"), SectionId::new("skill:bravo")]
        );
        assert_eq!(admitted(&all, "go", 0), Vec::<SectionId>::new());
        assert_eq!(admitted(&all, "go", 10).len(), 4);
    }

    #[test]
    fn a_disposed_skill_leaves_the_pool_as_it_was() {
        let p = Arc::new(Pool::default());
        let undo = p.insert(skill("alpha", "a"));
        assert_eq!(p.snapshot().len(), 1);
        undo();
        assert!(p.snapshot().is_empty());
    }
}
