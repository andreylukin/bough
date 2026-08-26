//! Invariant: an `Agent`-scoped section SHADOWS a `Global` section with the same `SectionId`, for
//! that agent alone (§5). Registration is an effect: a disposed section stops rendering, and the
//! registry is left as if it had never mounted.

use std::sync::Arc;

use bough_plugin_ledger::AgentName;
use bough_plugin_projection::{
    ProjectionError, SectionId, SectionScope, SectionSpec, SectionToken,
};

/// One registered section plus the serial that identifies it for removal. The serial, not the
/// `SectionId`, is what a disposer names: the same id may be registered once globally and once
/// per agent, and disposing one must never take the other with it.
struct Slot {
    serial: u64,
    spec: SectionSpec,
}

/// `SectionSpec` is not `Clone` in the Definition (it holds a trait object), but every field is,
/// and `for_agent` hands out copies rather than a borrow of the lock.
pub(crate) fn clone_spec(s: &SectionSpec) -> SectionSpec {
    SectionSpec {
        id: s.id.clone(),
        position: s.position,
        scope: s.scope,
        agent: s.agent.clone(),
        priority: s.priority,
        render: s.render.clone(),
    }
}

/// Every contributed section, global and per-agent.
#[derive(Default)]
pub struct Registry {
    inner: parking_lot::RwLock<Vec<Slot>>,
    next: std::sync::atomic::AtomicU64,
}

impl Registry {
    /// Add one section.
    ///
    /// Refuses a spec that declares agent scope and names no agent, and refuses a second spec at
    /// the same `(id, scope, agent)` — the shadowing rule is about a global and an agent spec
    /// sharing an id, never about two specs at the SAME scope sharing one.
    pub fn add(self: &Arc<Self>, spec: SectionSpec) -> Result<SectionToken, ProjectionError> {
        if spec.scope == SectionScope::Agent && spec.agent.is_none() {
            return Err(ProjectionError::AgentScopeWithoutAgent { id: spec.id });
        }
        // The six built-in band ids are the assembler's own: a contributed section carrying one
        // would be undroppable (`is_builtin`) and would shadow the real band in every rung's
        // `index_of` lookup.
        if crate::resolve::is_reserved_section_id(&spec.id) {
            return Err(ProjectionError::ReservedSection { id: spec.id });
        }
        let mut guard = self.inner.write();
        if guard.iter().any(|s| {
            s.spec.id == spec.id && s.spec.scope == spec.scope && s.spec.agent == spec.agent
        }) {
            return Err(ProjectionError::DuplicateSection { id: spec.id });
        }
        let serial = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        guard.push(Slot { serial, spec });
        drop(guard);

        let me = Arc::clone(self);
        Ok(SectionToken::new(move || {
            me.inner.write().retain(|s| s.serial != serial);
        }))
    }

    /// The specs that apply to `agent`, with agent scope shadowing global by `SectionId`, sorted
    /// into §5's fixed order — `(slot, place, id)`, never registration order (P1-D8).
    pub fn for_agent(&self, agent: &AgentName) -> Vec<SectionSpec> {
        let guard = self.inner.read();
        let shadowed: Vec<SectionId> = guard
            .iter()
            .filter(|s| s.spec.scope == SectionScope::Agent && s.spec.agent.as_ref() == Some(agent))
            .map(|s| s.spec.id.clone())
            .collect();
        let mut out: Vec<SectionSpec> = guard
            .iter()
            .filter(|s| match s.spec.scope {
                SectionScope::Global => !shadowed.contains(&s.spec.id),
                SectionScope::Agent => s.spec.agent.as_ref() == Some(agent),
            })
            .map(|s| clone_spec(&s.spec))
            .collect();
        out.sort_by(|a, b| a.position.sort_key(&a.id).cmp(&b.position.sort_key(&b.id)));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_projection::{
        DropPriority, Place, Position, SectionBody, SectionRender, SectionRequest, Slot as Band,
    };

    struct Fixed(&'static str);

    #[async_trait::async_trait]
    impl SectionRender for Fixed {
        async fn render(
            &self,
            _req: &SectionRequest,
        ) -> Result<Option<SectionBody>, ProjectionError> {
            Ok(Some(SectionBody {
                title: self.0.to_string(),
                body: self.0.to_string(),
                cites: Default::default(),
            }))
        }
    }

    fn spec(
        id: &str,
        scope: SectionScope,
        agent: Option<&str>,
        band: Band,
        text: &'static str,
    ) -> SectionSpec {
        SectionSpec {
            id: SectionId::new(id),
            position: Position {
                slot: band,
                place: Place::After,
            },
            scope,
            agent: agent.map(AgentName::new),
            priority: DropPriority::Coarse,
            render: Arc::new(Fixed(text)),
        }
    }

    #[test]
    fn agent_scope_shadows_global_for_that_agent_only() {
        let reg = Arc::new(Registry::default());
        reg.add(spec(
            "about",
            SectionScope::Global,
            None,
            Band::Identity,
            "global",
        ))
        .unwrap();
        reg.add(spec(
            "about",
            SectionScope::Agent,
            Some("sol"),
            Band::Identity,
            "sol",
        ))
        .unwrap();

        let sol = reg.for_agent(&AgentName::new("sol"));
        assert_eq!(sol.len(), 1, "sol sees exactly one `about`");
        assert_eq!(sol[0].scope, SectionScope::Agent);

        let terra = reg.for_agent(&AgentName::new("terra"));
        assert_eq!(terra.len(), 1, "terra still sees the global `about`");
        assert_eq!(terra[0].scope, SectionScope::Global);
    }

    #[test]
    fn a_disposed_section_stops_rendering() {
        let reg = Arc::new(Registry::default());
        let tok = reg
            .add(spec("note", SectionScope::Global, None, Band::Tail, "n"))
            .unwrap();
        assert_eq!(reg.for_agent(&AgentName::new("sol")).len(), 1);
        tok.remove();
        assert!(
            reg.for_agent(&AgentName::new("sol")).is_empty(),
            "the registry is left as if the section had never mounted"
        );
    }

    #[test]
    fn two_sections_in_one_band_order_by_id() {
        let reg = Arc::new(Registry::default());
        // Registered z-then-a on purpose: the order must not be registration order (P1-D8).
        reg.add(spec("z", SectionScope::Global, None, Band::Tail, "z"))
            .unwrap();
        reg.add(spec("a", SectionScope::Global, None, Band::Tail, "a"))
            .unwrap();
        let got: Vec<String> = reg
            .for_agent(&AgentName::new("sol"))
            .iter()
            .map(|s| s.id.to_string())
            .collect();
        assert_eq!(got, vec!["a".to_string(), "z".to_string()]);
    }
}
