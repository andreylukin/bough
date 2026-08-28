//! Invariant: the two GitHub identifiers a review thread is addressed by are DIFFERENT id spaces,
//! and this module is what keeps them from being spelled with one string (§0.2: opaque
//! cross-boundary ids are branded types, never bare strings).
//!
//! - [`ReviewCommentId`] is the REST database id of a review comment:
//!   `repos/{owner}/{repo}/pulls/comments/{id}`, and the reply endpoint
//!   `repos/{owner}/{repo}/pulls/{n}/comments/{id}/replies`. It is a NUMBER.
//! - [`ReviewThreadNodeId`] is the GraphQL `ID!` of a `PullRequestReviewThread`
//!   (`PRRT_…`), which is what `resolveReviewThread(input:{threadId:…})` takes. It is an
//!   OPAQUE STRING and is never derivable from the number — this crate LOOKS IT UP
//!   ([`crate::GithubActions::thread_node_id`]).
//! - [`CommentNodeId`] is the GraphQL `ID!` of the comment itself, which is what
//!   `minimizeComment(input:{subjectId:…})` takes. Also opaque, also not the thread's.

/// The REST database id of a review comment. The model names a thread by this.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
#[serde(transparent)]
pub struct ReviewCommentId(pub u64);

impl std::fmt::Display for ReviewCommentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The GraphQL node id of a `PullRequestReviewThread`. Never a [`ReviewCommentId`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct ReviewThreadNodeId(pub String);

impl std::fmt::Display for ReviewThreadNodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The GraphQL node id of a review COMMENT (`node_id` on the REST object).
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct CommentNodeId(pub String);

impl std::fmt::Display for CommentNodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
