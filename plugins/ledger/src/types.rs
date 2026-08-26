//! Invariant: the step-type map is MERGE-EXTENSIBLE (§3). A plugin adds types, never replaces the
//! map; a duplicate name is an error naming the owner; a type unknown to the reading binary is
//! refused on read unless its stored `ignorable` flag is set (P1-D7).

use std::collections::BTreeMap;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::error::LedgerError;
use crate::id::StepType;
use crate::step::{Append, Class};

/// Which classes a step type admits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClassRule {
    /// Only [`crate::step::Class::Evidence`] — so the row always carries cites.
    Evidence,
    /// Only [`crate::step::Class::Thought`].
    Thought,
    /// Either class; the caller decides per append.
    Either,
}

impl ClassRule {
    /// The spelling used in [`LedgerError::ClassRuleViolated`].
    pub fn as_str(&self) -> &'static str {
        match self {
            ClassRule::Evidence => "evidence",
            ClassRule::Thought => "thought",
            ClassRule::Either => "evidence or thought",
        }
    }

    /// Whether `class` satisfies this rule.
    pub fn admits(&self, class: Class) -> bool {
        matches!(
            (self, class),
            (ClassRule::Either, _)
                | (ClassRule::Evidence, Class::Evidence)
                | (ClassRule::Thought, Class::Thought)
        )
    }
}

/// One entry of the step-type map.
#[derive(Clone)]
pub struct StepTypeDef {
    pub name: StepType,
    /// The body schema. Compiled once at registration; every append is validated against it.
    pub schema: schemars::Schema,
    /// A binary that does not know this type SKIPS such rows on read instead of refusing (§3).
    pub ignorable: bool,
    pub class_rule: ClassRule,
    /// Catalog name of the plugin that declared it; it is what error messages name.
    pub owner: &'static str,
    /// The compiled validator for `schema`, built ONCE in [`StepTypeDef::of`] so an append pays a
    /// validation, never a compilation. Private: it is derived from `schema` and must stay so.
    validator: Arc<jsonschema::Validator>,
}

impl std::fmt::Debug for StepTypeDef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StepTypeDef")
            .field("name", &self.name)
            .field("ignorable", &self.ignorable)
            .field("class_rule", &self.class_rule)
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

impl StepTypeDef {
    /// Derive the body schema from `T`. `ignorable` defaults to `false` and `class_rule` to
    /// [`ClassRule::Thought`]; the builders below change them.
    ///
    /// PANICS if `T`'s schema cannot be compiled — for a type THIS binary owns that is a build
    /// bug, not a runtime condition. A plugin declaring step types for a `JsonSchema` impl it did
    /// not write should call [`StepTypeDef::try_of`], which fails that fiber instead of the
    /// process.
    pub fn of<T: schemars::JsonSchema>(name: &str, owner: &'static str) -> Self {
        Self::try_of::<T>(name, owner)
            .unwrap_or_else(|e| panic!("step type `{name}`: uncompilable body schema: {e}"))
    }

    /// [`StepTypeDef::of`] as a refusal rather than a panic.
    pub fn try_of<T: schemars::JsonSchema>(
        name: &str,
        owner: &'static str,
    ) -> Result<Self, LedgerError> {
        let schema = schemars::SchemaGenerator::default().into_root_schema_for::<T>();
        let value = schema.as_value().clone();
        let validator = jsonschema::validator_for(&value).map_err(|e| LedgerError::BodySchema {
            kind: StepType::new(name),
            detail: format!("uncompilable body schema: {e}"),
        })?;
        Ok(Self {
            name: StepType::new(name),
            schema,
            ignorable: false,
            class_rule: ClassRule::Thought,
            owner,
            validator: Arc::new(validator),
        })
    }

    /// Validate one body against this type's schema.
    pub fn validate_body(&self, body: &serde_json::Value) -> Result<(), LedgerError> {
        self.validator
            .validate(body)
            .map_err(|e| LedgerError::BodySchema {
                kind: self.name.clone(),
                detail: format!("{e}"),
            })
    }
    /// Builder: mark the type ignorable by a binary that does not know it.
    pub fn ignorable(mut self, yes: bool) -> Self {
        self.ignorable = yes;
        self
    }
    /// Builder: set the class rule.
    pub fn class_rule(mut self, rule: ClassRule) -> Self {
        self.class_rule = rule;
        self
    }
}

/// Returned by registration. Unregisters when [`StepTypeToken::unregister`] is called or when the
/// owning EFFECT is disposed — never on its own `Drop` (§0.2: registrations are effects).
pub struct StepTypeToken {
    #[doc(hidden)]
    pub(crate) inner: Arc<dyn Fn() + Send + Sync>,
}

impl std::fmt::Debug for StepTypeToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StepTypeToken")
    }
}

impl StepTypeToken {
    /// Remove the type from the map.
    pub fn unregister(self) {
        (self.inner)();
    }
    /// The inverse, as a callable an effect can `defer_sync`. Consumes the token, so the two ways
    /// of spending it are exclusive.
    pub fn into_inverse(self) -> impl FnOnce() + Send + 'static {
        let inner = self.inner;
        move || inner()
    }
}

/// The sixteen types the Definition installs into every provider at construction. Owner
/// `"ledger"`. See `vocabulary.rs` for the bodies.
pub fn builtin_step_types() -> Vec<StepTypeDef> {
    use crate::vocabulary as v;
    const OWNER: &str = "ledger";
    vec![
        StepTypeDef::of::<v::WakeStart>("wake/start", OWNER).class_rule(ClassRule::Thought),
        StepTypeDef::of::<v::WakeEnd>("wake/end", OWNER).class_rule(ClassRule::Thought),
        StepTypeDef::of::<v::StepStart>("step/start", OWNER).class_rule(ClassRule::Thought),
        StepTypeDef::of::<v::StepEnd>("step/end", OWNER).class_rule(ClassRule::Thought),
        StepTypeDef::of::<v::RequestHeader>("request/header", OWNER).class_rule(ClassRule::Thought),
        StepTypeDef::of::<v::InboxSpliced>("inbox/spliced", OWNER).class_rule(ClassRule::Thought),
        StepTypeDef::of::<v::MailDelivered>("mail/delivered", OWNER)
            .class_rule(ClassRule::Evidence),
        StepTypeDef::of::<v::RollupSealed>("rollup/sealed", OWNER).class_rule(ClassRule::Evidence),
        StepTypeDef::of::<v::PinSet>("pin/set", OWNER).class_rule(ClassRule::Either),
        StepTypeDef::of::<v::PinRetire>("pin/retire", OWNER).class_rule(ClassRule::Thought),
        StepTypeDef::of::<v::ClaimProposed>("claim/proposed", OWNER).class_rule(ClassRule::Thought),
        StepTypeDef::of::<v::ClaimAccepted>("claim/accepted", OWNER)
            .class_rule(ClassRule::Evidence),
        StepTypeDef::of::<v::ClaimRejected>("claim/rejected", OWNER).class_rule(ClassRule::Thought),
        StepTypeDef::of::<v::ActionIntent>("action/intent", OWNER).class_rule(ClassRule::Thought),
        StepTypeDef::of::<v::ActionDone>("action/done", OWNER).class_rule(ClassRule::Evidence),
        StepTypeDef::of::<v::ForkEndSeed>("fork/end-seed", OWNER).class_rule(ClassRule::Thought),
    ]
}

/// The map itself, shared by both providers so registration behaviour cannot diverge.
#[derive(Default)]
pub struct StepTypeMap {
    /// `Arc` so an unregister token can outlive the borrow that made it, which is what lets
    /// registration be an EFFECT rather than a `Drop` guard (§0.2).
    #[doc(hidden)]
    pub(crate) inner: Arc<RwLock<BTreeMap<StepType, StepTypeDef>>>,
}

impl StepTypeMap {
    /// A map preloaded with [`builtin_step_types`].
    pub fn with_builtins() -> Self {
        let map = Self::default();
        for def in builtin_step_types() {
            map.register(def)
                .expect("the builtin step types have distinct names")
                // The builtins are the Definition's own and are never unregistered; spending the
                // token here is what says so.
                .forget();
        }
        map
    }
    /// Add one type. `Err(DuplicateStepType)` if the name is taken.
    pub fn register(&self, def: StepTypeDef) -> Result<StepTypeToken, LedgerError> {
        let name = def.name.clone();
        {
            let mut guard = self.inner.write();
            if let Some(existing) = guard.get(&name) {
                return Err(LedgerError::DuplicateStepType {
                    kind: name,
                    owner: existing.owner,
                });
            }
            guard.insert(name.clone(), def);
        }
        let inner = self.inner.clone();
        Ok(StepTypeToken {
            inner: Arc::new(move || {
                inner.write().remove(&name);
            }),
        })
    }
    /// Every registered type, sorted by name.
    pub fn all(&self) -> Vec<StepTypeDef> {
        self.inner.read().values().cloned().collect()
    }
    /// Look one up.
    pub fn get(&self, kind: &StepType) -> Option<StepTypeDef> {
        self.inner.read().get(kind).cloned()
    }
    /// Validate an append against the type's class rule, cite requirement and body schema.
    /// The whole of the pre-transaction check, so both providers refuse identically.
    pub fn validate_append(&self, req: &Append) -> Result<StepTypeDef, LedgerError> {
        let def = self
            .get(&req.kind)
            .ok_or_else(|| LedgerError::UnknownStepTypeOnAppend {
                kind: req.kind.clone(),
            })?;
        if !def.class_rule.admits(req.class) {
            return Err(LedgerError::ClassRuleViolated {
                kind: req.kind.clone(),
                expected: def.class_rule.as_str(),
                got: req.class.as_str(),
            });
        }
        if req.class == Class::Evidence && req.cites.is_empty() {
            return Err(LedgerError::EvidenceWithoutCites {
                kind: req.kind.clone(),
            });
        }
        def.validate_body(&req.body)?;
        Ok(def)
    }
}

impl StepTypeToken {
    /// Spend the token without ever unregistering. Only the builtins do this.
    fn forget(self) {
        std::mem::drop(self.inner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::TrajId;
    use crate::step::Append;

    #[derive(schemars::JsonSchema)]
    #[allow(dead_code)]
    struct Note {
        text: String,
    }

    fn append(kind: &str, class: Class, body: serde_json::Value) -> Append {
        Append {
            traj: TrajId::new("t"),
            wake: crate::id::WakeId::new("w"),
            kind: StepType::new(kind),
            class,
            body,
            cites: vec![crate::step::Cite {
                r#ref: crate::id::Ref::new("gh:o/r#1"),
                url: None,
            }],
            at: chrono::Utc::now(),
            id: None,
        }
    }

    #[test]
    fn duplicate_type_is_an_error() {
        let map = StepTypeMap::with_builtins();
        let err = map
            .register(StepTypeDef::of::<Note>("wake/start", "probe"))
            .expect_err("a name already in the map must be refused");
        match err {
            LedgerError::DuplicateStepType { kind, owner } => {
                assert_eq!(kind.as_str(), "wake/start");
                // The error names the plugin that ALREADY owns it, which is who the reader has to
                // go talk to.
                assert_eq!(owner, "ledger");
            }
            other => panic!("wrong refusal: {other}"),
        }
    }

    #[test]
    fn unregister_removes_the_type() {
        let map = StepTypeMap::with_builtins();
        let token = map
            .register(StepTypeDef::of::<Note>("probe/note", "probe"))
            .expect("a fresh name registers");
        assert!(map.get(&StepType::new("probe/note")).is_some());
        token.unregister();
        assert!(
            map.get(&StepType::new("probe/note")).is_none(),
            "unregister must leave the map as if the type had never been declared"
        );
        // And the name is free again.
        map.register(StepTypeDef::of::<Note>("probe/note", "probe"))
            .expect("the name is free after unregister");
    }

    #[test]
    fn builtin_types_have_distinct_names() {
        let defs = builtin_step_types();
        assert_eq!(defs.len(), 16, "§2.3 declares sixteen builtin step types");
        let names: std::collections::BTreeSet<_> = defs.iter().map(|d| d.name.clone()).collect();
        assert_eq!(names.len(), defs.len(), "duplicate builtin step type name");
        assert_eq!(StepTypeMap::with_builtins().all().len(), 16);
    }

    #[test]
    fn class_rule_is_enforced_per_type() {
        let map = StepTypeMap::with_builtins();
        // `mail/delivered` is EVIDENCE-only: a thought of that type is refused.
        let req = append(
            "mail/delivered",
            Class::Thought,
            serde_json::json!({ "class": "wake", "from": "gh:o/r#1", "subject": "s", "summary": "x" }),
        );
        let err = map.validate_append(&req).expect_err("evidence-only type");
        match err {
            LedgerError::ClassRuleViolated { expected, got, .. } => {
                assert_eq!(expected, "evidence");
                assert_eq!(got, "thought");
            }
            other => panic!("wrong refusal: {other}"),
        }
        // As evidence with cites, the same body is accepted.
        let ok = append(
            "mail/delivered",
            Class::Evidence,
            serde_json::json!({ "class": "wake", "from": "gh:o/r#1", "subject": "s", "summary": "x" }),
        );
        map.validate_append(&ok).expect("evidence with cites");
        // `step/start` is THOUGHT-only, the mirror case.
        let err = map
            .validate_append(&append(
                "step/start",
                Class::Evidence,
                serde_json::json!({ "index": 0 }),
            ))
            .expect_err("thought-only type");
        assert!(matches!(err, LedgerError::ClassRuleViolated { .. }));
    }

    #[test]
    fn body_failing_its_schema_is_refused() {
        let map = StepTypeMap::with_builtins();
        let err = map
            .validate_append(&append(
                "step/start",
                Class::Thought,
                serde_json::json!({ "index": "not a number" }),
            ))
            .expect_err("a body of the wrong shape must be refused");
        match err {
            LedgerError::BodySchema { kind, detail } => {
                assert_eq!(kind.as_str(), "step/start");
                assert!(!detail.is_empty(), "the refusal must say what failed");
            }
            other => panic!("wrong refusal: {other}"),
        }
        // A missing required field fails too.
        assert!(matches!(
            map.validate_append(&append("step/start", Class::Thought, serde_json::json!({}))),
            Err(LedgerError::BodySchema { .. })
        ));
        // And the well-formed body passes.
        map.validate_append(&append(
            "step/start",
            Class::Thought,
            serde_json::json!({ "index": 3 }),
        ))
        .expect("a valid body is accepted");
    }
}
