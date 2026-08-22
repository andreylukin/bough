//! The cheap tier's shared call, and the first of its three features: **auto
//! session titles** (port of `src/worker/titles.ts`).
//!
//! THE INVARIANT THIS MODULE HOLDS, and the reason all three cheap-tier
//! features are written the way they are: **a cheap-model call can only ever
//! ADD something. It can never take anything away, delay anything, or fail
//! anything.** The contract is enforced at the type level by `CheapTier`
//! (`types.rs`: these methods resolve `None` on failure and NEVER error) and
//! structurally here by [`cheap_text`], which is the only path any of the
//! three take to a provider and which has no erroring branch at all.
//!
//! That is stronger than "wrap the call site in try/catch". A missing API key
//! errors from the provider client; a provider 500 errors from `run`; a hung
//! connection never errors at all. All three are the same non-event to a
//! caller, and the third is the one error-handling alone does not cover —
//! hence the deadline.
//!
//! MODULE POSITION. This file is the BASE of the cheap trio: `worker/ghost`
//! and `worker/activity` use [`cheap_text`] and [`cheap_model`] from here and
//! nothing imports back — the three reach DOWN to a shared primitive rather
//! than across to each other.
//!
//! THE TITLE FEATURE ITSELF is a bus listener, not a call site inside the
//! message handler: `server/sessions` persists a user message and announces
//! it; nothing about naming a session belongs on that path. Subscribing to
//! `message.started` gets the same trigger with none of the coupling.
//!
//! WHICH MODEL. The cheap tier is a single hosted model for the whole install
//! (spec §12), read from the environment at CALL time rather than captured at
//! boot. Never `ctx.model`: a user pinned to Opus for the coding work must
//! not pay Opus rates to put five words in a sidebar.

use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex};

use futures::FutureExt;
use regex::Regex;

use crate::bus::Bus;
use crate::llm::routing::{process_env, Env};
use crate::llm::{client_for, complete_text, ClientOpts, CompleteTextOpts};
use crate::schema::events::{BoughEvent, EventInput, EventType};
use crate::schema::parts::{Message, Part, Role, Session};
use crate::types::{CheapTier, LlmClient, SharedDb};

// ---------------------------------------------------------------------------
// The shared cheap call
// ---------------------------------------------------------------------------

/// The install-wide default when nothing is pinned. Outranked by the picker's
/// own write ([`set_cheap_model`]).
pub const CHEAP_MODEL_ENV: &str = "BOUGH_CHEAP_MODEL";

/// The floor when the picker has never been used. Small, hosted, and fast.
pub const DEFAULT_CHEAP_MODEL: &str = "claude-haiku-4-5";

/// The picker's write, mirroring `~/.bough/model.json`'s `cheapModel`.
///
/// A process global rather than a field on `AppCtx` because of WHERE the value
/// is read: `cheap_text` is reached from a bus listener and from a host
/// function, neither of which is handed a ctx, and the whole tier is
/// constructed once at boot by `create_cheap_tier()`. Threading a ctx to those
/// two call sites to carry one string would be a larger change than the
/// feature. The shape it replaces was already a process-wide read-per-call —
/// this is the same lifetime with a writer.
static CHEAP_PIN: LazyLock<std::sync::RwLock<Option<String>>> =
    LazyLock::new(|| std::sync::RwLock::new(None));

/// Install the stored cheap-model pin. Boot calls this with what is on disk;
/// `PUT /model-settings` calls it again with what was just saved, which is why
/// a pick needs no restart. `None` clears it, falling back to the env.
pub fn set_cheap_model(model: Option<String>) {
    let clean = model
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty());
    if let Ok(mut pin) = CHEAP_PIN.write() {
        *pin = clean;
    }
}

/// The pin in force, or `None` when nothing is stored.
pub fn cheap_model_pin() -> Option<String> {
    CHEAP_PIN.read().ok().and_then(|p| p.clone())
}

/// How long any cheap-model call may take before it is abandoned.
///
/// A deadline is not politeness here, it is the third failure mode: a provider
/// that neither answers nor errors would otherwise leave a ghost-text request
/// hanging and — worse — hold a session's one activity slot forever, so every
/// later round in that session would be dropped as "already in flight".
/// Abandoning is what makes the drop rule self-healing.
pub const CHEAP_TIMEOUT_MS: u64 = 12_000;

/// The cheap model the ENVIRONMENT asks for, from an injected env reader.
/// Deliberately blind to the pin, so a test that injects an env reader gets
/// exactly what it injected no matter what another test stored.
pub fn cheap_model_with(env: &Env) -> String {
    env(CHEAP_MODEL_ENV)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_CHEAP_MODEL.to_string())
}

/// The cheap model in force: the picker's write, else the env, else the floor.
/// Read per call, so a pick needs no restart.
///
/// The pin outranks the env for the same reason the frontier tier's stored
/// default outranks `BOUGH_MODEL` (`server/sessions.rs`): `BOUGH_CHEAP_MODEL`
/// is read from the launching shell and frozen there, so a pin that did not
/// beat it could never take effect for anyone who had ever exported it.
pub fn cheap_model() -> String {
    cheap_model_pin().unwrap_or_else(|| cheap_model_with(&process_env()))
}

/// The injectable seams of one cheap call. All absent in production; tests
/// inject an `llm` so nothing here ever reaches a real provider.
#[derive(Clone, Default)]
pub struct CheapCallOpts {
    /// Injected in tests. Absent = the provider-routed client for the cheap
    /// model.
    pub llm: Option<Arc<dyn LlmClient>>,
    /// Injected in tests. Absent = [`cheap_model`].
    pub model: Option<String>,
    pub timeout_ms: Option<u64>,
    pub env: Option<Env>,
}

/// One cheap-model completion. **Never errors, never hangs, never logs.**
///
/// Returns the concatenated text blocks, or `None` for every failure there
/// is: no key, an unroutable model id, a provider error, a refusal, an empty
/// answer, or the deadline. The caller cannot tell them apart and must not
/// try — every one of them means the same thing, which is that this round has
/// no title/ghost/blurb and the next one will describe itself.
///
/// Silent by design, including the absence of a `tracing::warn!`. A cosmetic
/// call that fires on every round would turn a lapsed API key into thousands
/// of lines of server log, burying the failures that matter.
pub async fn cheap_text(
    system: &str,
    prompt: &str,
    max_tokens: i64,
    opts: &CheapCallOpts,
) -> Option<String> {
    let timeout_ms = opts.timeout_ms.unwrap_or(CHEAP_TIMEOUT_MS);
    let model = opts.model.clone().unwrap_or_else(|| match &opts.env {
        Some(env) => cheap_model_with(env),
        None => cheap_model(),
    });
    let llm = opts
        .llm
        .clone()
        .unwrap_or_else(|| client_for(&model, ClientOpts::default()));
    let fut = complete_text(
        &llm,
        CompleteTextOpts {
            model,
            system: system.to_string(),
            max_tokens,
            prompt: prompt.to_string(),
        },
    );
    // The deadline drops the in-flight future, which cancels the request —
    // "abandon at deadline" (a hung provider is a `None` like any other).
    match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), fut).await {
        Ok(Ok(text)) => {
            let text = text.trim();
            if text.is_empty() {
                None
            } else {
                Some(text.to_string())
            }
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Titles
// ---------------------------------------------------------------------------

/// CONSOLIDATED, never appended to: a small model asked to satisfy a list of
/// clauses satisfies the first few. The grounding sentence came from a
/// measurement — without it the cheap tier invented subjects for files
/// nothing had read.
pub const TITLE_SYSTEM: &str =
    "You name coding sessions. Given the user's first message, reply with a short title only: \
     3-6 words, sentence case, no quotes, no trailing period, no preamble like 'Title:'. \
     Name only what the message names — never invent a subject, file, or domain it does not \
     mention.";

/// The project's own words, appended to [`TITLE_SYSTEM`] when there are any.
///
/// WHY A TITLER NEEDS A GLOSSARY. "tell me about nased" was titled *Nasal
/// decongestant medication overview* — while the session's actual answer,
/// which had the tag memory and the notes in front of it, correctly explained
/// NAS Element Demand. The titler is a separate cheap call with none of that
/// context, so it did the only thing it could with an unfamiliar token: it
/// guessed at the nearest English word. And the title is the primary
/// navigation surface, so the one surface with no domain context is the one
/// the human reads first.
///
/// The list is the same ranked vocabulary the session is primed with, so the
/// titler and the turn are looking at one set of words.
fn title_system_with(glossary: &[String]) -> String {
    if glossary.is_empty() {
        return TITLE_SYSTEM.to_string();
    }
    format!(
        "{TITLE_SYSTEM} Words this project uses, which may look like ordinary English but \
         are its own names for things: {}. Carry such a word through into the title as \
         written; never expand it into what it sounds like.",
        glossary.join(", ")
    )
}

/// How many project words the titler is shown. Enough to cover the names that
/// actually recur, short enough that the cheap call stays cheap.
const TITLE_GLOSSARY_TAGS: usize = 12;

/// Messages that name nothing to name a session after.
///
/// A bare greeting produced *Casual greeting session*, *Quick chat starter*,
/// *Simple greeting session* and a dozen more like them — a paid call whose
/// output is noise in the one list the user scans. Deferring costs nothing:
/// the placeholder survives, and the next message (the one that says what the
/// work is) titles the session, because the guard only ever replaces a
/// placeholder.
fn names_nothing(text: &str) -> bool {
    let cleaned: String = text
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect();
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    if words.is_empty() || words.len() > 4 {
        return false;
    }
    const PLEASANTRIES: [&str; 22] = [
        "hi",
        "hey",
        "hello",
        "yo",
        "sup",
        "good",
        "morning",
        "afternoon",
        "evening",
        "thanks",
        "thank",
        "you",
        "ok",
        "okay",
        "cool",
        "nice",
        "there",
        "again",
        "howdy",
        "greetings",
        "test",
        "ping",
    ];
    words.iter().all(|w| PLEASANTRIES.contains(w))
}

/// A title needs the gist, not a 50KB paste — and the paste is what is billed.
pub const TITLE_MAX_INPUT: usize = 2000;

/// The longest title the sidebar is asked to render.
pub const TITLE_MAX_CHARS: usize = 60;

/// Small models decorate. Take the first real line, strip the label and the
/// quoting, then cap. `""` = refused: no title at all is strictly better than
/// a bad one — the session falls back to its workspace name, which is true.
pub fn sanitize_title(raw: &str) -> String {
    static LABEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(title\s*:)\s*").unwrap());
    // MARKDOWN LEADERS. A live row read `# Big Python File Creation`: the
    // marker rode into the tree and masked the casing fix below. Stripped
    // BEFORE the quote stripping, which never saw them.
    static LEADER: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s*(#{1,6}|[-*•])\s+").unwrap());
    static QUOTES_LEAD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new("^[\"'\u{201C}\u{201D}`*]+").unwrap());
    static QUOTES_TRAIL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new("[\"'\u{201C}\u{201D}`*.]+$").unwrap());
    // The model ANSWERED instead of titling. Capping that manufactures a lie
    // ("I don't have access to your codebase, so") — a reply never begins a
    // noun phrase, so it is refused outright.
    static REPLY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)^(i|i'm|i'll|i've|sorry|sure|certainly|okay|ok|here|let|as|based|the user|you)\b",
        )
        .unwrap()
    });

    let line = raw
        .trim()
        .split('\n')
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let cleaned = LABEL.replace(line, "");
    let cleaned = LEADER.replace(&cleaned, "");
    let cleaned = QUOTES_LEAD.replace(&cleaned, "");
    let cleaned = QUOTES_TRAIL.replace(&cleaned, "");
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return String::new();
    }

    // A TITLE HAS TO CARRY INFORMATION. Three LETTERS is the floor: "Bug" and
    // "CI fix" pass; `1`, `42`, `-` and `ok` do not.
    if cleaned.chars().filter(|c| c.is_ascii_alphabetic()).count() < 3 {
        return String::new();
    }
    if REPLY.is_match(cleaned) {
        return String::new();
    }

    // Eight words turns a story into a readable stub; a genuine 3-6 word
    // title passes untouched. Trailing connectives are trimmed rather than
    // refused: a cap that lands on one leaves "… and the".
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    let mut capped: Vec<&str> = words.into_iter().take(8).collect();
    while capped.len() > 1 {
        let last = capped[capped.len() - 1]
            .trim_end_matches([',', ';', ':'])
            .to_lowercase();
        let connective = matches!(
            last.as_str(),
            "and"
                | "or"
                | "but"
                | "so"
                | "because"
                | "that"
                | "which"
                | "with"
                | "for"
                | "to"
                | "of"
                | "in"
                | "on"
                | "a"
                | "an"
                | "the"
        );
        if !connective {
            break;
        }
        capped.pop();
    }
    let mut out: String = sentence_case(&capped.join(" "))
        .chars()
        .take(TITLE_MAX_CHARS)
        .collect();
    if out.ends_with([',', ';', ':']) {
        out.pop();
    }
    out.trim().to_string()
}

/// Undo the model's Title Case so the tree reads as one column.
///
/// ONLY fires when EVERY word is capitalized, which is the decorating tic and
/// nothing else. Words that carry their own capitalization are never lowered
/// — `C`, `CI` and `API` pass through a title that IS rewritten, while a
/// lowercase-initial word such as `getUser` or `mod.py` stops the rewrite
/// entirely: an identifier is not prose, and recasing one makes the title
/// wrong rather than merely inconsistent.
fn sentence_case(title: &str) -> String {
    static PROSE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Z][a-z]+$").unwrap());
    let words: Vec<&str> = title.split(' ').collect();
    let prose = |w: &str| PROSE.is_match(w);
    // Anything else a Title-Cased title may legitimately contain: `C`, `API`,
    // `b()`, `3`.
    let opaque = |w: &str| !w.chars().next().is_some_and(|c| c.is_ascii_lowercase());
    if !words.iter().all(|w| prose(w) || opaque(w)) {
        return title.to_string();
    }
    if !words.iter().any(|w| prose(w)) {
        return title.to_string();
    }
    words
        .iter()
        .enumerate()
        .map(|(i, w)| {
            if i == 0 || !prose(w) {
                (*w).to_string()
            } else {
                w.to_lowercase()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `CheapTier::title`. Resolves the sanitized title, or `None` — never errors.
///
/// Also used by `history/compact` to name a compaction branch from its first
/// summary, which is why it takes free text rather than a session id.
pub async fn cheap_title(
    first_message: &str,
    glossary: &[String],
    opts: &CheapCallOpts,
) -> Option<String> {
    let text: String = first_message.chars().take(TITLE_MAX_INPUT).collect();
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let raw = cheap_text(&title_system_with(glossary), text, 64, opts).await?;
    let title = sanitize_title(&raw);
    if title.is_empty() {
        None
    } else {
        Some(title)
    }
}

// ---------------------------------------------------------------------------
// The auto-title feature
// ---------------------------------------------------------------------------

/// What titling needs off the app context. `cheap` absent = the feature is
/// off.
#[derive(Clone)]
pub struct TitleCtx {
    pub db: SharedDb,
    pub bus: Arc<Bus>,
    pub cheap: Option<Arc<dyn CheapTier>>,
}

/// The in-flight ledger: one title per session at a time, shared by every
/// trigger on one watcher.
pub type Inflight = Arc<Mutex<HashSet<String>>>;

#[derive(Clone, Default)]
pub struct AutoTitleOpts {
    /// The title a session must still be carrying for a generated one to
    /// replace it. Defaults to `""`, which is what `POST /sessions` stores.
    /// A subagent passes its spawn-time task stub so a content-derived name
    /// still supersedes it.
    pub placeholder: String,
    /// One title per session at a time: two messages posted in quick
    /// succession must not buy two titles for the same placeholder.
    pub inflight: Option<Inflight>,
}

/// Name a session from its first user message, in the background.
///
/// **Returns immediately and never errors.** Nothing waits on it. The session
/// is renamed — and `session.updated` published, which is what re-renders
/// every connected sidebar — if and only if the cheap tier answers.
///
/// Two guards, both about not overwriting a fact someone else established.
/// Before the call: the session must still be carrying the placeholder, so a
/// titled or renamed session is never re-titled and never re-billed. After
/// it: the SAME check again, because a user can rename during the round-trip.
/// This project's ranked tag vocabulary — the same words the session itself is
/// primed with, so the titler and the turn cannot disagree about what a name
/// means.
///
/// Every failure resolves to an empty glossary. A session with no workspace,
/// no history yet, or a database that will not answer gets the plain title
/// prompt, which is what it got before this existed.
fn title_glossary(ctx: &TitleCtx, session: &Session) -> Vec<String> {
    let Ok(db) = ctx.db.lock() else {
        return Vec::new();
    };
    let workspace = db
        .get_session_runtime(&session.id)
        .ok()
        .and_then(|r| r.workspace)
        .unwrap_or_default();
    if workspace.is_empty() {
        return Vec::new();
    }
    crate::history::tags::stats::top_repo_tags(
        &*db,
        &workspace,
        session.created_at,
        TITLE_GLOSSARY_TAGS,
    )
    .unwrap_or_default()
}

pub fn maybe_auto_title(ctx: &TitleCtx, session_id: &str, text: &str, opts: AutoTitleOpts) {
    let Some(cheap) = ctx.cheap.clone() else {
        return;
    };
    if text.trim().is_empty() {
        return;
    }
    // A greeting names nothing. Titling it buys *Casual greeting session* and
    // spends a call to do it; DEFERRING costs nothing, because the placeholder
    // survives and the next message — the one that says what the work is —
    // titles the session under the same guard.
    if names_nothing(text) {
        return;
    }

    let session = match ctx.db.lock().unwrap().get_session(session_id) {
        Ok(Some(s)) => s,
        _ => return,
    };
    // A `! command` title is PROVISIONAL. The TUI names a conversation after
    // the shell command that created it, and a guard that only ever replaced
    // an EMPTY title left a conversation that went on to do real work
    // permanently called `! ls -1 src`.
    let provisional = session.title.starts_with("! ");
    if session.title != opts.placeholder && !provisional {
        return;
    }

    if let Some(inflight) = &opts.inflight {
        let mut set = inflight.lock().unwrap();
        if set.contains(session_id) {
            return;
        }
        set.insert(session_id.to_string());
    }

    let release = |inflight: &Option<Inflight>, id: &str| {
        if let Some(inflight) = inflight {
            inflight.lock().unwrap().remove(id);
        }
    };

    // No runtime = nowhere to run the call; the honest answer is "no title".
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        release(&opts.inflight, session_id);
        return;
    };

    let input: String = text.chars().take(TITLE_MAX_INPUT).collect();
    // The project's own words, read HERE rather than inside the spawned task
    // so the database lock is taken on the caller's thread like every other
    // read in this function. A failure is an empty glossary, never a missing
    // title: this is a garnish on a garnish.
    let glossary = title_glossary(ctx, &session);
    let ctx = ctx.clone();
    let session_id = session_id.to_string();
    let placeholder = opts.placeholder.clone();
    let inflight = opts.inflight.clone();
    handle.spawn(async move {
        // `catch_unwind` on a method the type says cannot fail, because the
        // type is a contract this module cannot enforce on an injected
        // implementation — and a panic here is a missing title, not a
        // process-level event.
        let title = std::panic::AssertUnwindSafe(cheap.title(&input, &glossary))
            .catch_unwind()
            .await
            .ok()
            .flatten();
        if let Some(title) = title.filter(|t| !t.is_empty()) {
            // The SAME provisional rule as the entry guard — re-checked here
            // because the title may have been set while the call was in
            // flight; missing it here is why the first version of the `! `
            // fix did nothing.
            let updated = {
                let db = ctx.db.lock().unwrap();
                let now = db.get_session(&session_id).ok().flatten().map(|s| s.title);
                let still = match &now {
                    Some(t) => *t == placeholder || t.starts_with("! "),
                    None => false,
                };
                if still && db.set_session_title(&session_id, &title).is_ok() {
                    db.get_session(&session_id).ok().flatten()
                } else {
                    None
                }
            };
            if let Some(session) = updated {
                ctx.bus.publish(EventInput {
                    r#type: EventType::SessionUpdated,
                    session_id: Some(session_id.clone()),
                    data: serde_json::to_value(&session).unwrap_or_default(),
                });
            }
        }
        release(&inflight, &session_id);
    });
}

/// The user's words in a message, joined. Pure; empty when the message is
/// images only.
pub fn user_text(message: &Message) -> String {
    message
        .parts
        .iter()
        .filter_map(|p| match p {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Start auto-titling. Returns the unsubscribe (idempotent).
///
/// Listens for `message.started` with a `user` role — the event
/// `server/sessions` publishes the moment a posted message is persisted; the
/// role check excludes the turn runner's own supervisor messages. The
/// listener body is synchronous (the bus fans out synchronously — anything
/// slow here is latency on the publisher); all it does is start a task nobody
/// holds.
pub fn watch_titles(ctx: &TitleCtx) -> impl Fn() + Send + Sync {
    let inflight: Inflight = Arc::new(Mutex::new(HashSet::new()));
    let listener_ctx = ctx.clone();
    let bus = ctx.bus.clone();
    let id = bus.subscribe(Arc::new(move |e: &BoughEvent| {
        if e.r#type != EventType::MessageStarted {
            return;
        }
        let Some(session_id) = e.session_id.clone() else {
            return;
        };
        let Ok(message) = serde_json::from_value::<Message>(e.data.clone()) else {
            return;
        };
        if message.role != Role::User {
            return;
        }
        maybe_auto_title(
            &listener_ctx,
            &session_id,
            &user_text(&message),
            AutoTitleOpts {
                placeholder: String::new(),
                inflight: Some(inflight.clone()),
            },
        );
    }));
    let bus = ctx.bus.clone();
    move || bus.unsubscribe(id)
}

// ---------------------------------------------------------------------------
// Tests — ported from src/worker/titles.test.ts
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LlmParams, LlmResult, OnText};
    use crate::worker::test_support::{
        collect_events, hanging_client, saying_client, seed_session, test_title_ctx, GatedTier,
        StubTier,
    };
    use bough_llm::LlmError;
    use std::sync::atomic::Ordering;
    use tokio_util::sync::CancellationToken;

    /// Let the fire-and-forget tasks behind a publish run.
    async fn settle() {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    // ---- which model the tier runs on ---------------------------------------

    #[test]
    fn the_stored_pin_outranks_the_environment_and_clearing_it_falls_back() {
        // The pin is process-wide, so this test restores what it found. It is
        // the only test that touches it; `cheap_model_with` is what everything
        // else uses precisely because it cannot see the pin.
        let before = cheap_model_pin();

        set_cheap_model(None);
        assert_eq!(cheap_model_pin(), None);
        // Nothing pinned: the env answers, and the floor answers for the env.
        let empty: Env = Arc::new(|_| None);
        assert_eq!(cheap_model_with(&empty), DEFAULT_CHEAP_MODEL);

        set_cheap_model(Some("  openai:gpt-5-mini  ".into()));
        // Trimmed on the way in, so a picker that sent whitespace does not
        // store a model id nothing routes.
        assert_eq!(cheap_model(), "openai:gpt-5-mini");
        // …and it wins over `BOUGH_CHEAP_MODEL`, which is frozen at the
        // launching shell and could otherwise never be overridden.
        let with_env: Env = Arc::new(|k| (k == CHEAP_MODEL_ENV).then(|| "z-ai/glm-5.2".into()));
        assert_eq!(cheap_model_with(&with_env), "z-ai/glm-5.2");
        assert_eq!(cheap_model(), "openai:gpt-5-mini");

        // A blank pin is not a pin.
        set_cheap_model(Some("   ".into()));
        assert_eq!(cheap_model_pin(), None);

        set_cheap_model(before);
    }

    // ---- the shared call ----------------------------------------------------

    #[tokio::test]
    async fn cheap_text_returns_the_concatenated_text_of_a_successful_round() {
        let opts = CheapCallOpts {
            llm: Some(saying_client("  hello  ")),
            ..Default::default()
        };
        assert_eq!(
            cheap_text("s", "p", 16, &opts).await.as_deref(),
            Some("hello")
        );
    }

    #[tokio::test]
    async fn cheap_text_resolves_none_for_every_provider_failure_it_never_errors() {
        struct Failing;
        #[async_trait::async_trait]
        impl crate::types::LlmClient for Failing {
            async fn run(
                &self,
                _p: LlmParams,
                _t: OnText,
                _c: CancellationToken,
            ) -> Result<LlmResult, LlmError> {
                Err(LlmError::with("500 overloaded", 500, None))
            }
        }
        struct Empty;
        #[async_trait::async_trait]
        impl crate::types::LlmClient for Empty {
            async fn run(
                &self,
                _p: LlmParams,
                _t: OnText,
                _c: CancellationToken,
            ) -> Result<LlmResult, LlmError> {
                Ok(LlmResult {
                    content: vec![],
                    stop_reason: "end_turn".into(),
                    usage: None,
                })
            }
        }
        let clients: Vec<Arc<dyn crate::types::LlmClient>> =
            vec![Arc::new(Failing), Arc::new(Empty), saying_client("   ")];
        for llm in clients {
            let opts = CheapCallOpts {
                llm: Some(llm),
                ..Default::default()
            };
            assert_eq!(cheap_text("s", "p", 16, &opts).await, None);
        }
    }

    #[tokio::test]
    async fn cheap_text_abandons_a_hung_provider_at_its_deadline() {
        // The failure error-handling alone does not cover. Without this the
        // future never settles, and the activity watcher's one-slot-per-
        // session ledger would never be released.
        let started = std::time::Instant::now();
        let opts = CheapCallOpts {
            llm: Some(hanging_client()),
            timeout_ms: Some(20),
            ..Default::default()
        };
        assert_eq!(cheap_text("s", "p", 16, &opts).await, None);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the deadline, not the test runner, ended it"
        );
    }

    #[test]
    fn the_cheap_model_is_read_per_call_and_defaults_when_unset() {
        let none: Env = Arc::new(|_| None);
        let blank: Env = Arc::new(|_| Some("   ".into()));
        let set: Env =
            Arc::new(|k| (k == CHEAP_MODEL_ENV).then(|| "openai:gpt-5-mini".to_string()));
        assert_eq!(cheap_model_with(&none), DEFAULT_CHEAP_MODEL);
        assert_eq!(cheap_model_with(&blank), DEFAULT_CHEAP_MODEL);
        assert_eq!(cheap_model_with(&set), "openai:gpt-5-mini");
    }

    // ---- sanitizing ---------------------------------------------------------

    #[test]
    fn sanitize_title_refuses_a_title_that_carries_no_information() {
        assert_eq!(sanitize_title("1"), "");
        assert_eq!(sanitize_title("42"), "");
        assert_eq!(sanitize_title("-"), "");
        assert_eq!(sanitize_title("ok"), "");
        assert_eq!(sanitize_title("1 2 3"), "");
        // Three letters is the floor, and real short titles clear it.
        assert_eq!(sanitize_title("Bug"), "Bug");
        assert_eq!(sanitize_title("CI fix"), "CI fix");
        assert_eq!(sanitize_title("Fix cart pricing"), "Fix cart pricing");
    }

    #[test]
    fn sanitize_title_strips_the_label_the_quoting_and_the_trailing_period() {
        assert_eq!(
            sanitize_title("Title: \"Fix the patch parser.\""),
            "Fix the patch parser"
        );
        assert_eq!(
            sanitize_title("\n\n  rewrite the theme route  \n"),
            "rewrite the theme route"
        );
        assert_eq!(sanitize_title("**bold answer**"), "bold answer");
    }

    #[test]
    fn sanitize_title_caps_a_model_that_answered_the_message_instead_of_titling_it() {
        let prose = "Sure, I can help you with that — let me start by reading the file";
        assert_eq!(sanitize_title(prose), "");
        // Prose that is not a reply is still capped to a readable stub.
        assert_eq!(
            sanitize_title("Rewrite the theme route and then repaint every preview row")
                .split_whitespace()
                .count(),
            8
        );
        assert_eq!(
            sanitize_title("theme picker previews live"),
            "theme picker previews live"
        );
    }

    #[tokio::test]
    async fn cheap_title_is_none_for_empty_input_and_for_an_unusable_answer() {
        let x = CheapCallOpts {
            llm: Some(saying_client("x")),
            ..Default::default()
        };
        assert_eq!(cheap_title("   ", &[], &x).await, None);
        let quotes = CheapCallOpts {
            llm: Some(saying_client("\"\"")),
            ..Default::default()
        };
        assert_eq!(cheap_title("hello", &[], &quotes).await, None);
        let fix = CheapCallOpts {
            llm: Some(saying_client("Fix it")),
            ..Default::default()
        };
        assert_eq!(
            cheap_title("hello", &[], &fix).await.as_deref(),
            Some("Fix it")
        );
    }

    /// The nasal-decongestant bug. A project word reaches the titler, so the
    /// titler stops guessing at the nearest English word.
    #[test]
    fn the_glossary_tells_the_titler_the_projects_own_words() {
        let plain = title_system_with(&[]);
        assert_eq!(plain, TITLE_SYSTEM);
        let primed = title_system_with(&["nased".to_string(), "fmds".to_string()]);
        assert!(primed.starts_with(TITLE_SYSTEM), "{primed}");
        assert!(primed.contains("nased, fmds"), "{primed}");
        assert!(
            primed.contains("never expand it into what it sounds like"),
            "{primed}"
        );
    }

    /// A greeting buys no title — and anything that names work still does.
    #[test]
    fn a_bare_pleasantry_names_nothing_but_real_work_does() {
        for greeting in ["hi", "Hey!", "hello there", "good morning", "thanks", "ok"] {
            assert!(names_nothing(greeting), "{greeting:?} should defer");
        }
        for real in [
            "hi, fix the build",
            "tell me about nased",
            "hello world crashes on startup",
            "test the migration",
            "",
        ] {
            assert!(!names_nothing(real), "{real:?} should be titled");
        }
    }

    #[test]
    fn user_text_joins_the_text_parts_and_ignores_everything_else() {
        let message = Message {
            id: "m".into(),
            session_id: "s".into(),
            role: Role::User,
            pending: false,
            created_at: 0,
            parts: vec![
                Part::Text {
                    text: "look at".into(),
                },
                Part::Image {
                    path: "/x.png".into(),
                    media_type: "image/png".into(),
                    name: "x.png".into(),
                    size: 1,
                },
                Part::Text {
                    text: "this".into(),
                },
            ],
        };
        assert_eq!(user_text(&message), "look at\nthis");
    }

    // ---- auto-titling -------------------------------------------------------

    /// Publish the `message.started` a posted user message produces.
    fn post_user_message(ctx: &TitleCtx, session_id: &str, text: &str) {
        let message = Message {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            role: Role::User,
            parts: vec![Part::Text { text: text.into() }],
            pending: false,
            created_at: 1,
        };
        ctx.bus.publish(EventInput {
            r#type: EventType::MessageStarted,
            session_id: Some(session_id.to_string()),
            data: serde_json::to_value(&message).unwrap(),
        });
    }

    #[tokio::test]
    async fn a_posted_first_message_names_the_untitled_session_and_announces_it() {
        let ctx = test_title_ctx(Some(Arc::new(StubTier::title("fix the patch parser"))));
        let events = collect_events(&ctx.bus);
        let stop = watch_titles(&ctx);
        let session_id = seed_session(&ctx.db, "");
        post_user_message(&ctx, &session_id, "the patch parser drops the last line");
        settle().await;

        assert_eq!(
            ctx.db
                .lock()
                .unwrap()
                .get_session(&session_id)
                .unwrap()
                .unwrap()
                .title,
            "fix the patch parser"
        );
        let updated: Vec<_> = events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.r#type == EventType::SessionUpdated)
            .cloned()
            .collect();
        assert_eq!(
            updated.len(),
            1,
            "one session.updated re-renders every sidebar"
        );
        assert_eq!(updated[0].data["title"], "fix the patch parser");
        stop();
    }

    #[tokio::test]
    async fn a_session_that_already_has_a_title_is_never_re_titled_and_never_billed() {
        let tier = Arc::new(StubTier::title("generated"));
        let ctx = test_title_ctx(Some(tier.clone()));
        let stop = watch_titles(&ctx);
        let session_id = seed_session(&ctx.db, "the name I chose");
        post_user_message(&ctx, &session_id, "hello");
        settle().await;
        assert_eq!(tier.title_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            ctx.db
                .lock()
                .unwrap()
                .get_session(&session_id)
                .unwrap()
                .unwrap()
                .title,
            "the name I chose"
        );
        stop();
    }

    #[tokio::test]
    async fn a_shell_command_title_is_provisional_and_gets_replaced_by_a_real_one() {
        let ctx = test_title_ctx(Some(Arc::new(StubTier::title("Add a discount helper"))));
        let stop = watch_titles(&ctx);
        // The TUI writes this when `!command` creates the conversation.
        let provisional = seed_session(&ctx.db, "! ls -1 src");
        post_user_message(
            &ctx,
            &provisional,
            "Add a discount(items, pct) helper to src/cart.py",
        );
        settle().await;
        assert_eq!(
            ctx.db
                .lock()
                .unwrap()
                .get_session(&provisional)
                .unwrap()
                .unwrap()
                .title,
            "Add a discount helper"
        );

        // A title the user or the cheap tier already chose is left alone: the
        // provisional rule is only for the `! ` prefix the shell path writes.
        let chosen = seed_session(&ctx.db, "Pricing rewrite");
        post_user_message(&ctx, &chosen, "and now the shipping rules");
        settle().await;
        assert_eq!(
            ctx.db
                .lock()
                .unwrap()
                .get_session(&chosen)
                .unwrap()
                .unwrap()
                .title,
            "Pricing rewrite"
        );
        stop();
    }

    #[tokio::test]
    async fn a_rename_during_the_round_trip_is_not_clobbered_by_the_answer() {
        let tier = Arc::new(GatedTier::new());
        let ctx = test_title_ctx(Some(tier.clone()));
        let stop = watch_titles(&ctx);
        let session_id = seed_session(&ctx.db, "");
        post_user_message(&ctx, &session_id, "hello");
        settle().await;
        // The user renames while the cheap model is still thinking.
        ctx.db
            .lock()
            .unwrap()
            .set_session_title(&session_id, "mine")
            .unwrap();
        tier.release("the model's idea");
        settle().await;
        assert_eq!(
            ctx.db
                .lock()
                .unwrap()
                .get_session(&session_id)
                .unwrap()
                .unwrap()
                .title,
            "mine"
        );
        stop();
    }

    #[tokio::test]
    async fn two_messages_in_quick_succession_buy_exactly_one_title() {
        let tier = Arc::new(GatedTier::new());
        let ctx = test_title_ctx(Some(tier.clone()));
        let stop = watch_titles(&ctx);
        let session_id = seed_session(&ctx.db, "");
        post_user_message(&ctx, &session_id, "first");
        settle().await;
        post_user_message(&ctx, &session_id, "second");
        settle().await;
        assert_eq!(
            tier.calls.load(Ordering::SeqCst),
            1,
            "the second post rides the in-flight title, it does not buy one"
        );
        tier.release("one title");
        settle().await;
        assert_eq!(
            ctx.db
                .lock()
                .unwrap()
                .get_session(&session_id)
                .unwrap()
                .unwrap()
                .title,
            "one title"
        );
        stop();
    }

    #[tokio::test]
    async fn an_images_only_message_buys_no_title() {
        let tier = Arc::new(StubTier::title("nope"));
        let ctx = test_title_ctx(Some(tier.clone()));
        let session_id = seed_session(&ctx.db, "");
        maybe_auto_title(&ctx, &session_id, "   ", AutoTitleOpts::default());
        settle().await;
        assert_eq!(tier.title_calls.load(Ordering::SeqCst), 0);
    }

    // ---- failure is a non-event (the AC) ------------------------------------

    #[tokio::test]
    async fn a_panicking_cheap_tier_leaves_the_bus_and_the_session_untouched() {
        // A tier that violates its own contract, which is the worst case: the
        // type says these never fail, but an implementation is not bound by a
        // type.
        struct Panicking;
        #[async_trait::async_trait]
        impl CheapTier for Panicking {
            async fn title(&self, _f: &str, _glossary: &[String]) -> Option<String> {
                panic!("provider is down")
            }
            async fn ghost_text(&self, _p: &str) -> Option<String> {
                None
            }
            async fn activity(&self, _r: &str) -> Option<String> {
                None
            }
        }
        let ctx = test_title_ctx(Some(Arc::new(Panicking)));
        let events = collect_events(&ctx.bus);
        let stop = watch_titles(&ctx);
        // A listener registered AFTER the titler still receives the event.
        let seen = Arc::new(Mutex::new(Vec::<EventType>::new()));
        let sink = seen.clone();
        ctx.bus.subscribe(Arc::new(move |e: &BoughEvent| {
            sink.lock().unwrap().push(e.r#type)
        }));

        let session_id = seed_session(&ctx.db, "");
        post_user_message(&ctx, &session_id, "the patch parser drops the last line");
        settle().await;

        assert!(seen.lock().unwrap().contains(&EventType::MessageStarted));
        // The only consequence is the one the spec allows: the session keeps
        // its placeholder. Annoying, not broken.
        assert_eq!(
            ctx.db
                .lock()
                .unwrap()
                .get_session(&session_id)
                .unwrap()
                .unwrap()
                .title,
            ""
        );
        assert!(
            !events
                .lock()
                .unwrap()
                .iter()
                .any(|e| e.r#type == EventType::SessionUpdated),
            "no session.updated for a failed title"
        );
        stop();
    }

    #[tokio::test]
    async fn no_cheap_tier_at_all_is_a_working_server_not_a_degraded_one() {
        let ctx = test_title_ctx(None);
        let stop = watch_titles(&ctx);
        let session_id = seed_session(&ctx.db, "");
        post_user_message(&ctx, &session_id, "hello");
        settle().await;
        assert_eq!(
            ctx.db
                .lock()
                .unwrap()
                .get_session(&session_id)
                .unwrap()
                .unwrap()
                .title,
            ""
        );
        stop();
    }

    #[test]
    fn a_model_that_answered_instead_of_titling_yields_no_title_not_a_truncated_lie() {
        assert_eq!(
            sanitize_title("I don't have access to your codebase, so I can't say"),
            ""
        );
        for reply in [
            "I'll take a look at that for you",
            "Sure! Here is what that file does",
            "Sorry, I cannot help with that request",
            "Let me explain what the runner module does",
            "As an AI assistant I should note that",
            "Based on the code you have shared with me",
            "You asked about the turn runner and its",
        ] {
            assert_eq!(sanitize_title(reply), "", "should be refused: {reply}");
        }
        // A cap that would leave a dangling connective is trimmed too.
        assert_eq!(
            sanitize_title("Refactor the parser and the lexer and the rest"),
            "Refactor the parser and the lexer"
        );
    }

    #[test]
    fn a_real_title_still_passes_through_untouched() {
        assert_eq!(
            sanitize_title("Fix division by zero in calculator"),
            "Fix division by zero in calculator"
        );
        assert_eq!(
            sanitize_title("Title: Add retry to the LLM client"),
            "Add retry to the LLM client"
        );
        assert_eq!(
            sanitize_title("\"Wire up the changes rail\""),
            "Wire up the changes rail"
        );
        // "Interrupt" starts with I but is not the pronoun — the boundary holds.
        assert_eq!(
            sanitize_title("Interrupt handling for running turns"),
            "Interrupt handling for running turns"
        );
        assert_eq!(sanitize_title("Image input support"), "Image input support");
    }

    #[test]
    fn sanitize_title_lowers_the_models_title_case_so_the_tree_reads_as_one_column() {
        assert_eq!(
            sanitize_title("C Function Implementation"),
            "C function implementation"
        );
        assert_eq!(
            sanitize_title("Add b() function to mod.py"),
            "Add b() function to mod.py"
        );
        // Identifiers and acronyms carry their own capitalization.
        assert_eq!(sanitize_title("Fix CI Flake"), "Fix CI flake");
        // A lowercase-initial word means hands off the whole title.
        assert_eq!(
            sanitize_title("Rename getUser Everywhere"),
            "Rename getUser Everywhere"
        );
        // Already sentence case: untouched.
        assert_eq!(sanitize_title("Image input support"), "Image input support");
    }

    #[test]
    fn sanitize_title_drops_a_markdown_leader_the_model_decorated_with() {
        assert_eq!(
            sanitize_title("# Big Python File Creation"),
            "Big python file creation"
        );
        assert_eq!(sanitize_title("### Fix the parser"), "Fix the parser");
        assert_eq!(sanitize_title("- Add retry logic"), "Add retry logic");
        // A `#` that is part of the name, not a leader, stays put.
        assert_eq!(
            sanitize_title("Fix #412 in the parser"),
            "Fix #412 in the parser"
        );
    }
}
