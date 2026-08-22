//! The error taxonomy (port of `src/errors.ts`).
//!
//! "Every domain error … carries the status it should become, so the router has
//! exactly ONE try/catch that turns a thrown error into a response and no
//! handler contains a per-error catch block." A domain module never constructs
//! a response.
//!
//! **Error text is a product surface.** Two audiences: the user (HTTP) and the
//! MODEL (the message becomes the exception a program catches). Each message
//! names *what failed*, *the state that caused it*, and *the move that resolves
//! it*. A message that says only "failed" is a defect. Port every
//! constructor-site message string verbatim — tests grep substrings and the
//! model's behavior is trained on them.

use thiserror::Error;

/// The provider layer's error (`bough-llm`). It becomes [`BoughError::Llm`]
/// at the boundary (`From` below); re-exported so downstream crates can name
/// it without depending on `bough-llm` themselves.
pub use bough_llm::LlmError;

/// The caller-status error families. `name()` maps each to the TS class name
/// that appears in logs and the JSON error body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    BadRequest,
    NotFound,
    Conflict,
    /// Path escaped `confine` root. Not a security boundary, but the server's
    /// own path handling must not be steerable by a name in a URL.
    Path,
    Turn,
    /// Must distinguish timeout from interrupt and say what partial work survived.
    Program,
    /// Carries file + line range + "someone else changed those lines".
    Patch,
    /// Catchable "user declined".
    AskDeclined,
    Agent,
    Workflow,
    /// Raised at SUBMIT time, not mid-run.
    WorkflowScript,
    Branch,
    Fork,
    Compact,
    Sections,
    Extract,
    Move,
    Handoff,
    Changes,
    Schedule,
    /// 16KB/key limit.
    State,
    Artifact,
    /// 401 surfaces as "not authorized — open the mcp panel (^p) and press a",
    /// NEVER a hang.
    Mcp,
    /// The LSP backend itself failed — distinct from an empty result, which is
    /// an ordinary answer and not an error at all.
    Lsp,
    /// A non-2xx response is DATA, not an exception.
    Net,
    Skill,
    /// 503 — the index is gone, not the query. Named separately from a 400
    /// because the fix is different in kind: nothing the user retypes will
    /// help (`server/search.rs`).
    SearchIndexUnavailable,
}

impl ErrorKind {
    /// The TS class name — appears in logs and the JSON error body.
    pub fn name(&self) -> &'static str {
        match self {
            ErrorKind::BadRequest => "BadRequestError",
            ErrorKind::NotFound => "NotFoundError",
            ErrorKind::Conflict => "ConflictError",
            ErrorKind::Path => "PathError",
            ErrorKind::Turn => "TurnError",
            ErrorKind::Program => "ProgramError",
            ErrorKind::Patch => "PatchError",
            ErrorKind::AskDeclined => "AskDeclinedError",
            ErrorKind::Agent => "AgentError",
            ErrorKind::Workflow => "WorkflowError",
            ErrorKind::WorkflowScript => "WorkflowScriptError",
            ErrorKind::Branch => "BranchError",
            ErrorKind::Fork => "ForkError",
            ErrorKind::Compact => "CompactError",
            ErrorKind::Sections => "SectionsError",
            ErrorKind::Extract => "ExtractError",
            ErrorKind::Move => "MoveError",
            ErrorKind::Handoff => "HandoffError",
            ErrorKind::Changes => "ChangesError",
            ErrorKind::Schedule => "ScheduleError",
            ErrorKind::State => "StateError",
            ErrorKind::Artifact => "ArtifactError",
            ErrorKind::Mcp => "McpError",
            ErrorKind::Lsp => "LspError",
            ErrorKind::Net => "NetError",
            ErrorKind::Skill => "SkillError",
            ErrorKind::SearchIndexUnavailable => "SearchIndexUnavailableError",
        }
    }
}

/// One error taxonomy for the whole tree. Variants carrying distinct data get
/// their own arm; the caller-status families are [`BoughError::Http`] with an
/// [`ErrorKind`].
#[derive(Debug, Clone, Error, PartialEq)]
pub enum BoughError {
    /// The caller-status families (Turn/Agent/Workflow/Branch/Changes/
    /// Schedule/State/Artifact/Mcp/Net/Skill…) plus the fixed-status classes
    /// (BadRequest 400, NotFound 404, Conflict 409, Path 400, Program 400,
    /// Patch 400, AskDeclined 400, WorkflowScript 400).
    #[error("{message}")]
    Http {
        status: u16,
        kind: ErrorKind,
        message: String,
    },
    /// `status` drives retry classification; `None` = transport fault, always
    /// retryable (the constructor default is 502).
    #[error("{message}")]
    Llm {
        status: u16,
        retry_after_ms: Option<u64>,
        message: String,
    },
    /// 429. The message says WHICH cap: per-turn 8 or concurrent tree-wide 4.
    #[error("{message}")]
    SpawnCap { message: String },
    /// 413. The message NAMES the limit; compaction is user-initiated, never
    /// silent.
    #[error("{message}")]
    ContextOverflow { message: String },
}

impl BoughError {
    pub fn http(status: u16, kind: ErrorKind, message: impl Into<String>) -> Self {
        BoughError::Http {
            status,
            kind,
            message: message.into(),
        }
    }
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::http(400, ErrorKind::BadRequest, message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::http(404, ErrorKind::NotFound, message)
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::http(409, ErrorKind::Conflict, message)
    }
    pub fn path(message: impl Into<String>) -> Self {
        Self::http(400, ErrorKind::Path, message)
    }
    pub fn program(message: impl Into<String>) -> Self {
        Self::http(400, ErrorKind::Program, message)
    }
    pub fn patch(message: impl Into<String>) -> Self {
        Self::http(400, ErrorKind::Patch, message)
    }
    pub fn ask_declined(message: impl Into<String>) -> Self {
        Self::http(400, ErrorKind::AskDeclined, message)
    }
    pub fn workflow_script(message: impl Into<String>) -> Self {
        Self::http(400, ErrorKind::WorkflowScript, message)
    }
    /// `LlmError` with the TS constructor defaults (`status = 502`).
    pub fn llm(message: impl Into<String>) -> Self {
        BoughError::Llm {
            status: 502,
            retry_after_ms: None,
            message: message.into(),
        }
    }
    /// `LlmError` with an explicit status and optional Retry-After hint (ms).
    pub fn llm_with(message: impl Into<String>, status: u16, retry_after_ms: Option<u64>) -> Self {
        BoughError::Llm {
            status,
            retry_after_ms,
            message: message.into(),
        }
    }
    /// 429 — the message says WHICH cap: per-turn 8 or concurrent tree-wide 4.
    pub fn spawn_cap(message: impl Into<String>) -> Self {
        BoughError::SpawnCap {
            message: message.into(),
        }
    }
    /// 413 — the message NAMES the limit.
    pub fn context_overflow(message: impl Into<String>) -> Self {
        BoughError::ContextOverflow {
            message: message.into(),
        }
    }

    /// The provider layer's own error, when this is one — what the retry
    /// classifier and the trace writer in `bough-llm` take.
    pub fn as_llm(&self) -> Option<bough_llm::LlmError> {
        match self {
            BoughError::Llm {
                status,
                retry_after_ms,
                message,
            } => Some(bough_llm::LlmError {
                status: *status,
                retry_after_ms: *retry_after_ms,
                message: message.clone(),
            }),
            _ => None,
        }
    }

    /// The HTTP status this error should become.
    pub fn status(&self) -> u16 {
        match self {
            BoughError::Http { status, .. } => *status,
            BoughError::Llm { status, .. } => *status,
            BoughError::SpawnCap { .. } => 429,
            BoughError::ContextOverflow { .. } => 413,
        }
    }

    /// The TS class name for logs and the JSON error body.
    pub fn name(&self) -> &'static str {
        match self {
            BoughError::Http { kind, .. } => kind.name(),
            BoughError::Llm { .. } => "LlmError",
            BoughError::SpawnCap { .. } => "SpawnCapError",
            BoughError::ContextOverflow { .. } => "ContextOverflowError",
        }
    }
}

/// The provider layer's error crosses into the tree as `BoughError::Llm`,
/// field for field — status, Retry-After hint and the message verbatim.
impl From<bough_llm::LlmError> for BoughError {
    fn from(err: bough_llm::LlmError) -> Self {
        BoughError::Llm {
            status: err.status,
            retry_after_ms: err.retry_after_ms,
            message: err.message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_status_constructors() {
        assert_eq!(BoughError::bad_request("x").status(), 400);
        assert_eq!(BoughError::not_found("x").status(), 404);
        assert_eq!(BoughError::conflict("x").status(), 409);
        assert_eq!(BoughError::path("x").status(), 400);
        assert_eq!(BoughError::program("x").status(), 400);
        assert_eq!(BoughError::patch("x").status(), 400);
        assert_eq!(BoughError::ask_declined("x").status(), 400);
        assert_eq!(BoughError::workflow_script("x").status(), 400);
        assert_eq!(BoughError::llm("x").status(), 502);
        assert_eq!(BoughError::spawn_cap("x").status(), 429);
        assert_eq!(BoughError::context_overflow("x").status(), 413);
    }

    #[test]
    fn caller_status_families_carry_the_callers_status() {
        // TurnError / AgentError / WorkflowError / BranchError / ChangesError /
        // ScheduleError / StateError / ArtifactError / McpError / LspError /
        // NetError / SkillError all take the status from the constructor site.
        for kind in [
            ErrorKind::Turn,
            ErrorKind::Agent,
            ErrorKind::Workflow,
            ErrorKind::Branch,
            ErrorKind::Fork,
            ErrorKind::Compact,
            ErrorKind::Sections,
            ErrorKind::Extract,
            ErrorKind::Move,
            ErrorKind::Handoff,
            ErrorKind::Changes,
            ErrorKind::Schedule,
            ErrorKind::State,
            ErrorKind::Artifact,
            ErrorKind::Mcp,
            ErrorKind::Lsp,
            ErrorKind::Net,
            ErrorKind::Skill,
        ] {
            assert_eq!(BoughError::http(400, kind, "x").status(), 400);
            assert_eq!(BoughError::http(404, kind, "x").status(), 404);
            assert_eq!(BoughError::http(409, kind, "x").status(), 409);
        }
    }

    #[test]
    fn llm_status_drives_retry_classification() {
        // No status in TS = the 502 default = a transport fault, always retryable.
        let transport = BoughError::llm("connection reset");
        assert_eq!(transport.status(), 502);
        match &transport {
            BoughError::Llm { retry_after_ms, .. } => assert_eq!(*retry_after_ms, None),
            _ => unreachable!(),
        }
        let rate_limited = BoughError::llm_with("overloaded", 429, Some(1500));
        assert_eq!(rate_limited.status(), 429);
        match &rate_limited {
            BoughError::Llm { retry_after_ms, .. } => assert_eq!(*retry_after_ms, Some(1500)),
            _ => unreachable!(),
        }
    }

    #[test]
    fn every_kind_maps_to_its_ts_class_name() {
        // The TS class name appears in logs and the JSON error body — one
        // mapping per class, exhaustive.
        let table: &[(ErrorKind, &str)] = &[
            (ErrorKind::BadRequest, "BadRequestError"),
            (ErrorKind::NotFound, "NotFoundError"),
            (ErrorKind::Conflict, "ConflictError"),
            (ErrorKind::Path, "PathError"),
            (ErrorKind::Turn, "TurnError"),
            (ErrorKind::Program, "ProgramError"),
            (ErrorKind::Patch, "PatchError"),
            (ErrorKind::AskDeclined, "AskDeclinedError"),
            (ErrorKind::Agent, "AgentError"),
            (ErrorKind::Workflow, "WorkflowError"),
            (ErrorKind::WorkflowScript, "WorkflowScriptError"),
            (ErrorKind::Branch, "BranchError"),
            (ErrorKind::Fork, "ForkError"),
            (ErrorKind::Compact, "CompactError"),
            (ErrorKind::Sections, "SectionsError"),
            (ErrorKind::Extract, "ExtractError"),
            (ErrorKind::Move, "MoveError"),
            (ErrorKind::Handoff, "HandoffError"),
            (ErrorKind::Changes, "ChangesError"),
            (ErrorKind::Schedule, "ScheduleError"),
            (ErrorKind::State, "StateError"),
            (ErrorKind::Artifact, "ArtifactError"),
            (ErrorKind::Mcp, "McpError"),
            (ErrorKind::Lsp, "LspError"),
            (ErrorKind::Net, "NetError"),
            (ErrorKind::Skill, "SkillError"),
        ];
        for (kind, name) in table {
            assert_eq!(kind.name(), *name);
            assert_eq!(BoughError::http(400, *kind, "x").name(), *name);
        }
    }

    #[test]
    fn dedicated_variants_have_their_ts_class_names() {
        assert_eq!(BoughError::llm("x").name(), "LlmError");
        assert_eq!(BoughError::spawn_cap("x").name(), "SpawnCapError");
        assert_eq!(
            BoughError::context_overflow("x").name(),
            "ContextOverflowError"
        );
    }

    #[test]
    fn display_is_the_message() {
        // Error text is a product surface: Display must be the message alone —
        // the model catches it as the exception a program sees.
        assert_eq!(
            BoughError::bad_request("what failed").to_string(),
            "what failed"
        );
        assert_eq!(
            BoughError::llm("stream stalled").to_string(),
            "stream stalled"
        );
        assert_eq!(
            BoughError::spawn_cap("per-turn spawn cap (8) reached").to_string(),
            "per-turn spawn cap (8) reached"
        );
    }
}
