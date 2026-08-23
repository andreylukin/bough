//! The rolling summary: a session's `title` and `description`, rewritten by
//! the cheap tier as the session's LOG grows. The fourth cheap-tier feature.
//!
//! WHAT IT FIXES. The titler names a session after its FIRST message, once;
//! "Fix this issue" is what a session is called for the rest of its life even
//! when it became the deicing-consumer PR. The activity blurb says what the
//! current program is doing and is gone when it ends. Neither answers the
//! question the session list exists for — *what is this one, and where did it
//! get to?* — so this module keeps one line per session that does, and lets
//! the title follow the work.
//!
//! THE INPUT IS THE LOG, NOT THE TRANSCRIPT. The model reads the session's
//! milestones (`hostfn/milestone`) and the user's own messages — what landed
//! and what was asked — and nothing a tool printed. A summary built from tool
//! output narrates commands; one built from milestones narrates actions,
//! which is the altitude a description should sit at. Prior summaries feed
//! the next one (`description` is an input), so the line is rolling, not
//! recomputed.
//!
//! THE INVARIANTS, shared with the rest of the tier: **a cheap call can only
//! ADD**. Every failure is a missing summary and the next turn gets another
//! chance. **One in-flight per session, drop rather than queue**, as
//! `activity.rs`: a burst of short turns must not fan out into a burst of
//! calls. **It never takes a title the user chose.** The only titles it may
//! replace are the ones machines wrote — the placeholder, a `! command`
//! name, or a title the titler or this worker wrote earlier in this process
//! (recorded in [`AUTO_TITLES`]; there is no provenance column, and a
//! process-local ledger is enough because a restart only ever makes the
//! worker MORE conservative).
//!
//! WHEN. On `turn.finished`, and only when enough happened: [`should_run`]
//! is the whole trigger, kept pure so it is testable without a bus.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};

use futures::FutureExt;

use crate::bus::Bus;
use crate::schema::events::{BoughEvent, EventInput, EventType, TurnFinishedData};
use crate::schema::parts::{Milestone, Part, Role};
use crate::types::{CheapTier, SharedDb};
use crate::worker::titles::{cheap_text, sanitize_title, CheapCallOpts};

// ---------------------------------------------------------------------------
// The trigger (pure)
// ---------------------------------------------------------------------------

/// Seconds between two summaries of one session, at the least.
pub const MIN_INTERVAL_MS: i64 = 180_000;

/// What the watcher remembers per session between runs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mark {
    /// When the last summary finished (0 = never).
    pub at: i64,
    /// How many milestones / user messages the session had then.
    pub milestones: usize,
    pub user_messages: usize,
}

/// Is a summary worth a call right now? The rule, in full:
///
/// - first ever: once there is ≥1 milestone or ≥2 user messages;
/// - after that: ≥2 new milestones OR ≥2 new user messages since the mark,
///   AND at least [`MIN_INTERVAL_MS`] since it.
pub fn should_run(mark: &Mark, milestones: usize, user_messages: usize, now: i64) -> bool {
    if mark.at == 0 {
        return milestones >= 1 || user_messages >= 2;
    }
    let grown = milestones.saturating_sub(mark.milestones) >= 2
        || user_messages.saturating_sub(mark.user_messages) >= 2;
    grown && now - mark.at >= MIN_INTERVAL_MS
}

// ---------------------------------------------------------------------------
// Prompt shaping (pure)
// ---------------------------------------------------------------------------

pub const SUMMARY_SYSTEM: &str = "You keep a one-line summary of a coding session for the \
     engineer running it. You get the session's current title and summary, the engineer's \
     own messages, and the session's LOG — the milestones the session recorded when an \
     overarching action landed. Reply with ONLY this JSON, nothing else:\n\
     {\"title\": \"3-7 words, sentence case, naming what the session is FOR and its current \
     thrust; return the current title VERBATIM if it is still right\", \"description\": \
     \"<=25 words: what this session is doing and where it stands — overarching actions \
     (a PR opened, a fix landed, a decision), never commands or files read\", \"state\": \
     \"working|blocked|waiting|done\"}\n\
     Name only what the log and the messages name — never invent a subject, file, or \
     outcome. Keep ticket keys and PR numbers as written.";

/// The user's words since the session began, latest-biased and clipped.
pub const USER_TEXT_MAX: usize = 1_500;
/// The most recent milestones the model sees.
pub const MILESTONES_MAX: usize = 40;
pub const SUMMARY_MAX_TOKENS: i64 = 200;

pub fn build_prompt(
    title: &str,
    description: &str,
    user_messages: &[String],
    milestones: &[Milestone],
) -> String {
    // Latest-biased: drop from the FRONT until the user's words fit.
    let mut kept: Vec<&str> = user_messages.iter().map(String::as_str).collect();
    let total = |v: &[&str]| v.iter().map(|m| m.chars().count() + 1).sum::<usize>();
    while kept.len() > 1 && total(&kept) > USER_TEXT_MAX {
        kept.remove(0);
    }
    let users: Vec<String> = kept
        .iter()
        .map(|m| {
            let clipped: String = m.chars().take(USER_TEXT_MAX).collect();
            format!("- {}", clipped.split_whitespace().collect::<Vec<_>>().join(" "))
        })
        .collect();
    let start = milestones.len().saturating_sub(MILESTONES_MAX);
    let log: Vec<String> = milestones[start..]
        .iter()
        .map(|m| format!("- {}", m.text))
        .collect();
    format!(
        "Current title: {}\nCurrent summary: {}\n\nEngineer's messages:\n{}\n\nLog:\n{}",
        if title.is_empty() { "(none)" } else { title },
        if description.is_empty() { "(none yet)" } else { description },
        if users.is_empty() { "(none)".into() } else { users.join("\n") },
        if log.is_empty() { "(nothing logged yet)".into() } else { log.join("\n") },
    )
}

/// What the model answered, once parsed and cleaned.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub title: String,
    pub description: String,
    pub state: String,
}

/// Parse the cheap model's answer. Tolerates prose around the object and a
/// fenced block; `None` when there is no object or no description in it —
/// a summary without a description is the one thing this exists to produce.
pub fn parse_summary(text: &str) -> Option<Summary> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end < start {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&text[start..=end]).ok()?;
    let description = v
        .get("description")
        .and_then(|d| d.as_str())
        .map(|d| d.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|d| !d.is_empty())?;
    let title = v
        .get("title")
        .and_then(|t| t.as_str())
        .map(sanitize_title)
        .unwrap_or_default();
    let state = v
        .get("state")
        .and_then(|s| s.as_str())
        .map(|s| s.trim().to_lowercase())
        .filter(|s| matches!(s.as_str(), "working" | "blocked" | "waiting" | "done"))
        .unwrap_or_else(|| "working".into());
    Some(Summary {
        title,
        description: clip_words(&description, 40),
        state,
    })
}

fn clip_words(s: &str, max: usize) -> String {
    let words: Vec<&str> = s.split_whitespace().collect();
    if words.len() <= max {
        return words.join(" ");
    }
    format!("{}…", words[..max].join(" "))
}

// ---------------------------------------------------------------------------
// Title provenance (process-local)
// ---------------------------------------------------------------------------

/// Titles machines wrote, per session, in this process. The titler and this
/// worker both record here; the worker may replace exactly these (plus the
/// placeholder and `! command` names) and nothing else.
static AUTO_TITLES: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Record that `title` on `session_id` was written by a machine.
pub fn note_auto_title(session_id: &str, title: &str) {
    if let Ok(mut m) = AUTO_TITLES.lock() {
        m.insert(session_id.to_string(), title.to_string());
    }
}

/// May the worker replace `current`? True for the placeholder, a `! command`
/// name, or a title a machine wrote earlier in this process.
pub fn title_is_replaceable(session_id: &str, current: &str) -> bool {
    if current.trim().is_empty() || current.starts_with("! ") {
        return true;
    }
    AUTO_TITLES
        .lock()
        .map(|m| m.get(session_id).map(|t| t == current).unwrap_or(false))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// The call
// ---------------------------------------------------------------------------

/// The production call, behind `CheapTier::summary`.
pub async fn cheap_summary(prompt: &str, opts: &CheapCallOpts) -> Option<String> {
    cheap_text(SUMMARY_SYSTEM, prompt, SUMMARY_MAX_TOKENS, opts).await
}

// ---------------------------------------------------------------------------
// The watcher
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct SummaryCtx {
    pub db: SharedDb,
    pub bus: Arc<Bus>,
    pub cheap: Option<Arc<dyn CheapTier>>,
    pub now: crate::types::Clock,
}

type Marks = Arc<Mutex<HashMap<String, Mark>>>;
type Inflight = Arc<Mutex<HashSet<String>>>;

/// The user's messages as text, in order.
fn user_messages(db: &SharedDb, session_id: &str) -> Vec<String> {
    let guard = db.lock().unwrap();
    guard
        .messages_for(session_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|m| m.role == Role::User)
        .map(|m| {
            m.parts
                .iter()
                .filter_map(|p| match p {
                    Part::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|t| !t.trim().is_empty())
        .collect()
}

/// One pass for one session: decide, call, write, announce. Public so a test
/// can drive it without the bus; the watcher is this behind `turn.finished`.
pub fn maybe_summarize(ctx: &SummaryCtx, session_id: &str, marks: &Marks, inflight: &Inflight) {
    let Some(cheap) = ctx.cheap.clone() else {
        return;
    };
    let milestones = {
        let db = ctx.db.lock().unwrap();
        db.milestones(session_id).unwrap_or_default()
    };
    let users = user_messages(&ctx.db, session_id);
    let now = (ctx.now)();
    {
        let m = marks.lock().unwrap();
        let mark = m.get(session_id).cloned().unwrap_or_default();
        if !should_run(&mark, milestones.len(), users.len(), now) {
            return;
        }
    }
    {
        let mut set = inflight.lock().unwrap();
        if !set.insert(session_id.to_string()) {
            return;
        }
    }
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        inflight.lock().unwrap().remove(session_id);
        return;
    };
    let (title, description) = {
        let db = ctx.db.lock().unwrap();
        match db.get_session(session_id).ok().flatten() {
            Some(s) => (s.title, s.description.unwrap_or_default()),
            None => {
                inflight.lock().unwrap().remove(session_id);
                return;
            }
        }
    };
    let prompt = build_prompt(&title, &description, &users, &milestones);
    let ctx = ctx.clone();
    let marks = marks.clone();
    let inflight = inflight.clone();
    let session_id = session_id.to_string();
    let counts = (milestones.len(), users.len());
    handle.spawn(async move {
        let answer = std::panic::AssertUnwindSafe(cheap.summary(&prompt))
            .catch_unwind()
            .await
            .ok()
            .flatten();
        if let Some(summary) = answer.as_deref().and_then(parse_summary) {
            let updated = {
                let db = ctx.db.lock().unwrap();
                let mut wrote = db.set_session_description(&session_id, &summary.description).is_ok();
                if !summary.title.is_empty() && summary.title != title {
                    // Re-read: the user may have renamed it while the call was out.
                    let current = db
                        .get_session(&session_id)
                        .ok()
                        .flatten()
                        .map(|s| s.title)
                        .unwrap_or_default();
                    if title_is_replaceable(&session_id, &current)
                        && db.set_session_title(&session_id, &summary.title).is_ok()
                    {
                        note_auto_title(&session_id, &summary.title);
                        wrote = true;
                    }
                }
                if wrote {
                    db.get_session(&session_id).ok().flatten()
                } else {
                    None
                }
            };
            // The mark moves only on success: a failed call leaves the next
            // turn free to try again immediately.
            marks.lock().unwrap().insert(
                session_id.clone(),
                Mark {
                    at: (ctx.now)(),
                    milestones: counts.0,
                    user_messages: counts.1,
                },
            );
            if let Some(session) = updated {
                ctx.bus.publish(EventInput {
                    r#type: EventType::SessionUpdated,
                    session_id: Some(session_id.clone()),
                    data: serde_json::to_value(&session).unwrap_or_default(),
                });
            }
        }
        inflight.lock().unwrap().remove(&session_id);
    });
}

/// Subscribe the summary to `turn.finished`. Returns the unsubscribe thunk.
pub fn watch_summaries(ctx: &SummaryCtx) -> impl Fn() + Send + Sync {
    let marks: Marks = Arc::new(Mutex::new(HashMap::new()));
    let inflight: Inflight = Arc::new(Mutex::new(HashSet::new()));
    let listener_ctx = ctx.clone();
    let bus = ctx.bus.clone();
    let id = bus.subscribe(Arc::new(move |e: &BoughEvent| {
        if e.r#type != EventType::TurnFinished {
            return;
        }
        let Ok(d) = serde_json::from_value::<TurnFinishedData>(e.data.clone()) else {
            return;
        };
        maybe_summarize(&listener_ctx, &d.session_id, &marks, &inflight);
    }));
    let bus = ctx.bus.clone();
    move || bus.unsubscribe(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_summary_waits_for_one_milestone_or_two_messages() {
        let m = Mark::default();
        assert!(!should_run(&m, 0, 1, 10));
        assert!(should_run(&m, 1, 0, 10));
        assert!(should_run(&m, 0, 2, 10));
    }

    #[test]
    fn later_summaries_need_growth_and_the_interval() {
        let m = Mark {
            at: 1_000,
            milestones: 3,
            user_messages: 2,
        };
        // grown, too soon
        assert!(!should_run(&m, 5, 2, 1_000 + MIN_INTERVAL_MS - 1));
        // old enough, not grown
        assert!(!should_run(&m, 4, 3, 1_000 + MIN_INTERVAL_MS));
        // both
        assert!(should_run(&m, 5, 2, 1_000 + MIN_INTERVAL_MS));
        assert!(should_run(&m, 3, 4, 1_000 + MIN_INTERVAL_MS));
    }

    #[test]
    fn parse_tolerates_prose_and_fences_and_requires_a_description() {
        let s = parse_summary(
            "Sure:\n```json\n{\"title\": \"Title: Fix the deicing consumer\", \"description\": \"  Opened  #34; waiting on review \", \"state\": \"Waiting\"}\n```",
        )
        .unwrap();
        assert_eq!(s.title, "Fix the deicing consumer");
        assert_eq!(s.description, "Opened #34; waiting on review");
        assert_eq!(s.state, "waiting");
        assert!(parse_summary("{\"title\": \"x\"}").is_none());
        assert!(parse_summary("no json here").is_none());
        assert_eq!(parse_summary("{\"description\": \"d\", \"state\": \"odd\"}").unwrap().state, "working");
    }

    #[test]
    fn the_prompt_is_latest_biased_and_bounded() {
        let users: Vec<String> = (0..10).map(|i| format!("{i} {}", "w".repeat(400))).collect();
        let log: Vec<Milestone> = (0..60)
            .map(|i| Milestone {
                ts: i,
                text: format!("m{i}"),
            })
            .collect();
        let p = build_prompt("T", "", &users, &log);
        assert!(!p.contains("- 0 w"), "oldest message dropped");
        assert!(p.contains("- 9 w"), "latest message kept");
        assert!(!p.contains("- m0\n"), "oldest milestones dropped");
        assert!(p.contains("- m59"));
        assert!(p.contains("(none yet)"));
    }

    struct SummaryTier {
        answer: Option<String>,
        calls: std::sync::atomic::AtomicUsize,
    }
    #[async_trait::async_trait]
    impl CheapTier for SummaryTier {
        async fn title(&self, _f: &str, _g: &[String]) -> Option<String> {
            None
        }
        async fn ghost_text(&self, _p: &str) -> Option<String> {
            None
        }
        async fn activity(&self, _r: &str) -> Option<String> {
            None
        }
        async fn summary(&self, _p: &str) -> Option<String> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.answer.clone()
        }
    }

    fn ctx_with(tier: Arc<SummaryTier>, now: i64) -> SummaryCtx {
        let db = crate::worker::test_support::test_db();
        let bus = Arc::new(Bus::new(crate::types::system_clock()));
        SummaryCtx {
            db,
            bus,
            cheap: Some(tier),
            now: Arc::new(move || now),
        }
    }

    fn finish_turn(ctx: &SummaryCtx, session_id: &str) {
        ctx.bus.publish(EventInput {
            r#type: EventType::TurnFinished,
            session_id: Some(session_id.into()),
            data: serde_json::to_value(TurnFinishedData {
                turn_id: "t1".into(),
                session_id: session_id.into(),
                status: crate::schema::parts::TurnStatus::Done,
                error: None,
            })
            .unwrap(),
        });
    }

    #[tokio::test]
    async fn a_finished_turn_with_a_milestone_writes_description_and_retitles_a_machine_title() {
        let tier = Arc::new(SummaryTier {
            answer: Some(
                "{\"title\": \"Deicing consumer PR\", \"description\": \"Opened #34; review pending\", \"state\": \"waiting\"}".into(),
            ),
            calls: Default::default(),
        });
        let ctx = ctx_with(tier.clone(), 10_000);
        let session_id = crate::worker::test_support::seed_session(&ctx.db, "Fix this issue");
        note_auto_title(&session_id, "Fix this issue");
        let events = crate::worker::test_support::collect_events(&ctx.bus);
        let stop = watch_summaries(&ctx);
        ctx.db.lock().unwrap().add_milestone(&session_id, 1, "Opened #34").unwrap();
        finish_turn(&ctx, &session_id);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let s = ctx.db.lock().unwrap().get_session(&session_id).unwrap().unwrap();
        assert_eq!(s.description.as_deref(), Some("Opened #34; review pending"));
        assert_eq!(s.title, "Deicing consumer PR");
        assert!(events
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.r#type == EventType::SessionUpdated && e.data["description"] == "Opened #34; review pending"));
        // A second turn right away: nothing new and too soon → no call.
        finish_turn(&ctx, &session_id);
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert_eq!(tier.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        stop();
    }

    #[tokio::test]
    async fn a_user_chosen_title_survives_the_summary() {
        let tier = Arc::new(SummaryTier {
            answer: Some("{\"title\": \"Something else\", \"description\": \"d\"}".into()),
            calls: Default::default(),
        });
        let ctx = ctx_with(tier, 10_000);
        let session_id = crate::worker::test_support::seed_session(&ctx.db, "the name I chose");
        let stop = watch_summaries(&ctx);
        ctx.db.lock().unwrap().add_milestone(&session_id, 1, "x").unwrap();
        finish_turn(&ctx, &session_id);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let s = ctx.db.lock().unwrap().get_session(&session_id).unwrap().unwrap();
        assert_eq!(s.title, "the name I chose");
        assert_eq!(s.description.as_deref(), Some("d"));
        stop();
    }

    #[tokio::test]
    async fn a_quiet_session_costs_nothing() {
        let tier = Arc::new(SummaryTier {
            answer: Some("{\"description\": \"d\"}".into()),
            calls: Default::default(),
        });
        let ctx = ctx_with(tier.clone(), 10_000);
        let session_id = crate::worker::test_support::seed_session(&ctx.db, "");
        let stop = watch_summaries(&ctx);
        finish_turn(&ctx, &session_id);
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert_eq!(tier.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        stop();
    }

    #[test]
    fn only_machine_written_titles_are_replaceable() {
        assert!(title_is_replaceable("s", ""));
        assert!(title_is_replaceable("s", "! ls -la"));
        assert!(!title_is_replaceable("s", "my own name"));
        note_auto_title("s", "Auto name");
        assert!(title_is_replaceable("s", "Auto name"));
        assert!(!title_is_replaceable("s", "Auto name edited"));
    }
}
