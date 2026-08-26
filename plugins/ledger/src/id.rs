//! Invariant: every identifier that crosses the ledger boundary is a branded newtype (§0.2), and
//! an intra-ledger citation has exactly one spelling — `step:<id>` / `rollup:<id>` (P1-D5).

bough_util::brand_id!(
    /// One trajectory: a lane's chain, or a fork of one.
    pub struct TrajId;
);
bough_util::brand_id!(
    /// One step. uuid v7 by default; tests supply their own so goldens are byte-stable (P1-D6).
    pub struct StepId;
);
bough_util::brand_id!(
    /// One wake. Every step carries one (§3).
    pub struct WakeId;
);
bough_util::brand_id!(
    /// One rollup (tier, digest or reconciliation).
    pub struct RollupId;
);
bough_util::brand_id!(
    /// One row of the actions journal.
    pub struct ActionId;
);
bough_util::brand_id!(
    /// The idempotency key of an action. Phase 1 stores it; Phase 2 owns the formula (P1-D11).
    pub struct IdemKey;
);
bough_util::brand_id!(
    /// An agent's name, which is also the primary key of the (mutable) `agents` row.
    pub struct AgentName;
);
bough_util::brand_id!(
    /// A step type, e.g. `wake/start` or `probe/note`. Dynamic — hence branded, not an enum.
    pub struct StepType;
);
bough_util::brand_id!(
    /// A routing/matching ref: `gh:o/r#12`, `step:<id>`, `rollup:<id>`.
    pub struct Ref;
);

impl Ref {
    /// The one canonical spelling of a citation of a step (P1-D5).
    pub fn step(id: &StepId) -> Ref {
        Ref::new(format!("step:{id}"))
    }
    /// The one canonical spelling of a citation of a rollup (P1-D5).
    pub fn rollup(id: &RollupId) -> Ref {
        Ref::new(format!("rollup:{id}"))
    }
}

impl WakeId {
    /// The synthetic wake a fork's end-seed marker is written under: `seed:<traj>`.
    pub fn seed(child: &TrajId) -> WakeId {
        WakeId::new(format!("seed:{child}"))
    }
}

/// A step's position in its trajectory. 1-based, per trajectory, no gaps (§3).
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, serde::Serialize, serde::Deserialize,
)]
pub struct Seq(pub u64);

/// The on-disk envelope version. Bumped only when the ENVELOPE changes — never when a step type
/// is registered.
pub const LEDGER_FORMAT_VERSION: u32 = 1;

/// sha256 over the declared envelope only (table + column names of `steps`/`edges`/`rollups`, in
/// order). Changing it without bumping [`LEDGER_FORMAT_VERSION`] fails a test. Step types are not
/// part of it.
pub const ENVELOPE: &[(&str, &[&str])] = &[
    (
        "steps",
        &[
            "id",
            "traj_id",
            "seq",
            "at",
            "wake_id",
            "type",
            "class",
            "body",
            "cites",
            "ignorable",
        ],
    ),
    ("step_refs", &["step_id", "ref"]),
    (
        "edges",
        &["child_traj", "parent_traj", "at_seq", "kind", "at"],
    ),
    (
        "rollups",
        &[
            "id",
            "traj_id",
            "kind",
            "tier",
            "from_seq",
            "to_seq",
            "src_trajs",
            "body",
            "notable_refs",
            "prompt_ver",
            "sealed_at",
            "superseded_by",
        ],
    ),
];

pub fn envelope_fingerprint() -> &'static str {
    static FP: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    FP.get_or_init(|| {
        use sha2::Digest;
        let mut h = sha2::Sha256::new();
        for (table, cols) in ENVELOPE {
            h.update(table.as_bytes());
            h.update(b"(");
            for c in *cols {
                h.update(c.as_bytes());
                h.update(b",");
            }
            h.update(b")");
        }
        format!("{:x}", h.finalize())
    })
    .as_str()
}

/// A branded id is a plain string in a body schema. `brand_id!` lives in `bough-util`, which has
/// no `schemars` dependency (§0.1: the util crate is a pure leaf), so the impls are written here —
/// once, by a macro, so no id can acquire a different schema shape than its siblings.
macro_rules! id_json_schema {
    ($($t:ty),* $(,)?) => {$(
        impl schemars::JsonSchema for $t {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                std::borrow::Cow::Borrowed(stringify!($t))
            }
            fn json_schema(_g: &mut schemars::SchemaGenerator) -> schemars::Schema {
                schemars::json_schema!({ "type": "string" })
            }
        }
    )*};
}

id_json_schema!(TrajId, StepId, WakeId, RollupId, ActionId, IdemKey, AgentName, StepType, Ref);

impl schemars::JsonSchema for Seq {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Seq")
    }
    fn json_schema(_g: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "integer", "minimum": 1 })
    }
}
