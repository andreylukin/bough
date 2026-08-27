//! Invariant: line numbers are in the coordinates of the version the model VIEWED. Earlier
//! operations do not shift later numbers, and a file this session never saw is refused rather
//! than patched blind. A port of `git show main:crates/bough-core/src/hostfn/patch.rs`.

/// The six operations of main's patch grammar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpKind {
    /// `SWAP A.=B:` — replaces lines A..B.
    Swap,
    /// `DEL A.=B` — removes them.
    Del,
    /// `INS.PRE A:` — before line A.
    InsPre,
    /// `INS.POST A:` — after line A.
    InsPost,
    /// `INS.HEAD:` — at the file's start.
    InsHead,
    /// `INS.TAIL:` — at its end.
    InsTail,
}

/// One operation against one file, in VIEWED coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct PatchOp {
    pub path: String,
    /// The hash anchor: `[path#TAG]`, or empty for `[path#]` ("the version you just saw").
    pub tag: String,
    pub kind: OpKind,
    pub from: usize,
    pub to: usize,
    /// `+`-prefixed NEW text only. There are no `-` rows.
    pub body: Vec<String>,
}

/// One file's operations, grouped.
#[derive(Clone, Debug, PartialEq)]
pub struct FileOps {
    pub path: String,
    pub tag: String,
    pub ops: Vec<PatchOp>,
}

/// Why a patch was refused.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum PatchError {
    #[error("{0}")]
    Grammar(String),
    #[error("`{path}` was never viewed in this session; view it before patching it")]
    Unseen { path: String },
    #[error("`{path}` changed since you viewed it (tag {saw} is now {now})")]
    StaleTag {
        path: String,
        saw: String,
        now: String,
    },
    #[error("`{path}`: line {line} is outside the file's {count} lines")]
    OutOfRange {
        path: String,
        line: usize,
        count: usize,
    },
    #[error("`{path}`: the edit at lines {from}..{to} conflicts with a change made since you viewed it")]
    Conflict {
        path: String,
        from: usize,
        to: usize,
    },
    #[error("{0}")]
    Io(String),
}

/// CRLF and a BOM do not change a file's identity.
///
/// WP-3 owns the body.
pub fn normalize(_text: &str) -> String {
    todo!("WP-3: port main's normalize")
}

/// fnv1a over utf16 code units, 4 hex chars — the `[path#TAG]` anchor.
///
/// WP-3 owns the body.
pub fn tag_of(_text: &str) -> String {
    todo!("WP-3: port main's tag_of")
}

/// `[path#TAG]` followed by `N:text` rows — what `view` returns.
///
/// WP-3 owns the body.
pub fn render_numbered(_path: &str, _text: &str) -> String {
    todo!("WP-3: port main's render_numbered")
}

/// Parse a patch body into operations.
///
/// WP-3 owns the body.
pub fn parse_patch(_input: &str) -> Result<Vec<PatchOp>, PatchError> {
    todo!("WP-3: port main's parse_patch")
}

/// Group operations by file, preserving order.
///
/// WP-3 owns the body.
pub fn group_by_file(_ops: &[PatchOp]) -> Result<Vec<FileOps>, PatchError> {
    todo!("WP-3: port main's group_by_file")
}

/// Range and overlap checks against a file of `count` lines.
///
/// WP-3 owns the body.
pub fn check_ops(_path: &str, _ops: &[PatchOp], _count: usize) -> Result<(), PatchError> {
    todo!("WP-3: port main's check_ops")
}

/// Apply operations to lines, all in VIEWED coordinates.
///
/// WP-3 owns the body.
pub fn materialize(_lines: &[String], _ops: &[PatchOp]) -> Vec<String> {
    todo!("WP-3: port main's materialize")
}
