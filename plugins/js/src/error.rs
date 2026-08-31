//! Invariant: this taxonomy is what the MODEL sees. A program ends with exactly one of these or
//! with a [`crate::Run`] — never both.

/// The one thing that lands as a `program/error` step.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JsError {
    Syntax {
        message: String,
        line: Option<u32>,
        col: Option<u32>,
    },
    Thrown {
        message: String,
        stack: Option<String>,
    },
    OpsExceeded {
        ops: u64,
    },
    MemoryExceeded {
        bytes: usize,
    },
    TimeExceeded {
        ms: u64,
    },
    StackExceeded,
    Cancelled,
    /// No Provider set an engine. Fail-loud: the `tools-codemode` row refuses to boot.
    NoEngine,
}

impl std::fmt::Display for JsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JsError::Syntax { message, .. } => write!(f, "syntax error: {message}"),
            JsError::Thrown { message, .. } => write!(f, "uncaught: {message}"),
            JsError::OpsExceeded { ops } => write!(f, "the program exceeded {ops} operations"),
            JsError::MemoryExceeded { bytes } => write!(f, "the program exceeded {bytes} bytes"),
            JsError::TimeExceeded { ms } => write!(f, "the program exceeded {ms}ms"),
            JsError::StackExceeded => write!(f, "the program exceeded its stack"),
            JsError::Cancelled => write!(f, "the program was cancelled"),
            JsError::NoEngine => write!(f, "no JS engine is installed on the `js` seam"),
        }
    }
}

impl std::error::Error for JsError {}
