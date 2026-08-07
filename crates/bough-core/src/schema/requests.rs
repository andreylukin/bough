//! Request bodies for every route the spec describes (port of
//! `src/schema/requests.ts`). One struct per body, parsed at the router edge
//! and nowhere else — a handler receives data that is already the right shape.
//!
//! The invariant worth stating: these shapes (plus their `validate()` checks at
//! the router edge) are the ONLY place a 400 is decided for malformed bodies;
//! semantic 400s belong to the domain module. Naming rule: `<Verb><Noun>Body`.
//!
//! Tri-state PATCH fields (absent = keep, null = clear, value = set) use
//! [`crate::types::Patch`] — the serde double-Option adapter.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::errors::BoughError;
use crate::types::{Effort, Patch};

// ---- shared -----------------------------------------------------------------

/// A field that must be PRESENT and may be `null` — TS's `z.string().nullable()`
/// as opposed to `.nullable().optional()`.
///
/// serde's derive treats EVERY `Option<T>` field as optional: a missing key
/// deserializes to `None` with no error, because the generated code falls back
/// to `missing_field`, which succeeds for `Option`. That silent default is the
/// wrong contract wherever `null` is an INSTRUCTION rather than an absence —
/// `PUT /draft` with a typo'd key would clear the composer instead of answering
/// 400. Naming a `deserialize_with` takes the field off that implicit-default
/// path, so an absent key is `missing field`, while an explicit `null` still
/// reads as `None`.
fn required_nullable<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d)
}

/// One message selected out of a thread, optionally narrowed to specific part
/// indexes. Absent `parts` = the whole message. When present, `parts` must be
/// non-empty (validated at the router edge).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PartPick {
    pub message_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parts: Option<Vec<u32>>,
}

// ---- sessions ---------------------------------------------------------------

/// POST /sessions — all optional; `{}` is legal.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionBody {
    /// Absent = the session is created untitled and the cheap tier names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<super::parts::SessionKind>,
    /// The checkout the session operates on. Must exist at creation time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Per-session pins; absent = the global defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

/// One composer attachment already copied under `~/.bough/attachments/`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PostMessageImage {
    pub path: String,
    pub media_type: String,
    pub name: String,
    pub size: i64,
}

/// POST /sessions/:id/messages. A message posted while a turn runs is queued
/// and drains into a fresh turn — never dropped, never racing the running one.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PostMessageBody {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<PostMessageImage>>,
}

/// PUT /sessions/:id/draft — `null` clears the prefilled composer text.
///
/// `draft` is REQUIRED and nullable, never optional: clearing the composer is
/// something a client must ASK for, so a body that forgot the key is a 400 and
/// not a silent wipe of text the user typed.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SetDraftBody {
    #[serde(deserialize_with = "required_nullable")]
    pub draft: Option<String>,
}

/// PATCH /sessions/:id — per-session `model` / `effort` overrides.
/// Absent = leave the override alone; explicit `null` = clear it.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct PatchSessionBody {
    #[serde(default, skip_serializing_if = "Patch::is_keep")]
    pub model: Patch<String>,
    #[serde(default, skip_serializing_if = "Patch::is_keep")]
    pub effort: Patch<Effort>,
}

/// PUT /model-settings — what a NEW conversation runs on, for the whole
/// install. Same tri-state shape as a session pin, deliberately.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct PutModelSettingsBody {
    #[serde(default, skip_serializing_if = "Patch::is_keep")]
    pub model: Patch<String>,
    #[serde(default, skip_serializing_if = "Patch::is_keep")]
    pub effort: Patch<Effort>,
}

/// POST /sessions/:id/questions/:qid — `{answer}` settles the hold;
/// `{decline: true}` rejects the program's `ask()` with a catchable
/// "user declined".
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AnswerQuestionBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decline: Option<bool>,
}

// ---- history operations ------------------------------------------------------

/// POST /sessions/:id/fork. `editedText` makes it "edit & resend". Limited to
/// the session's OWN messages.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ForkBody {
    pub at_message_id: String,
    /// Cut inside the at-message: keep `parts[0..atPart]` of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_part: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_text: Option<String>,
    /// Cut BEFORE the at-message rather than including it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusive: Option<bool>,
    /// Carry a summary of the ABANDONED tail onto the branch. Off by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summarize_abandoned: Option<bool>,
}

/// POST /sessions/:id/unsend — the take-back. A single id on purpose; there is
/// no "and everything after it" flag.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UnsendBody {
    pub at_message_id: String,
}

/// POST /sessions/:id/compact. `picks` must be non-empty.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CompactBody {
    pub picks: Vec<PartPick>,
    /// Steers what the summary keeps. Absent = the default summarization prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SectionsTurn {
    /// Max 500 chars (validated at the router edge).
    pub gist: String,
}

/// POST /sessions/:id/sections. One gist per turn in thread order; index i of
/// the reply is turn i. Min 1, max 500 turns.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SectionsBody {
    pub turns: Vec<SectionsTurn>,
}

/// POST /sessions/:id/extract — copy picked messages into a fresh ROOT.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExtractBody {
    pub picks: Vec<PartPick>,
}

/// POST /sessions/:id/move-into — append copies of `sourceId`'s picked
/// messages onto THIS session. A copy, never a move.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MoveBody {
    pub source_id: String,
    pub picks: Vec<PartPick>,
}

/// POST /sessions/:id/handoff — draft the opening prompt for a fresh root from
/// a stated goal.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HandoffBody {
    pub goal: String,
}

/// POST /sessions/:id/jobs — the user's own `!command`. 1..4000 chars.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RunShellBody {
    pub command: String,
}

// ---- changes -----------------------------------------------------------------

/// POST /sessions/:id/changes/revert. Absent `paths` reverts the session's
/// whole change set; `paths: []` is a 400 (decided at the router edge).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct RevertChangesBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<String>>,
}

// ---- schedules ---------------------------------------------------------------

/// POST /schedules. `spec` grammar is validated by the schedules module, not here.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CreateScheduleBody {
    pub title: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    pub spec: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// PATCH /schedules/:id — every field optional; `workspace: null` clears it.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct PatchScheduleBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Patch::is_keep")]
    pub workspace: Patch<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

// ---- workflows ---------------------------------------------------------------

/// POST /workflows
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CreateWorkflowBody {
    pub session_id: String,
    pub script: String,
    /// Handed to the script as `args`, verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
}

/// POST /workflows/:id/rerun. With `script`, the edited source replaces the
/// original and only calls whose journal key changed re-run.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct RerunWorkflowBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
}

// ---- artifacts and comments --------------------------------------------------

/// POST /sessions/:id/comments — one pinned note on a served artifact page.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PostCommentBody {
    /// The artifact the note is pinned to, relative to the session's artifact dir.
    pub artifact: String,
    pub text: String,
    /// Free-form anchor the injected widget records (selector, offsets, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Value>,
}

/// POST /sessions/:id/comments/send — deliver the pending batch as one
/// `[artifact comments]` system message.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SendCommentsBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ids: Option<Vec<String>>,
}

// ---- config, keys, theme -----------------------------------------------------

/// PATCH /config. Switching the model moves the default NEW sessions start on
/// and pins `sessionId` when given.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PatchConfigBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Pin the change to one session instead of moving the global default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// PUT /config/keys — provider API keys, written to the launcher env file.
pub type PutKeysBody = HashMap<String, String>;

/// PUT /theme — a NAMED PARTIAL palette. `colors` is intentionally an open
/// record; the theme module owns the token set and rejects unknown tokens.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PutThemeBody {
    pub name: String,
    pub colors: HashMap<String, String>,
}

// ---- MCP ---------------------------------------------------------------------

/// PUT /mcp/servers/:name — one registry entry, local (stdio subprocess) or
/// remote (Streamable HTTP). The MCP module owns the full validation,
/// including `${VAR}` secret references it must not mangle.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum PutMcpServerBody {
    Local {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<HashMap<String, String>>,
    },
    Remote {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        headers: Option<HashMap<String, String>>,
    },
}

/// POST /mcp/servers/:name/enable — grant the server to a session.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpActivationBody {
    pub session_id: String,
    /// e.g. "2h"; absent = until revoked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
}

// ---- search ------------------------------------------------------------------

/// GET /search?q=… — keyword (SQLite FTS) over transcripts. No embeddings.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SearchQuery {
    pub q: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// 1..=200.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

// ---- router-edge validation --------------------------------------------------
// The Rust counterpart of the zod constraints (`.min(1)`, `.max(...)`) that
// serde cannot express. Called by the router edge right after the body parses;
// a failure is the same 400 a failed parse produces. These are the ONLY place
// a malformed-body 400 is decided; semantic 400s belong to the domain module.

fn require_non_empty(value: &str, what: &str) -> Result<(), BoughError> {
    if value.is_empty() {
        return Err(BoughError::bad_request(format!("{what} must not be empty")));
    }
    Ok(())
}

/// `picks` arrays are min-1 everywhere they appear — an empty selection would
/// make a no-op branch.
fn validate_picks(picks: &[PartPick]) -> Result<(), BoughError> {
    if picks.is_empty() {
        return Err(BoughError::bad_request("picks must not be empty"));
    }
    for pick in picks {
        pick.validate()?;
    }
    Ok(())
}

impl PartPick {
    /// `parts`, when present, must name at least one index (zod `min(1)`).
    /// Absent `parts` = the whole message, and is fine.
    pub fn validate(&self) -> Result<(), BoughError> {
        if let Some(parts) = &self.parts {
            if parts.is_empty() {
                return Err(BoughError::bad_request(
                    "parts must name at least one part index when present",
                ));
            }
        }
        Ok(())
    }
}

impl PatchSessionBody {
    pub fn validate(&self) -> Result<(), BoughError> {
        if let Patch::Set(model) = &self.model {
            require_non_empty(model, "model")?;
        }
        Ok(())
    }
}

impl PutModelSettingsBody {
    pub fn validate(&self) -> Result<(), BoughError> {
        if let Patch::Set(model) = &self.model {
            require_non_empty(model, "model")?;
        }
        Ok(())
    }
}

impl CompactBody {
    pub fn validate(&self) -> Result<(), BoughError> {
        validate_picks(&self.picks)
    }
}

impl SectionsBody {
    /// Min 1, max 500 turns; each gist max 500 chars.
    pub fn validate(&self) -> Result<(), BoughError> {
        if self.turns.is_empty() {
            return Err(BoughError::bad_request("turns must not be empty"));
        }
        if self.turns.len() > 500 {
            return Err(BoughError::bad_request(
                "turns must have at most 500 entries",
            ));
        }
        for turn in &self.turns {
            if turn.gist.chars().count() > 500 {
                return Err(BoughError::bad_request(
                    "gist must be at most 500 characters",
                ));
            }
        }
        Ok(())
    }
}

impl ExtractBody {
    pub fn validate(&self) -> Result<(), BoughError> {
        validate_picks(&self.picks)
    }
}

impl MoveBody {
    pub fn validate(&self) -> Result<(), BoughError> {
        validate_picks(&self.picks)
    }
}

impl HandoffBody {
    pub fn validate(&self) -> Result<(), BoughError> {
        require_non_empty(&self.goal, "goal")
    }
}

impl RunShellBody {
    /// 1..=4000 chars.
    pub fn validate(&self) -> Result<(), BoughError> {
        require_non_empty(&self.command, "command")?;
        if self.command.chars().count() > 4000 {
            return Err(BoughError::bad_request(
                "command must be at most 4000 characters",
            ));
        }
        Ok(())
    }
}

impl CreateScheduleBody {
    pub fn validate(&self) -> Result<(), BoughError> {
        require_non_empty(&self.title, "title")?;
        require_non_empty(&self.prompt, "prompt")?;
        if let Some(workspace) = &self.workspace {
            require_non_empty(workspace, "workspace")?;
        }
        Ok(())
    }
}

impl PatchScheduleBody {
    pub fn validate(&self) -> Result<(), BoughError> {
        if let Some(title) = &self.title {
            require_non_empty(title, "title")?;
        }
        if let Some(prompt) = &self.prompt {
            require_non_empty(prompt, "prompt")?;
        }
        if let Patch::Set(workspace) = &self.workspace {
            require_non_empty(workspace, "workspace")?;
        }
        Ok(())
    }
}

impl CreateWorkflowBody {
    pub fn validate(&self) -> Result<(), BoughError> {
        require_non_empty(&self.session_id, "sessionId")?;
        require_non_empty(&self.script, "script")
    }
}

impl RerunWorkflowBody {
    pub fn validate(&self) -> Result<(), BoughError> {
        if let Some(script) = &self.script {
            require_non_empty(script, "script")?;
        }
        Ok(())
    }
}

impl PostCommentBody {
    pub fn validate(&self) -> Result<(), BoughError> {
        require_non_empty(&self.artifact, "artifact")?;
        require_non_empty(&self.text, "text")
    }
}

impl PutThemeBody {
    /// `name` is trimmed, 1..=80 chars (the one trimmed string in the schema).
    pub fn validate(&self) -> Result<(), BoughError> {
        let trimmed = self.name.trim();
        if trimmed.is_empty() {
            return Err(BoughError::bad_request("name must not be empty"));
        }
        if trimmed.chars().count() > 80 {
            return Err(BoughError::bad_request(
                "name must be at most 80 characters",
            ));
        }
        Ok(())
    }
}

impl PutMcpServerBody {
    pub fn validate(&self) -> Result<(), BoughError> {
        match self {
            PutMcpServerBody::Local { command, .. } => require_non_empty(command, "command"),
            // Full URL validation (and `${VAR}` secrets) is the MCP module's.
            PutMcpServerBody::Remote { url, .. } => require_non_empty(url, "url"),
        }
    }
}

impl SearchQuery {
    /// `q` min 1; `limit` 1..=200.
    pub fn validate(&self) -> Result<(), BoughError> {
        require_non_empty(&self.q, "q")?;
        if let Some(limit) = self.limit {
            if limit == 0 || limit > 200 {
                return Err(BoughError::bad_request("limit must be between 1 and 200"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_session_body_accepts_empty_object() {
        let b: CreateSessionBody = serde_json::from_str("{}").unwrap();
        assert_eq!(b, CreateSessionBody::default());
    }

    #[test]
    fn patch_session_body_tri_state() {
        // absent = keep, null = clear, value = set
        let b: PatchSessionBody = serde_json::from_str("{}").unwrap();
        assert!(b.model.is_keep() && b.effort.is_keep());

        let b: PatchSessionBody = serde_json::from_str(r#"{"model":null}"#).unwrap();
        assert_eq!(b.model, Patch::Clear);
        assert!(b.effort.is_keep());

        let b: PatchSessionBody =
            serde_json::from_str(r#"{"model":"claude","effort":"high"}"#).unwrap();
        assert_eq!(b.model, Patch::Set("claude".into()));
        assert_eq!(b.effort, Patch::Set(Effort::High));
    }

    #[test]
    fn set_draft_needs_the_key_because_null_is_an_instruction_not_an_absence() {
        // `z.string().nullable()` — required, and nullable. An explicit null is
        // "clear the composer"; a MISSING key is a malformed body. serde makes
        // every `Option<T>` field optional unless it is taken off the
        // implicit-default path, so without the guard a client that PUT the
        // wrong key wiped text the user had typed and got a 200 for it.
        let set: SetDraftBody = serde_json::from_str(r#"{"draft":"hello"}"#).unwrap();
        assert_eq!(set.draft.as_deref(), Some("hello"));
        let cleared: SetDraftBody = serde_json::from_str(r#"{"draft":null}"#).unwrap();
        assert_eq!(cleared.draft, None);
        for body in ["{}", r#"{"text":"typo"}"#] {
            let err = serde_json::from_str::<SetDraftBody>(body).unwrap_err();
            assert!(
                err.to_string().contains("missing field `draft`"),
                "{body}: {err}"
            );
        }
    }

    #[test]
    fn request_bodies_reject_the_empty_selections_that_make_a_no_op_branch() {
        // The freeze-suite pin: `PartPick.parse({messageId, parts: []})` rejects
        // (min 1 when present); absent parts is the whole message and is fine.
        let empty: PartPick = serde_json::from_str(r#"{"messageId":"m1","parts":[]}"#).unwrap();
        assert!(empty.validate().is_err(), "parts: [] must reject");
        let whole: PartPick = serde_json::from_str(r#"{"messageId":"m1"}"#).unwrap();
        assert_eq!(whole.parts, None);
        assert!(whole.validate().is_ok());

        let compact: CompactBody = serde_json::from_str(r#"{"picks":[]}"#).unwrap();
        assert!(compact.validate().is_err(), "empty picks must reject");
        let compact: CompactBody =
            serde_json::from_str(r#"{"picks":[{"messageId":"m1"}]}"#).unwrap();
        assert!(compact.validate().is_ok());
        // A nested empty-parts pick fails the containing body too.
        let compact: CompactBody =
            serde_json::from_str(r#"{"picks":[{"messageId":"m1","parts":[]}]}"#).unwrap();
        assert!(compact.validate().is_err());
    }

    #[test]
    fn validation_bounds() {
        assert!(RunShellBody { command: "".into() }.validate().is_err());
        assert!(RunShellBody {
            command: "x".repeat(4000)
        }
        .validate()
        .is_ok());
        assert!(RunShellBody {
            command: "x".repeat(4001)
        }
        .validate()
        .is_err());

        let turns = |n: usize| SectionsBody {
            turns: (0..n).map(|_| SectionsTurn { gist: "g".into() }).collect(),
        };
        assert!(turns(0).validate().is_err());
        assert!(turns(1).validate().is_ok());
        assert!(turns(500).validate().is_ok());
        assert!(turns(501).validate().is_err());

        // Tri-state pins clear with null but never with an empty string.
        let b: PatchSessionBody = serde_json::from_str(r#"{"model":""}"#).unwrap();
        assert!(b.validate().is_err());
        let b: PatchSessionBody = serde_json::from_str(r#"{"model":null}"#).unwrap();
        assert!(b.validate().is_ok());

        assert!(SearchQuery {
            q: "x".into(),
            session_id: None,
            limit: Some(0)
        }
        .validate()
        .is_err());
        assert!(SearchQuery {
            q: "x".into(),
            session_id: None,
            limit: Some(200)
        }
        .validate()
        .is_ok());
        assert!(SearchQuery {
            q: "x".into(),
            session_id: None,
            limit: Some(201)
        }
        .validate()
        .is_err());
    }

    #[test]
    fn put_mcp_server_body_union() {
        let b: PutMcpServerBody = serde_json::from_str(r#"{"command":"npx"}"#).unwrap();
        assert!(matches!(b, PutMcpServerBody::Local { .. }));
        let b: PutMcpServerBody =
            serde_json::from_str(r#"{"url":"https://x.example/mcp"}"#).unwrap();
        assert!(matches!(b, PutMcpServerBody::Remote { .. }));
    }
}
