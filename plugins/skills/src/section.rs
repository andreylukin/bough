//! Invariant: an UNMENTIONED SKILL CONTRIBUTES NOTHING — `render` returns `Ok(None)`, so the
//! section does not appear at all and costs no budget. The scan reads only rows VISIBLE AT
//! `SectionRequest::as_of`, so re-assembling a past request injects exactly the skills that
//! request injected.

use std::sync::Arc;

use bough_plugin_ledger::{Order, Step, StepQuery};
use bough_plugin_projection::{
    DropPriority, Place, Position, ProjectionError, SectionBody, SectionCites, SectionId,
    SectionRender, SectionRequest, SectionScope, SectionSpec, Slot,
};

use crate::parse::{mentioned, Skill};
use crate::registry::{admitted, Pool};

/// §9's auto-injection rides the tiers band, right after it: a skill is reference material, read
/// after what the agent already knows and before the verbatim tail.
pub const POSITION: Position = Position {
    slot: Slot::Tiers,
    place: Place::After,
};

/// The catalog's section id.
pub fn catalog_id() -> SectionId {
    SectionId::new("skills:catalog")
}

/// The catalog (drivability §5): every skill in the pool by name + description, so the model
/// CHOOSES what to load with the `skill` tool instead of waiting on a trigger word. Registered
/// once by the HOST; empty pool ⇒ no section.
pub fn catalog_spec(pool: Arc<Pool>) -> SectionSpec {
    SectionSpec {
        id: catalog_id(),
        position: POSITION,
        scope: SectionScope::Global,
        agent: None,
        priority: DropPriority::Coarse,
        render: Arc::new(CatalogSection { pool }),
    }
}

/// The catalog's renderer. Like the prompt files, the section cites nothing: skill files are not
/// ledgered, and the body deliberately does not vary with `as_of`.
pub struct CatalogSection {
    pub pool: Arc<Pool>,
}

/// PURE: the catalog body for a pool snapshot. `None` for an empty pool.
pub fn catalog_body(skills: &[Arc<Skill>]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }
    let mut body = String::from(
        "Loaded on demand: call the `skill` tool with a name to read one BEFORE doing the task \
         it covers.\n",
    );
    for s in skills {
        body.push_str("- ");
        body.push_str(&s.name);
        if !s.description.is_empty() {
            body.push_str(" \u{2014} ");
            body.push_str(&s.description);
        }
        body.push('\n');
    }
    Some(body)
}

#[async_trait::async_trait]
impl SectionRender for CatalogSection {
    async fn render(&self, _req: &SectionRequest) -> Result<Option<SectionBody>, ProjectionError> {
        Ok(catalog_body(&self.pool.snapshot()).map(|body| SectionBody {
            title: "Skills".to_string(),
            body,
            cites: SectionCites::default(),
        }))
    }
}

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
