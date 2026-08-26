//! Invariant: the step-type map is MERGE-EXTENSIBLE (§3). A plugin adds types, never replaces the
//! map; a duplicate name is an error naming the owner; a type unknown to the reading binary is
//! refused on read unless its stored `ignorable` flag is set (P1-D7).

use crate::error::LedgerError;
use crate::id::StepType;

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
        todo!("WP-1: ClassRule::as_str")
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
}

impl StepTypeDef {
    /// Derive the body schema from `T`. `ignorable` defaults to `false` and `class_rule` to
    /// [`ClassRule::Thought`]; the builders below change them.
    pub fn of<T: schemars::JsonSchema>(name: &str, owner: &'static str) -> Self {
        todo!("WP-1: StepTypeDef::of")
    }
    /// Builder: mark the type ignorable by a binary that does not know it.
    pub fn ignorable(self, yes: bool) -> Self {
        todo!("WP-1: StepTypeDef::ignorable")
    }
    /// Builder: set the class rule.
    pub fn class_rule(self, rule: ClassRule) -> Self {
        todo!("WP-1: StepTypeDef::class_rule")
    }
}

/// Returned by registration. Unregisters when [`StepTypeToken::unregister`] is called or when the
/// owning EFFECT is disposed — never on its own `Drop` (§0.2: registrations are effects).
pub struct StepTypeToken {
    #[doc(hidden)]
    pub(crate) inner: std::sync::Arc<dyn Fn() + Send + Sync>,
}

impl StepTypeToken {
    /// Remove the type from the map.
    pub fn unregister(self) {
        todo!("WP-1: StepTypeToken::unregister")
    }
}

/// The sixteen types the Definition installs into every provider at construction. Owner
/// `"ledger"`. See `vocabulary.rs` for the bodies.
pub fn builtin_step_types() -> Vec<StepTypeDef> {
    todo!("WP-1: builtin_step_types")
}

/// The map itself, shared by both providers so registration behaviour cannot diverge.
#[derive(Default)]
pub struct StepTypeMap {
    #[doc(hidden)]
    pub(crate) inner: parking_lot::RwLock<std::collections::BTreeMap<StepType, StepTypeDef>>,
}

impl StepTypeMap {
    /// A map preloaded with [`builtin_step_types`].
    pub fn with_builtins() -> Self {
        todo!("WP-1: StepTypeMap::with_builtins")
    }
    /// Add one type. `Err(DuplicateStepType)` if the name is taken.
    pub fn register(&self, def: StepTypeDef) -> Result<StepTypeToken, LedgerError> {
        todo!("WP-1: StepTypeMap::register")
    }
    /// Every registered type, sorted by name.
    pub fn all(&self) -> Vec<StepTypeDef> {
        todo!("WP-1: StepTypeMap::all")
    }
    /// Look one up.
    pub fn get(&self, kind: &StepType) -> Option<StepTypeDef> {
        todo!("WP-1: StepTypeMap::get")
    }
    /// Validate an append against the type's class rule, cite requirement and body schema.
    /// The whole of the pre-transaction check, so both providers refuse identically.
    pub fn validate_append(&self, req: &crate::step::Append) -> Result<StepTypeDef, LedgerError> {
        todo!("WP-1: StepTypeMap::validate_append")
    }
}
