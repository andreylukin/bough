//! Invariant: the bytes a snapshot holds are `Assembled::to_text()` and nothing else — the same
//! function of the same `Assembled` that `agent-loop`'s `request::build` puts in
//! `LlmRequest::system`. Nothing in this module re-spells the surface (§0.2, D-C1).

use bough_plugin_ledger::{AgentName, LedgerHandle, Seq};
use bough_plugin_projection::{AssembleRequest, Assembled, Flag, ProjectionHandle, SectionId};
use chrono::{DateTime, Utc};
use std::collections::BTreeSet;

use crate::error::PreviewError;

/// WHICH ledger high-water the preview assembles at. The pane's whole honesty question (D-C1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreviewAt {
    /// `as_of = ledger.head_seq(traj)`: what the agent would see if it woke this instant — before
    /// the wake writes its own `wake/start`, its mail deliveries and its `step/start`.
    Head,
    /// A named high-water: exactly the value a past wake's `request/header.as_of` carries. The
    /// mode V1 asserts byte-exactness in.
    Seq(Seq),
}

impl PreviewAt {
    /// The word the header prints for this mode.
    pub fn word(&self) -> &'static str {
        match self {
            PreviewAt::Head => "head",
            PreviewAt::Seq(_) => "anchored",
        }
    }
}

/// One taken preview. `text` is THE byte-exact surface.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub agent: AgentName,
    pub at: PreviewAt,
    pub as_of: Seq,
    /// `Assembled::to_text()`, and nothing else.
    pub text: String,
    pub tokens: usize,
    pub budget: usize,
    pub flags: BTreeSet<Flag>,
    /// `(section id, tokens)`, in render order.
    pub sections: Vec<(SectionId, usize)>,
    /// sha256 hex of `text`; equals `request/header.projection_digest` for the same `as_of`.
    pub digest: String,
    pub taken_at: DateTime<Utc>,
}

/// Take a preview. The ONLY call in this crate that reaches the seam; everything else is pure.
///
/// Resolves the agent's trajectory through the ledger's mutable `agents` row (never a `?? default`
/// trajectory), reads `head_seq` for [`PreviewAt::Head`], and calls `ProjectionHandle::assemble`
/// with `wake: None` and `budget: None` — the same call `agent-loop` makes, with the same
/// defaults, so the bytes are the loop's by construction.
pub async fn snapshot(
    projection: &ProjectionHandle,
    ledger: &LedgerHandle,
    agent: &AgentName,
    at: PreviewAt,
    now: DateTime<Utc>,
) -> Result<Snapshot, PreviewError> {
    // The trajectory is RESOLVED, explicitly, from the row that owns it. A preview taken against
    // a defaulted trajectory would be somebody else's context wearing this agent's name.
    let row = ledger
        .0
        .agent(agent)
        .await?
        .ok_or_else(|| PreviewError::NoSuchAgent(agent.to_string()))?;
    if row.traj.as_str().is_empty() {
        return Err(PreviewError::NoTrajectory(agent.to_string()));
    }
    let as_of = match &at {
        PreviewAt::Head => ledger.0.head_seq(&row.traj).await?.unwrap_or(Seq(0)),
        PreviewAt::Seq(seq) => *seq,
    };
    // The loop assembles with the wake it is IN (`agent-loop/src/wake.rs`, step 6), and a section
    // may render differently for one. So does the preview: at an anchored `as_of` the wake is the
    // one that owned the newest step at or below it — the very wake whose `request/header` the
    // byte gate compares against. At head there is no wake yet, which is the honest `None`.
    let wake = match &at {
        PreviewAt::Head => None,
        PreviewAt::Seq(_) => wake_at(ledger, &row.traj, as_of).await?,
    };
    let assembled = projection
        .0
        .assemble(&AssembleRequest {
            agent: agent.clone(),
            wake,
            at: now,
            budget: None,
            as_of: Some(as_of),
        })
        .await?;
    let text = system_prefix(&assembled);
    let digest = digest(&text);
    Ok(Snapshot {
        agent: agent.clone(),
        at,
        as_of,
        tokens: assembled.tokens,
        budget: assembled.budget,
        flags: assembled.flags.clone(),
        sections: assembled
            .sections
            .iter()
            .map(|s| (s.id.clone(), s.tokens))
            .collect(),
        text,
        digest,
        taken_at: now,
    })
}

/// The wake that owned the newest step at or below `as_of`, if the chain has one.
async fn wake_at(
    ledger: &LedgerHandle,
    traj: &bough_plugin_ledger::TrajId,
    as_of: Seq,
) -> Result<Option<bough_plugin_ledger::WakeId>, PreviewError> {
    let steps = ledger
        .0
        .steps(&bough_plugin_ledger::StepQuery {
            trajs: vec![traj.clone()],
            before: Some(Seq(as_of.0 + 1)),
            order: bough_plugin_ledger::Order::SeqDesc,
            limit: Some(1),
            ..Default::default()
        })
        .await?;
    Ok(steps.first().map(|s| s.wake.clone()))
}

/// PURE: the system prefix a request built from `a` carries.
///
/// One line, and it exists so the claim "the pane and the loop spell this the same way" is a call
/// and not a comment.
pub fn system_prefix(a: &Assembled) -> String {
    a.to_text()
}

/// PURE: sha256 hex. The same spelling as `agent_loop::request::digest`.
pub fn digest(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    format!("{:x}", h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_kernel::{Context, KernelCore};
    use bough_plugin_ledger::{Append, Class, LedgerError, LedgerStore, StepType, TrajId, WakeId};
    use bough_plugin_ledger_memory::store::MemoryStore;
    use bough_plugin_projection::{
        FileViewRequest, Place, Position, PrefixSource, PrefixToken, ProjectionError, Projector,
        RenderedSection, SectionCites, SectionSpec, SectionToken, Slot,
    };
    use chrono::TimeZone;
    use parking_lot::Mutex;
    use std::sync::Arc;

    fn at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 27, 9, 0, 0).unwrap()
    }

    /// A projector that records the requests it was handed and answers with the `as_of` it saw.
    struct Spy {
        seen: Mutex<Vec<AssembleRequest>>,
    }

    #[async_trait::async_trait]
    impl Projector for Spy {
        fn provider(&self) -> &'static str {
            "spy"
        }
        fn section(&self, _spec: SectionSpec) -> Result<SectionToken, ProjectionError> {
            unreachable!("the preview never contributes a section")
        }
        async fn assemble(&self, req: &AssembleRequest) -> Result<Assembled, ProjectionError> {
            self.seen.lock().push(req.clone());
            Ok(Assembled {
                agent: req.agent.clone(),
                sections: vec![RenderedSection {
                    id: SectionId::new("tail"),
                    position: Position {
                        slot: Slot::Identity,
                        place: Place::Band,
                    },
                    title: "tail".into(),
                    body: format!("as_of {:?}", req.as_of.map(|s| s.0)),
                    cites: SectionCites::default(),
                    tokens: 7,
                    degraded: None,
                }],
                flags: BTreeSet::new(),
                tokens: 7,
                budget: 100,
                cites: SectionCites::default(),
            })
        }
        async fn file_view(&self, _req: &FileViewRequest) -> Result<String, ProjectionError> {
            unreachable!("the preview never renders a file view")
        }
        fn pin_prefix(
            &self,
            _agent: AgentName,
            _prefix: Assembled,
            _source: PrefixSource,
        ) -> Result<PrefixToken, ProjectionError> {
            unreachable!("the preview pins nothing")
        }
        async fn write_file_view(
            &self,
            _req: &FileViewRequest,
            _dir: Option<&std::path::Path>,
        ) -> Result<std::path::PathBuf, ProjectionError> {
            unreachable!("the preview writes nothing")
        }
    }

    async fn fixture(traj: &str) -> (LedgerHandle, ProjectionHandle, Arc<Spy>) {
        let ctx = Context::root(KernelCore::new());
        let ledger = LedgerHandle(MemoryStore::new(ctx) as Arc<dyn LedgerStore>);
        ledger
            .0
            .put_agent(bough_plugin_ledger::AgentRow {
                name: AgentName::new("sol"),
                traj: TrajId::new(traj),
                routing_refs: Default::default(),
                wake_classes: Default::default(),
                model_override: None,
                tick_floor: None,
                digest_rollup: None,
            })
            .await
            .expect("agents is mutable config");
        let spy = Arc::new(Spy {
            seen: Mutex::new(Vec::new()),
        });
        (ledger, ProjectionHandle(spy.clone()), spy)
    }

    async fn append(ledger: &LedgerHandle, traj: &str, id: &str) -> Result<(), LedgerError> {
        ledger
            .0
            .append(Append {
                traj: TrajId::new(traj),
                wake: WakeId::new("w1"),
                kind: StepType::new("wake/start"),
                class: Class::Thought,
                body: serde_json::json!({ "urgency": "immediate" }),
                cites: Vec::new(),
                at: at(),
                id: Some(bough_plugin_ledger::StepId::new(id)),
            })
            .await
            .map(|_| ())
    }

    #[tokio::test]
    async fn head_uses_the_ledger_head_as_as_of() {
        let (ledger, projection, spy) = fixture("t-sol").await;
        append(&ledger, "t-sol", "s1").await.expect("an append");
        append(&ledger, "t-sol", "s2").await.expect("an append");
        let head = ledger
            .0
            .head_seq(&TrajId::new("t-sol"))
            .await
            .expect("a read")
            .expect("two rows are in");
        let snap = snapshot(
            &projection,
            &ledger,
            &AgentName::new("sol"),
            PreviewAt::Head,
            at(),
        )
        .await
        .expect("a preview");
        assert_eq!(snap.as_of, head);
        let seen = spy.seen.lock();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].as_of, Some(head));
        assert!(seen[0].wake.is_none(), "the loop's default, unchanged");
        assert!(seen[0].budget.is_none(), "the loop's default, unchanged");
    }

    #[tokio::test]
    async fn seq_mode_uses_the_seq_it_was_given() {
        let (ledger, projection, spy) = fixture("t-sol").await;
        append(&ledger, "t-sol", "s1").await.expect("an append");
        append(&ledger, "t-sol", "s2").await.expect("an append");
        let snap = snapshot(
            &projection,
            &ledger,
            &AgentName::new("sol"),
            PreviewAt::Seq(Seq(1)),
            at(),
        )
        .await
        .expect("a preview");
        assert_eq!(snap.as_of, Seq(1));
        assert_eq!(spy.seen.lock()[0].as_of, Some(Seq(1)));
    }

    #[tokio::test]
    async fn an_agent_with_no_trajectory_is_refused_not_defaulted() {
        let (ledger, projection, spy) = fixture("").await;
        let err = snapshot(
            &projection,
            &ledger,
            &AgentName::new("sol"),
            PreviewAt::Head,
            at(),
        )
        .await
        .expect_err("an agent with no trajectory has no context to preview");
        assert!(
            matches!(err, PreviewError::NoTrajectory(ref a) if a == "sol"),
            "{err}"
        );
        assert!(
            spy.seen.lock().is_empty(),
            "nothing was assembled on a defaulted trajectory"
        );

        let missing = snapshot(
            &projection,
            &ledger,
            &AgentName::new("nobody"),
            PreviewAt::Head,
            at(),
        )
        .await
        .expect_err("there is no such agent");
        assert!(matches!(missing, PreviewError::NoSuchAgent(_)), "{missing}");
    }

    #[tokio::test]
    async fn the_digest_is_sha256_of_the_text() {
        let (ledger, projection, _) = fixture("t-sol").await;
        let snap = snapshot(
            &projection,
            &ledger,
            &AgentName::new("sol"),
            PreviewAt::Head,
            at(),
        )
        .await
        .expect("a preview");
        assert_eq!(snap.digest, digest(&snap.text));
        // The literal, so a re-spelling of `digest` cannot pass by agreeing with itself.
        assert_eq!(
            digest(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(snap.digest.len(), 64);
    }
}
