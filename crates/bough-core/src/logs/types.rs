//! Port of `src/logs/types.ts` — the shapes the pipeline passes between its
//! stages, and the vocabulary the formatters render.
//!
//! "The JSON formatter serializes `Analysis` almost verbatim, which makes this
//! file the de-facto public contract of `--json`: renaming a field here changes
//! an output format somebody may be parsing." Field names are therefore the TS
//! names exactly (`patternCount`, `firstSeen`, …) and optional fields are
//! OMITTED rather than nulled, because TS spreads them conditionally.

use serde::{Serialize, Serializer};

/// JS prints an integral float without a decimal point (`5`, not `5.0`) and
/// `serde_json` prints `5.0`. `--json` is a wire format somebody may diff
/// against the TS implementation, so integral floats are emitted as integers.
pub(crate) fn js_num<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 9e15 {
        s.serialize_i64(*v as i64)
    } else {
        s.serialize_f64(*v)
    }
}

// ---------------------------------------------------------------------------
// Variables
// ---------------------------------------------------------------------------

/// What a variable slot turned out to hold. `enum` and `id` are NOT decided by
/// the masker — they are properties of a slot's whole value distribution and
/// are assigned in `stats.rs` once the counts exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VarKind {
    Ipv4,
    Ipv6,
    Uuid,
    Url,
    Path,
    Duration,
    Bytes,
    Hex,
    Float,
    Int,
    Quoted,
    Timestamp,
    Enum,
    Id,
    String,
}

impl VarKind {
    /// The placeholder text, i.e. what goes between the angle brackets.
    pub fn as_str(self) -> &'static str {
        match self {
            VarKind::Ipv4 => "ipv4",
            VarKind::Ipv6 => "ipv6",
            VarKind::Uuid => "uuid",
            VarKind::Url => "url",
            VarKind::Path => "path",
            VarKind::Duration => "duration",
            VarKind::Bytes => "bytes",
            VarKind::Hex => "hex",
            VarKind::Float => "float",
            VarKind::Int => "int",
            VarKind::Quoted => "quoted",
            VarKind::Timestamp => "timestamp",
            VarKind::Enum => "enum",
            VarKind::Id => "id",
            VarKind::String => "string",
        }
    }
}

/// One variable occurrence pulled out of one line.
#[derive(Debug, Clone, PartialEq)]
pub struct VarValue {
    pub kind: VarKind,
    /// The text exactly as it appeared, punctuation and unit suffix included.
    pub raw: String,
    /// The comparable magnitude, normalized to a base unit (ms / bytes).
    pub num: Option<f64>,
    /// Where the placeholder sits in the logtype, as a CHARACTER offset.
    pub at: usize,
}

/// One entry of a slot's top-values ranking.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TopValue {
    pub value: String,
    pub count: u64,
    #[serde(serialize_with = "js_num")]
    pub share: f64,
}

/// Quantiles, for the kinds that carry a magnitude.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NumericSummary {
    #[serde(serialize_with = "js_num")]
    pub min: f64,
    #[serde(serialize_with = "js_num")]
    pub max: f64,
    #[serde(serialize_with = "js_num")]
    pub mean: f64,
    #[serde(serialize_with = "js_num")]
    pub p50: f64,
    #[serde(serialize_with = "js_num")]
    pub p90: f64,
    #[serde(serialize_with = "js_num")]
    pub p99: f64,
    /// The base unit `num` was normalized to — `ms`, `bytes`, or absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

/// What one variable slot of one pattern turned out to be, over every line.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VarSummary {
    pub slot: usize,
    pub kind: VarKind,
    pub count: u64,
    /// Estimated distinct values (HyperLogLog, ~1.6% error).
    pub unique: u64,
    /// `None` (serialized as `null`) when the slot has too many distinct values
    /// for a ranking to mean anything.
    pub top: Option<Vec<TopValue>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub numeric: Option<NumericSummary>,
}

// ---------------------------------------------------------------------------
// Patterns
// ---------------------------------------------------------------------------

/// The severity read off a line's own words, ordered so comparisons work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

/// `SEVERITIES` in `types.ts`, same order.
pub const SEVERITIES: [Severity; 5] = [
    Severity::Debug,
    Severity::Info,
    Severity::Warn,
    Severity::Error,
    Severity::Fatal,
];

impl Severity {
    /// Rank for comparisons; higher is worse.
    pub fn rank(self) -> u8 {
        match self {
            Severity::Debug => 0,
            Severity::Info => 1,
            Severity::Warn => 2,
            Severity::Error => 3,
            Severity::Fatal => 4,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Debug => "debug",
            Severity::Info => "info",
            Severity::Warn => "warn",
            Severity::Error => "error",
            Severity::Fatal => "fatal",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum AnomalyKind {
    #[serde(rename = "frequency-spike")]
    FrequencySpike,
    #[serde(rename = "error-burst")]
    ErrorBurst,
    #[serde(rename = "single-value")]
    SingleValue,
    #[serde(rename = "bimodal")]
    Bimodal,
    #[serde(rename = "rare")]
    Rare,
    #[serde(rename = "high-cardinality")]
    HighCardinality,
    #[serde(rename = "long-tail")]
    LongTail,
}

/// Something about a pattern that a reader would want pointed at.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Anomaly {
    pub kind: AnomalyKind,
    /// One line, already phrased for a human. Formatters print this verbatim.
    pub detail: String,
}

/// One cluster of structurally identical lines.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pattern {
    pub id: u32,
    pub template: String,
    pub count: u64,
    #[serde(serialize_with = "js_num")]
    pub share: f64,
    pub severity: Severity,
    #[serde(rename = "firstSeen", skip_serializing_if = "Option::is_none")]
    pub first_seen: Option<i64>,
    #[serde(rename = "lastSeen", skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<i64>,
    pub vars: Vec<VarSummary>,
    pub examples: Vec<String>,
    pub buckets: Vec<u64>,
    pub anomalies: Vec<Anomaly>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum CorrelationKind {
    #[serde(rename = "temporal")]
    Temporal,
    #[serde(rename = "shared-value")]
    SharedValue,
}

/// Two patterns that appear to be related, and why.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Correlation {
    pub a: u32,
    pub b: u32,
    pub kind: CorrelationKind,
    #[serde(serialize_with = "js_num")]
    pub strength: f64,
    pub detail: String,
}

// ---------------------------------------------------------------------------
// The analysis
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TimeSpan {
    pub from: i64,
    pub to: i64,
}

/// Everything one run learned. This is what `--json` prints.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Analysis {
    pub lines: u64,
    #[serde(rename = "patternCount")]
    pub pattern_count: usize,
    pub patterns: Vec<Pattern>,
    pub correlations: Vec<Correlation>,
    #[serde(rename = "timeSpan", skip_serializing_if = "Option::is_none")]
    pub time_span: Option<TimeSpan>,
    #[serde(rename = "bucketMs", skip_serializing_if = "Option::is_none")]
    pub bucket_ms: Option<i64>,
    pub truncated: bool,
}
