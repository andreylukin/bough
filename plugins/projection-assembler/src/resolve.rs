//! Invariant: every request-time default lives HERE, in an explicit `resolve(request) -> Spec`
//! step (§0.2: "Defaulting is an explicit `resolve(request) -> Spec` step in the owning provider,
//! never a hidden `?? default` inside `run()`"). `assemble` and `write_file_view` read a resolved
//! `Spec` and never a `req.x.unwrap_or(cfg.x)`.

use std::path::{Path, PathBuf};

use bough_plugin_projection::{
    AssembleRequest, DropPriority, FileViewRequest, ProjectionError, SectionId,
};

use crate::AssemblerConfig;

/// What one [`AssembleRequest`] resolves to against the row's config.
#[derive(Clone, Debug, PartialEq)]
pub struct AssembleSpec {
    /// The token budget before headroom.
    pub budget: usize,
    /// The drop priority of a section nobody declared one for — a section the
    /// `projection/assemble` waterfall appended. Budgeted, and dropped no earlier than rung 3.
    pub default_priority: DropPriority,
}

/// Resolve an assemble request. Pure, and the ONE place `budget` may fall back to the config.
pub fn resolve_assemble(req: &AssembleRequest, cfg: &AssemblerConfig) -> AssembleSpec {
    AssembleSpec {
        budget: req.budget.unwrap_or(cfg.budget_tokens),
        default_priority: DropPriority::Coarse,
    }
}

/// What one [`FileViewRequest`] resolves to: a directory and a file NAME, never a caller-shaped
/// path.
#[derive(Clone, Debug, PartialEq)]
pub struct FileViewSpec {
    pub dir: PathBuf,
    pub file_name: String,
}

impl FileViewSpec {
    pub fn path(&self) -> PathBuf {
        self.dir.join(&self.file_name)
    }
}

/// Resolve a file-view request against the row's config.
///
/// A [`bough_plugin_ledger::TrajId`] is an unvalidated branded string and the codebase's own
/// fixtures use slash-bearing ids (`lane/sol`), so the id is SANITISED into a single file name
/// rather than joined as a path: a `/` would land the write in a directory that does not exist, a
/// leading `/` would discard `dir` entirely, and `..` would escape it.
pub fn resolve_file_view(
    req: &FileViewRequest,
    cfg: &AssemblerConfig,
    dir: Option<&Path>,
) -> Result<FileViewSpec, ProjectionError> {
    let dir = dir.unwrap_or(&cfg.file_view_dir).to_path_buf();
    let stem = sanitize(req.traj.as_str());
    if stem.is_empty() {
        return Err(ProjectionError::FileView {
            path: dir.display().to_string(),
            detail: format!(
                "trajectory `{}` has no character that can name a file",
                req.traj
            ),
        });
    }
    Ok(FileViewSpec {
        dir,
        file_name: format!("{stem}.txt"),
    })
}

/// Every character outside `[A-Za-z0-9._-]` becomes `_`, and a name that is only dots is refused
/// by the caller. Deterministic: the same traj id always names the same file.
fn sanitize(traj: &str) -> String {
    let mapped: String = traj
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if mapped.chars().all(|c| c == '.') {
        return String::new();
    }
    mapped
}

/// The six built-in band ids, which a contributed section may not claim.
pub fn is_reserved_section_id(id: &SectionId) -> bool {
    crate::degrade::is_builtin(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::TrajId;

    fn cfg() -> AssemblerConfig {
        AssemblerConfig {
            budget_tokens: 1000,
            headroom: 0.6,
            tail_steps: 10,
            tail_floor_steps: 2,
            mail_newest_n: 3,
            max_tiers: 3,
            file_view_dir: PathBuf::from("/views"),
        }
    }

    fn req(traj: &str) -> FileViewRequest {
        FileViewRequest {
            traj: TrajId::new(traj),
            at: chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .into(),
        }
    }

    #[test]
    fn a_flat_traj_id_names_itself() {
        let spec = resolve_file_view(&req("t-sol"), &cfg(), None).expect("resolves");
        assert_eq!(spec.path(), PathBuf::from("/views/t-sol.txt"));
    }

    #[test]
    fn a_slash_bearing_traj_id_stays_inside_the_view_dir() {
        for traj in ["lane/sol", "/etc/passwd", "../../escape", "..", "a\0b"] {
            match resolve_file_view(&req(traj), &cfg(), None) {
                Ok(spec) => {
                    let path = spec.path();
                    assert_eq!(
                        path.parent(),
                        Some(Path::new("/views")),
                        "`{traj}` escaped the view dir: {}",
                        path.display()
                    );
                    assert!(!spec.file_name.contains('/'), "`{traj}` kept a separator");
                }
                // Refusing is the other acceptable answer; writing outside the dir is not.
                Err(ProjectionError::FileView { .. }) => {}
                Err(e) => panic!("unexpected refusal for `{traj}`: {e}"),
            }
        }
    }

    #[test]
    fn a_request_budget_wins_over_the_config() {
        let c = cfg();
        let base = AssembleRequest {
            agent: bough_plugin_ledger::AgentName::new("a1"),
            wake: None,
            at: chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .into(),
            budget: None,
        };
        assert_eq!(resolve_assemble(&base, &c).budget, 1000);
        let asked = AssembleRequest {
            budget: Some(42),
            ..base
        };
        assert_eq!(resolve_assemble(&asked, &c).budget, 42);
    }
}
