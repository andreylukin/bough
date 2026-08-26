//! Invariant: everything under `config` is PURE — no kernel state, no tokio, no I/O beyond reading
//! the files an `include:` names. The loader turns YAML layers into one `Composition`; the kernel
//! turns a `Composition` into fibers. Keeping the two apart is what lets `--dump-config` and boot
//! share one code path (§0.5, V6).

pub mod compose;
pub mod entry;
pub mod expr;
pub mod patch;
pub mod render;

pub use compose::{Composer, Composition, Fingerprint, PluginLookup, RowProvenance};
pub use entry::{parse_entries, Entry, Inject, InjectRepr, RealmLabel};
pub use expr::{
    eval_str, evaluate_tree, normalize_expr_tags, Expr, ExprEnv, ExprError, ExprValue,
    FromExprValue, EXPR_TAG, PLATFORM,
};
pub use patch::{EntryPatch, Insert, InsertAt, Patch, PatchRepr};
pub use render::{render, DumpFormat};

pub use crate::error::ComposeError;

bough_util::brand_id!(
    /// Identifies one patch layer: `"bundle:bough-base"`, `"profile:tui"`, `"user"`,
    /// `"patch:0:/path/to.yml"`.
    pub struct LayerId;
);

/// A patch that named a row id no layer ever created. A warning, never an error (§0.2).
#[derive(Clone, Debug)]
pub enum ComposeWarning {
    AbsentRowId {
        layer: LayerId,
        id: crate::fiber::EntryId,
    },
}
