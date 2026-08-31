//! Invariant: the ONE list of every step type a plugin in this binary can write.
//!
//! A `LedgerStore` refuses to READ a step type it was not told about (`UnknownStepTypeOnRead`,
//! §3), so a reader OUTSIDE the kernel — a crash test that opens the ledger a killed process left
//! behind, a tool that dumps a chain — has to declare the tree's vocabulary before it can read
//! one. Track C's hardening tests hand-listed thirteen crates and the list went stale the moment
//! the code-mode merge added a fourteenth: `docs/track-c-merge-notes.md` asks for exactly this
//! helper, "or one exported by the launcher". The launcher is where it belongs, because the
//! launcher is the crate that links every plugin.
//!
//! It is NOT how the running tree declares its vocabulary. A row does that itself, through
//! `LedgerHandle::declare_step_types`, for the life of the binary (AGENTS.md). This is the list a
//! reader with no kernel needs, and `every_plugin_that_writes_steps_is_in_the_list` below is what
//! keeps it honest.

use bough_plugin_ledger::StepTypeDef;

/// Every step type declared by a plugin in this binary, in no particular order.
///
/// The ledger's own builtins are NOT here: a store has them from `StepTypeMap::with_builtins`.
/// Registering a definition twice is a reference, not an error, so a caller may hand the whole
/// list to a store that already knows some of them.
pub fn all() -> Vec<StepTypeDef> {
    let mut out = Vec::new();
    out.extend(bough_plugin_about_line::step_types());
    out.extend(bough_plugin_agents::vocabulary::step_types());
    out.extend(bough_plugin_dormancy::vocabulary::step_types());
    out.extend(bough_plugin_drafts::step_types());
    out.extend(bough_plugin_drift_watch::vocabulary::step_types());
    out.extend(bough_plugin_graph_ops::vocabulary::step_types());
    out.extend(bough_plugin_hooks_exec::vocabulary::step_types());
    out.extend(bough_plugin_leader::vocabulary::step_types());
    out.extend(bough_plugin_llm::usage::step_types());
    out.extend(bough_plugin_mail_router::vocabulary::step_types());
    out.extend(bough_plugin_reconsolidation::vocabulary::step_types());
    out.extend(bough_plugin_rollups_summarizer::step_types());
    out.extend(bough_plugin_tools::vocabulary::step_types());
    out.extend(bough_plugin_tools_codemode::vocabulary::step_types());
    out.extend(bough_plugin_tools_operator::schedule::step_types());
    out.extend(bough_plugin_wards_rhai::vocabulary::step_types());
    out.extend(bough_plugin_worker_fork::vocabulary::step_types());
    out.extend(bough_plugin_workers::vocabulary::step_types());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The list goes stale silently, and a stale list reads a chain as EMPTY rather than as an
    /// error — which is how four crash-reconciliation cases went red at the merge. So it is
    /// checked against the TREE: every crate under `plugins/` that exports a `step_types()` must
    /// be in the list above.
    #[test]
    fn every_plugin_that_writes_steps_is_in_the_list() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let src = std::fs::read_to_string(root.join("crates/bough/src/vocabulary.rs"))
            .expect("this file");
        let mut missing = Vec::new();
        for e in std::fs::read_dir(root.join("plugins"))
            .expect("plugins/")
            .flatten()
        {
            let dir = e.path();
            if !dir.is_dir() {
                continue;
            }
            let name = dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            let mut exports = false;
            let mut stack = vec![dir.join("src")];
            while let Some(d) = stack.pop() {
                let Ok(entries) = std::fs::read_dir(&d) else {
                    continue;
                };
                for f in entries.flatten() {
                    let p = f.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else if p.extension().and_then(|x| x.to_str()) == Some("rs")
                        && std::fs::read_to_string(&p)
                            .unwrap_or_default()
                            .contains("pub fn step_types()")
                    {
                        exports = true;
                    }
                }
            }
            let krate = format!("bough_plugin_{}", name.replace('-', "_"));
            if exports && !src.contains(&krate) {
                missing.push(krate);
            }
        }
        missing.sort();
        assert!(
            missing.is_empty(),
            "these crates export a `step_types()` this list does not carry, so a reader outside \
             the kernel cannot read what they write:\n{}",
            missing.join("\n")
        );
    }

    /// Two crates declaring the same type is legal (a byte-identical redeclaration is a
    /// reference), but a NAME collision between two DIFFERENT definitions is a bug that only
    /// shows up when both rows mount. Registering the whole list into one map finds it here.
    #[test]
    fn the_whole_list_registers_into_one_map() {
        let map = bough_plugin_ledger::StepTypeMap::with_builtins();
        for def in all() {
            let name = def.name.clone();
            map.register(def)
                .unwrap_or_else(|e| panic!("`{name}` clashes with a standing definition: {e}"))
                .forget();
        }
    }
}
