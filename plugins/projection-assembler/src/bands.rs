//! Invariant: the six built-in bands render from the ledger and NOTHING ELSE, in `Slot` order, and
//! each works with ZERO rollups — Phase 4 produces tiers and digests, so a band with no input
//! renders nothing at all rather than an empty header. The tail is de-interleaved by `wake_id`
//! (§3): the window is selected by seq, then grouped by wake, wakes ordered by their first
//! selected seq, seq order preserved inside a wake — a pure function of the rows.

use std::collections::BTreeSet;

use bough_plugin_ledger::{
    AgentRow, Class, Pin, Ref, Rollup, RollupKind, RollupQuery, Step, StepType,
};
use bough_plugin_projection::{
    tokens, Position, ProjectionError, RenderedSection, SectionCites, SectionId, SectionRequest,
    Slot,
};

use crate::AssemblerConfig;

/// `mail/delivered`, the one step type the mail band reads.
pub const MAIL_DELIVERED: &str = "mail/delivered";

/// Assemble one rendered section. `tokens` is measured on exactly the text `to_text` will print,
/// so the budget and the golden can never disagree.
pub(crate) fn section(
    id: &str,
    slot: Slot,
    title: &str,
    body: String,
    cites: SectionCites,
) -> RenderedSection {
    let tokens = tokens::count(&format!("## {title}\n\n{body}\n"));
    RenderedSection {
        id: SectionId::new(id),
        // The band's OWN place, not `Before`: a contributed `Place::Before` section must sort
        // ahead of the band and a `Place::After` one behind it, and only a place of its own can
        // make both true (`Place::Band` sits between them).
        position: Position::band(slot),
        title: title.to_string(),
        body,
        cites,
        tokens,
        degraded: None,
    }
}

/// Re-measure a section after a rung edited its body.
pub(crate) fn remeasure(s: &mut RenderedSection) {
    s.tokens = tokens::count(&format!("## {}\n\n{}\n", s.title, s.body));
}

// ---- identity ---------------------------------------------------------------------------------

/// **Identity** — the `agents` row plus the digest pointer. The about-line's state half arrives in
/// Phase 2 as a contributed section at `Position { Identity, After }` (P1-D12).
pub async fn identity(
    req: &SectionRequest,
    _cfg: &AssemblerConfig,
) -> Result<Option<RenderedSection>, ProjectionError> {
    let row = req.ledger.0.agent(&req.agent).await?;
    Ok(Some(identity_section(&req.agent, row.as_ref())))
}

/// Pure: the identity band as a function of the (mutable) agents row alone.
pub fn identity_section(
    name: &bough_plugin_ledger::AgentName,
    row: Option<&AgentRow>,
) -> RenderedSection {
    let mut body = format!("name: {name}\n");
    let mut cites = SectionCites::default();
    match row {
        Some(r) => {
            body.push_str(&format!("trajectory: {}\n", r.traj));
            body.push_str(&format!("routing refs: {}\n", join(&r.routing_refs)));
            body.push_str(&format!(
                "wake classes: {}\n",
                if r.wake_classes.is_empty() {
                    "-".to_string()
                } else {
                    r.wake_classes
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ));
            if let Some(m) = &r.model_override {
                body.push_str(&format!("model override: {m}\n"));
            }
            match &r.digest_rollup {
                Some(d) => {
                    body.push_str(&format!("digest: {d}\n"));
                    cites.rollups.push(d.clone());
                }
                None => body.push_str("digest: none\n"),
            }
        }
        // An agent with no row is still an agent: identity is never dropped, so it never refuses.
        None => body.push_str("trajectory: -\nrouting refs: -\nwake classes: -\ndigest: none\n"),
    }
    section("identity", Slot::Identity, "Identity", body, cites)
}

fn join(refs: &BTreeSet<Ref>) -> String {
    if refs.is_empty() {
        return "-".to_string();
    }
    refs.iter()
        .map(|r| r.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

// ---- pins -------------------------------------------------------------------------------------

/// DELIBERATELY does not honour expiry (§3, V7): a pin's only relief valve is supersession, which
/// `live_pins` already implements, so an expiry marker naming a pin is IGNORED here.
///
/// **Pins** — `live_pins(connected)`, verbatim, oldest first, each with its step id. Never
/// filtered by age, never demoted (§5). Selected inline by `assemble`, which needs the
/// `Vec<Pin>` itself for the degradation ladder; the two halves are [`sort_pins`] and
/// [`pins_section`], so there is only ever ONE copy of the selection.
/// Oldest first, and deterministic when two trajectories share a seq.
pub fn sort_pins(pins: &mut [Pin]) {
    pins.sort_by(|a, b| (a.seq, a.traj.as_str()).cmp(&(b.seq, b.traj.as_str())));
}

/// Pure: pins render VERBATIM. `None` when there are none — no empty header.
pub fn pins_section(pins: &[Pin]) -> Option<RenderedSection> {
    if pins.is_empty() {
        return None;
    }
    let mut body = String::new();
    let mut cites = SectionCites::default();
    for p in pins {
        body.push_str(&format!("- {} (step:{})\n", p.title, p.step));
        for line in p.text.lines() {
            body.push_str(&format!("  {line}\n"));
        }
        cites.steps.push(p.step.clone());
    }
    Some(section("pins", Slot::Pins, "Pins", body, cites))
}

/// Rung 4's collapse: titles + count, and nothing else.
pub fn pins_collapsed_body(pins: &[Pin]) -> String {
    let mut body = format!("{} pins, collapsed to titles:\n", pins.len());
    for p in pins {
        body.push_str(&format!("- {}\n", p.title));
    }
    body
}

// ---- digest -----------------------------------------------------------------------------------

/// **Digest** — the agent's `digest_rollup`, if any. With zero rollups: nothing, and no header.
pub async fn digest(
    req: &SectionRequest,
    _cfg: &AssemblerConfig,
) -> Result<Option<RenderedSection>, ProjectionError> {
    let Some(row) = req.ledger.0.agent(&req.agent).await? else {
        return Ok(None);
    };
    let Some(id) = row.digest_rollup.clone() else {
        return Ok(None);
    };
    let all = req
        .ledger
        .0
        .rollups(&RollupQuery {
            trajs: req.connected.trajectories().into_iter().collect(),
            kind: Some(RollupKind::Digest),
            ..Default::default()
        })
        .await?;
    let Some(r) = all
        .into_iter()
        .filter(|r| req.visible(r.to_seq))
        .find(|r| r.id == id)
    else {
        // Phase 4 produces digests. A dangling pointer renders nothing rather than a lie.
        return Ok(None);
    };
    // §8: an expired digest renders NOTHING. The pointer is `agents` (mutable config) and the
    // marker is a step; the marker wins, and the row itself is never edited.
    if crate::expiry::load(req).await?.rollups.contains(&r.id) {
        return Ok(None);
    }
    Ok(Some(digest_section(&r)))
}

/// Pure: the digest band over one rollup.
pub fn digest_section(r: &Rollup) -> RenderedSection {
    let mut cites = SectionCites::default();
    cites.rollups.push(r.id.clone());
    section("digest", Slot::Digest, "Digest", rollup_text(r), cites)
}

/// Rung 6's truncation: the first paragraph of the body, and a marker that says so.
pub fn first_paragraph(body: &str) -> String {
    let head = body.split("\n\n").next().unwrap_or("").trim_end();
    format!("{head}\n(truncated to its first paragraph)\n")
}

/// A rollup's text. A rollup body is JSON; a string is itself, an object's `text` or `summary`
/// field wins, anything else prints as compact JSON so the render is total and deterministic.
pub fn rollup_text(r: &Rollup) -> String {
    let text = match &r.body {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(o) => o
            .get("text")
            .or_else(|| o.get("summary"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| serde_json::to_string(&r.body).unwrap_or_default()),
        other => serde_json::to_string(other).unwrap_or_default(),
    };
    if text.ends_with('\n') {
        text
    } else {
        format!("{text}\n")
    }
}

// ---- tiers ------------------------------------------------------------------------------------

/// **Tiers** — kind `tier`, COARSE TO FINE, tier ≤ `max_tiers`, kept when
/// `notable_refs ∩ agent.refs ≠ ∅` **or** `notable_refs` is empty (P1-D13).
pub async fn tiers(
    req: &SectionRequest,
    cfg: &AssemblerConfig,
) -> Result<Vec<RenderedSection>, ProjectionError> {
    let rollups = req
        .ledger
        .0
        .rollups(&RollupQuery {
            trajs: req.connected.trajectories().into_iter().collect(),
            kind: Some(RollupKind::Tier),
            max_tier: Some(cfg.max_tiers),
            ..Default::default()
        })
        .await?;
    // §2.7 item 3: a rollup covering rows above `as_of` did not exist for the request being
    // reproduced.
    // §8: a block named by an appended expiry marker leaves the band. (A SUPERSEDED block is
    // already gone: `RollupQuery::include_superseded` defaults to false.)
    let expired = crate::expiry::load(req).await?;
    let rollups: Vec<Rollup> = rollups
        .into_iter()
        .filter(|r| req.visible(r.to_seq))
        .filter(|r| !expired.rollups.contains(&r.id))
        .collect();
    Ok(tier_sections(&rollups, &req.connected.refs, cfg.max_tiers))
}

/// The `SectionId` of a tier band. Higher tiers sort FIRST, because the section order breaks ties
/// by id and §5 wants tiers coarse to fine.
pub fn tier_section_id(tier: u8) -> String {
    format!("tier-{:03}", u8::MAX - tier)
}

/// Pure: filter, group and order the tier bands. Zero rollups ⇒ zero sections, no empty header.
pub fn tier_sections(
    rollups: &[Rollup],
    agent_refs: &BTreeSet<Ref>,
    max_tier: u8,
) -> Vec<RenderedSection> {
    let mut kept: Vec<&Rollup> = rollups
        .iter()
        .filter(|r| r.kind == RollupKind::Tier && r.tier <= max_tier)
        .filter(|r| r.notable_refs.is_empty() || !r.notable_refs.is_disjoint(agent_refs))
        .collect();
    // Coarse to fine, then oldest range first inside a tier.
    kept.sort_by(|a, b| {
        (b.tier, a.from_seq, a.id.as_str()).cmp(&(a.tier, b.from_seq, b.id.as_str()))
    });

    let mut out: Vec<RenderedSection> = Vec::new();
    let mut tier_now: Option<u8> = None;
    for r in kept {
        if tier_now != Some(r.tier) {
            tier_now = Some(r.tier);
            out.push(section(
                &tier_section_id(r.tier),
                Slot::Tiers,
                &format!("Tier {} summary", r.tier),
                String::new(),
                SectionCites::default(),
            ));
        }
        let s = out.last_mut().expect("a section was just pushed");
        s.body.push_str(&format!(
            "- [{}..{}] {}",
            r.from_seq.0,
            r.to_seq.0,
            rollup_text(r)
        ));
        s.cites.rollups.push(r.id.clone());
    }
    for s in out.iter_mut() {
        remeasure(s);
    }
    out
}

/// The tier a tier section renders, read back off its id. `None` for anything else.
pub fn tier_of(id: &SectionId) -> Option<u8> {
    id.as_str()
        .strip_prefix("tier-")
        .and_then(|n| n.parse::<u8>().ok())
        .map(|inverted| u8::MAX - inverted)
}

// ---- tail -------------------------------------------------------------------------------------

/// **Tail** — the newest `tail_steps` steps of the agent's own chain, verbatim, oldest first,
/// de-interleaved by `wake_id`.
pub async fn tail(
    req: &SectionRequest,
    cfg: &AssemblerConfig,
) -> Result<(Option<RenderedSection>, Vec<Step>), ProjectionError> {
    // §2.7 item 3: with `as_of` the window is the newest `tail_steps` rows AT OR BELOW it, not
    // the newest rows overall — a post-filter would silently shrink the tail instead.
    let steps = match req.before() {
        None => {
            req.ledger
                .0
                .tail(&req.connected.own, cfg.tail_steps)
                .await?
        }
        Some(before) => {
            let mut window = req
                .ledger
                .0
                .steps(&bough_plugin_ledger::StepQuery {
                    trajs: vec![req.connected.own.clone()],
                    before: Some(before),
                    order: bough_plugin_ledger::Order::SeqDesc,
                    limit: Some(cfg.tail_steps),
                    ..Default::default()
                })
                .await?;
            window.reverse();
            window
        }
    };
    // §8: an expired step leaves the verbatim tail. The floor rung 2 shrinks toward therefore
    // counts SURVIVING steps: `steps` is what the ladder is handed, and the expired rows are gone
    // from it before it ever gets there.
    let expired = crate::expiry::load(req).await?;
    let steps: Vec<Step> = steps
        .into_iter()
        .filter(|s| !expired.steps.contains(&s.id))
        .collect();
    Ok((tail_section(&steps), steps))
}

/// Pure: the tail band over an already-selected window. `None` for an empty window.
pub fn tail_section(steps: &[Step]) -> Option<RenderedSection> {
    if steps.is_empty() {
        return None;
    }
    let mut body = String::new();
    let mut cites = SectionCites::default();
    for block in de_interleave(steps) {
        let wake = block[0].wake.clone();
        body.push_str(&format!("### wake {wake}\n"));
        for s in &block {
            body.push_str(&step_line(s));
            cites.steps.push(s.id.clone());
        }
    }
    Some(section("tail", Slot::Tail, "Recent steps", body, cites))
}

/// One verbatim step line. Compact JSON, whose object keys `serde_json` orders for us.
pub fn step_line(s: &Step) -> String {
    let class = match s.class {
        Class::Evidence => "evidence",
        Class::Thought => "thought",
    };
    format!(
        "- #{} {} [{}] {}\n",
        s.seq.0,
        s.kind,
        class,
        serde_json::to_string(&*s.body).unwrap_or_default()
    )
}

/// Group a selected window into wake blocks: wakes ordered by their first selected seq, seq order
/// preserved inside a wake. Pure — no clock, no arrival order.
pub fn de_interleave(steps: &[Step]) -> Vec<Vec<Step>> {
    let mut order: Vec<bough_plugin_ledger::WakeId> = Vec::new();
    let mut blocks: Vec<Vec<Step>> = Vec::new();
    let mut sorted: Vec<Step> = steps.to_vec();
    sorted.sort_by(|a, b| (a.seq, a.traj.as_str()).cmp(&(b.seq, b.traj.as_str())));
    for s in sorted {
        match order.iter().position(|w| *w == s.wake) {
            Some(i) => blocks[i].push(s),
            None => {
                order.push(s.wake.clone());
                blocks.push(vec![s]);
            }
        }
    }
    blocks
}

// ---- mail -------------------------------------------------------------------------------------

/// DELIBERATELY does not honour expiry (§5): unconsumed mail has its own consumption mechanism —
/// the union of the `wake/end` sets — and a marker must never silently un-deliver a message.
///
/// **Mail** — `unconsumed_mail`, newest first, grouped by class.
pub async fn mail(
    req: &SectionRequest,
    _cfg: &AssemblerConfig,
) -> Result<(Option<RenderedSection>, Vec<Step>), ProjectionError> {
    let steps: Vec<Step> = req
        .ledger
        .0
        .unconsumed_mail(&req.connected.own)
        .await?
        .into_iter()
        // §2.7 item 3. DEVIATION worth naming: consumption is read as it stands NOW, because
        // `wake/end.consumed` is not seq-addressable per piece. Mail delivered after `as_of` is
        // invisible; mail delivered before it and consumed since stays invisible too.
        .filter(|s| req.visible(s.seq))
        .collect();
    Ok((mail_section(&steps), steps))
}

/// The mail class of one `mail/delivered` step: `wake` or `ordinary` (§3's vocabulary).
pub fn mail_class(s: &Step) -> String {
    s.body
        .get("class")
        .and_then(|v| v.as_str())
        .unwrap_or("ordinary")
        .to_string()
}

/// Group mail into `(class, newest-first steps)`, classes in a fixed alphabetical order.
pub fn mail_groups(steps: &[Step]) -> Vec<(String, Vec<Step>)> {
    let mut by: std::collections::BTreeMap<String, Vec<Step>> = Default::default();
    for s in steps {
        by.entry(mail_class(s)).or_default().push(s.clone());
    }
    by.into_iter()
        .map(|(k, mut v)| {
            v.sort_by(|a, b| (b.seq, a.traj.as_str()).cmp(&(a.seq, b.traj.as_str())));
            (k, v)
        })
        .collect()
}

/// Pure: the mail band. `None` when nothing is unconsumed — no empty header.
pub fn mail_section(steps: &[Step]) -> Option<RenderedSection> {
    if steps.is_empty() {
        return None;
    }
    let mut body = String::new();
    let mut cites = SectionCites::default();
    for (class, group) in mail_groups(steps) {
        body.push_str(&format!("### {class}\n"));
        for s in &group {
            body.push_str(&mail_line(s));
            cites.steps.push(s.id.clone());
        }
    }
    Some(section("mail", Slot::Mail, "Unconsumed mail", body, cites))
}

fn field(s: &Step, name: &str) -> String {
    s.body
        .get(name)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// One mail header line.
pub fn mail_line(s: &Step) -> String {
    format!(
        "- from {}: {} — {} (step:{})\n",
        field(s, "from"),
        field(s, "subject"),
        field(s, "summary"),
        s.id
    )
}

/// Rung 5's collapse: per-class counts plus the newest N of each class, and nothing else.
pub fn mail_collapsed_body(steps: &[Step], newest_n: usize) -> String {
    let mut body = String::new();
    for (class, group) in mail_groups(steps) {
        body.push_str(&format!("### {class}: {} unconsumed\n", group.len()));
        for s in group.iter().take(newest_n) {
            body.push_str(&mail_line(s));
        }
        if group.len() > newest_n {
            body.push_str(&format!(
                "- … {} older, collapsed\n",
                group.len() - newest_n
            ));
        }
    }
    body
}

/// The step type a mail header carries.
pub fn mail_step_type() -> StepType {
    StepType::new(MAIL_DELIVERED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    // ---- pins ---------------------------------------------------------------------------------

    #[test]
    fn pins_ride_every_projection_verbatim() {
        let p = pin(
            "p1",
            3,
            "keep the tree green",
            "run `make gates` before every commit",
        );
        let s = pins_section(&[p]).expect("a pin renders");
        assert!(
            s.body.contains("run `make gates` before every commit"),
            "the pin text is verbatim, not summarised: {}",
            s.body
        );
        assert_eq!(s.cites.steps.len(), 1, "the band cites the pin's step");
    }

    #[test]
    fn a_pin_older_than_the_tail_still_renders() {
        // The tail window starts at seq 50; the pin is at seq 1. Age is never a criterion (§3).
        let window: Vec<Step> = (50..=52).map(|n| step(&format!("s{n}"), n, "w1")).collect();
        let tail = tail_section(&window).unwrap();
        assert!(
            !tail.body.contains("#1 "),
            "seq 1 is outside the tail window"
        );
        let s = pins_section(&[pin("p1", 1, "old but standing", "still true")]).unwrap();
        assert!(s.body.contains("old but standing"));
    }

    // ---- tiers --------------------------------------------------------------------------------

    #[test]
    fn tiers_are_coarse_to_fine() {
        let rollups = vec![
            tier_rollup("fine", 1, 1, 10, &[]),
            tier_rollup("coarse", 3, 1, 90, &[]),
            tier_rollup("mid", 2, 1, 30, &[]),
        ];
        let out = tier_sections(&rollups, &BTreeSet::new(), 3);
        let tiers: Vec<u8> = out.iter().map(|s| tier_of(&s.id).unwrap()).collect();
        assert_eq!(tiers, vec![3, 2, 1], "highest tier first");
        let ids: Vec<String> = out.iter().map(|s| s.id.to_string()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted, "the id order IS the coarse-to-fine order");
    }

    #[test]
    fn a_tier_whose_notable_refs_miss_the_agent_is_filtered_out() {
        let mine = BTreeSet::from([Ref::new("gh:o/r#1")]);
        let rollups = vec![
            tier_rollup("hit", 1, 1, 10, &["gh:o/r#1"]),
            tier_rollup("miss", 1, 11, 20, &["gh:o/r#99"]),
            tier_rollup("everyone", 1, 21, 30, &[]),
        ];
        let out = tier_sections(&rollups, &mine, 3);
        assert_eq!(out.len(), 1, "one tier band");
        assert!(out[0].body.contains("hit"), "{}", out[0].body);
        assert!(
            out[0].body.contains("everyone"),
            "empty notable_refs means notable to everyone (P1-D13)"
        );
        assert!(!out[0].body.contains("miss"), "{}", out[0].body);
    }

    #[test]
    fn zero_rollups_render_no_tier_band_at_all() {
        assert!(tier_sections(&[], &BTreeSet::new(), 3).is_empty());
    }

    // ---- tail ---------------------------------------------------------------------------------

    #[test]
    fn tail_de_interleaves_concurrent_wakes_by_wake_id() {
        let steps = vec![
            step("a", 1, "w1"),
            step("b", 2, "w2"),
            step("c", 3, "w1"),
            step("d", 4, "w2"),
        ];
        let blocks = de_interleave(&steps);
        assert_eq!(blocks.len(), 2, "two wakes, two blocks");
        assert_eq!(seqs(&blocks[0]), vec![1, 3]);
        assert_eq!(seqs(&blocks[1]), vec![2, 4]);
    }

    #[test]
    fn a_single_wake_tail_is_plain_seq_order() {
        let steps: Vec<Step> = (1..=4).map(|n| step(&format!("s{n}"), n, "w1")).collect();
        let blocks = de_interleave(&steps);
        assert_eq!(blocks.len(), 1);
        assert_eq!(seqs(&blocks[0]), vec![1, 2, 3, 4]);
    }

    #[test]
    fn wake_blocks_order_by_their_first_selected_seq() {
        // Handed to the band in reverse: the order must come from the rows, not from arrival.
        let steps = vec![
            step("d", 9, "late"),
            step("c", 4, "early"),
            step("b", 7, "late"),
            step("a", 2, "early"),
        ];
        let blocks = de_interleave(&steps);
        assert_eq!(blocks[0][0].wake.as_str(), "early");
        assert_eq!(blocks[1][0].wake.as_str(), "late");
        assert_eq!(seqs(&blocks[0]), vec![2, 4]);
        assert_eq!(seqs(&blocks[1]), vec![7, 9]);
    }

    // ---- mail ---------------------------------------------------------------------------------

    #[test]
    fn mail_headers_group_by_class_newest_first() {
        let steps = vec![
            mail_step("m1", 1, "ordinary", "first"),
            mail_step("m2", 2, "wake", "second"),
            mail_step("m3", 3, "ordinary", "third"),
        ];
        let s = mail_section(&steps).unwrap();
        let ordinary = s.body.find("### ordinary").unwrap();
        let wake = s.body.find("### wake").unwrap();
        assert!(ordinary < wake, "classes are ordered, not arrival-ordered");
        let third = s.body.find("third").unwrap();
        let first = s.body.find("first").unwrap();
        assert!(third < first, "newest first inside a class");
    }

    // ---- ledger-backed ------------------------------------------------------------------------

    #[tokio::test]
    async fn a_superseding_pin_retires_its_predecessor() {
        let f = Fixture::memory().await;
        let old = f.pin_set("first rule", "v1", &[]).await;
        let new = f
            .pin_set("first rule", "v2", std::slice::from_ref(&old))
            .await;
        let live = f.live_pins().await;
        let ids: Vec<_> = live.iter().map(|p| p.step.clone()).collect();
        assert_eq!(
            ids,
            vec![new],
            "the superseded pin is gone, the new one stands"
        );
        let s = pins_section(&live).unwrap();
        assert!(s.body.contains("v2") && !s.body.contains("v1"));
    }

    #[tokio::test]
    async fn a_retired_pin_leaves_the_projection() {
        let f = Fixture::memory().await;
        let p = f.pin_set("temporary", "until friday", &[]).await;
        assert_eq!(f.live_pins().await.len(), 1);
        f.pin_retire(&[p], "friday came").await;
        assert!(f.live_pins().await.is_empty());
        assert!(
            pins_section(&f.live_pins().await).is_none(),
            "no pins ⇒ no band at all, not an empty header"
        );
    }

    #[tokio::test]
    async fn re_accepting_a_requirement_supersedes_its_old_pin() {
        let f = Fixture::memory().await;
        let v1 = f
            .pin_set("requirement: ship gated", "gates green", &[])
            .await;
        // The requirement is re-accepted with edited text: one pin, the new one.
        let v2 = f
            .pin_set("requirement: ship gated", "gates green AND pushed", &[v1])
            .await;
        let live = f.live_pins().await;
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].step, v2);
        assert_eq!(live[0].text, "gates green AND pushed");
    }

    #[tokio::test]
    async fn unconsumed_mail_only() {
        let f = Fixture::memory().await;
        let consumed = f.mail("alice", "old news").await;
        let standing = f.mail("bob", "still waiting").await;
        f.close_wake_consuming(&[consumed]).await;
        let mail = f.unconsumed().await;
        let s = mail_section(&mail).unwrap();
        assert!(s.body.contains("still waiting"), "{}", s.body);
        assert!(!s.body.contains("old news"), "{}", s.body);
        assert_eq!(s.cites.steps, vec![standing.id]);
    }

    fn seqs(block: &[Step]) -> Vec<u64> {
        block.iter().map(|s| s.seq.0).collect()
    }
}
