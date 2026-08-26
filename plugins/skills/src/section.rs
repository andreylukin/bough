//! Invariant: an UNMENTIONED SKILL CONTRIBUTES NOTHING — `render` returns `Ok(None)`, so the
//! section does not appear at all and costs no budget. The scan reads only rows VISIBLE AT
//! `SectionRequest::as_of`, so re-assembling a past request injects exactly the skills that
//! request injected.

use std::sync::Arc;

use bough_plugin_ledger::{Order, Step, StepQuery};
use bough_plugin_projection::{
    Place, Position, ProjectionError, SectionBody, SectionCites, SectionRender, SectionRequest,
    Slot,
};

use crate::parse::{mentioned, Skill};
use crate::registry::{admitted, Pool};

/// §9's auto-injection rides the tiers band, right after it: a skill is reference material, read
/// after what the agent already knows and before the verbatim tail.
pub const POSITION: Position = Position {
    slot: Slot::Tiers,
    place: Place::After,
};

/// One skill file's section.
pub struct SkillSection {
    pub skill: Arc<Skill>,
    pub pool: Arc<Pool>,
    pub scan_steps: usize,
    pub max_injected: usize,
}

/// PURE: the text one step contributes to the trigger scan.
///
/// The whole body, as JSON. A skill triggers on what the agent has been reading, and every step
/// type spells its text differently; scanning the serialized body needs no per-type vocabulary and
/// cannot silently miss a new one.
pub fn scan_text_of(step: &Step) -> String {
    step.body.to_string()
}

/// PURE: the scanned text and the steps it came from, oldest first.
pub fn scan(steps: &[Step]) -> (String, Vec<Step>) {
    let mut text = String::new();
    for s in steps {
        text.push_str(&scan_text_of(s));
        text.push('\n');
    }
    (text, steps.to_vec())
}

#[async_trait::async_trait]
impl SectionRender for SkillSection {
    async fn render(&self, req: &SectionRequest) -> Result<Option<SectionBody>, ProjectionError> {
        let traj = &req.connected.own;
        // The verbatim tail, at `as_of`. `before()` is exclusive, so `as_of` itself is included.
        let mut steps = req
            .ledger
            .0
            .steps(&StepQuery {
                trajs: vec![traj.clone()],
                before: req.before(),
                order: Order::SeqDesc,
                limit: Some(self.scan_steps),
                ..Default::default()
            })
            .await
            .map_err(ProjectionError::from)?;
        steps.reverse();
        // Unconsumed mail: what the agent has NOT read yet is exactly what a mention should fire
        // on. Filtered to `as_of` by hand — the store's query has no bound of its own.
        let mail = req
            .ledger
            .0
            .unconsumed_mail(traj)
            .await
            .map_err(ProjectionError::from)?;
        for m in mail {
            if req.visible(m.seq) && !steps.iter().any(|s| s.id == m.id) {
                steps.push(m);
            }
        }
        steps.sort_by_key(|s| s.seq);

        let (text, scanned) = scan(&steps);
        if !mentioned(&self.skill, &text) {
            return Ok(None);
        }
        // The cap is decided over the whole pool, so a skill that is mentioned but out-ranked
        // contributes nothing either.
        if !admitted(&self.pool.snapshot(), &text, self.max_injected).contains(&self.skill.id) {
            return Ok(None);
        }

        // Model-visible ⟺ ledgered: the section names the rows whose text triggered it.
        let steps: Vec<_> = scanned
            .iter()
            .filter(|s| mentioned(&self.skill, &scan_text_of(s)))
            .map(|s| s.id.clone())
            .collect();
        Ok(Some(SectionBody {
            title: format!("Skill: {}", self.skill.name),
            body: self.skill.body.trim_end().to_string(),
            cites: SectionCites {
                steps,
                rollups: vec![],
            },
        }))
    }
}
