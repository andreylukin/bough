//! §0.2 runtime invariant for `bough-plugin-skills`:
//!
//! **Every assembled projection contains a skill's section if and only if that request mentioned
//! one of its triggers**, and at most `max_injected` skill sections appear, chosen by `SectionId`
//! order and never by load order.
//!
//! The "if and only if" half is a property of one function — [`crate::registry::admitted`] is what
//! every skill section consults — so it is checked HERE as a relation over the live pool: an
//! assembly can only inject what the pool holds, and the pool must not hold two skills under one
//! `SectionId` (the cap would then depend on which child rendered) nor a skill with no trigger
//! (which could never fire, and §0.2 makes that a misconfiguration rather than a quiet no-op).

use std::sync::Arc;

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};

use crate::parse::Skill;

const NAME: &str = "skill_pools_are_injectable_and_uniquely_identified";

/// PURE: the whole check, over one pool's snapshot.
pub fn evaluate(dir: &str, skills: &[Arc<Skill>]) -> Result<(), String> {
    let mut seen: Vec<&str> = Vec::new();
    for s in skills {
        if s.triggers.is_empty() {
            return Err(format!(
                "skill pool `{dir}`: `{}` has no trigger and could never inject",
                s.name
            ));
        }
        if seen.contains(&s.id.as_str()) {
            return Err(format!(
                "skill pool `{dir}`: two skills share the section id `{}`; the `max_injected` cap \
                 would then depend on which child rendered first",
                s.id
            ));
        }
        seen.push(s.id.as_str());
    }
    Ok(())
}

/// The spec [`crate::SkillsHostPlugin::invariants`] returns.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: NAME,
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }]
}

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    for (dir, pool) in crate::registry::all_pools() {
        evaluate(&dir.display().to_string(), &pool.snapshot()).map_err(|detail| {
            InvariantViolation {
                invariant: NAME,
                plugin: crate::PLUGIN_NAME,
                entry: ctx.entry_id().clone(),
                detail,
            }
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_projection::SectionId;

    fn skill(name: &str, triggers: &[&str]) -> Arc<Skill> {
        Arc::new(Skill {
            id: SectionId::new(format!("skill:{name}")),
            name: name.into(),
            description: String::new(),
            triggers: triggers.iter().map(|t| t.to_string()).collect(),
            body: String::new(),
        })
    }

    #[test]
    fn a_healthy_pool_passes() {
        assert!(evaluate("/skills", &[skill("a", &["a"]), skill("b", &["b"])]).is_ok());
    }

    #[test]
    fn a_triggerless_skill_in_the_pool_is_a_violation() {
        let err = evaluate("/skills", &[skill("a", &[])]).expect_err("violation");
        assert!(err.contains("could never inject"), "{err}");
    }

    #[test]
    fn two_skills_under_one_section_id_is_a_violation() {
        let err =
            evaluate("/skills", &[skill("a", &["x"]), skill("a", &["y"])]).expect_err("violation");
        assert!(err.contains("share the section id"), "{err}");
    }
}
