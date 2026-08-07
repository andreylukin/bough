//! Live activity blurbs: a present-tense one-liner describing what a session
//! is doing right now — "running the test suite", "rewriting the patch
//! parser" — derived from the `run_steps` program as it goes by (port of
//! `src/worker/activity.ts`). The third of the cheap tier's three features.
//!
//! THE INVARIANT THIS HOLDS: **one in-flight blurb per session — rounds that
//! land while it is busy are DROPPED, not queued.** A blurb describes the
//! round it was generated from: a queue makes the tier's cost scale with the
//! session's round rate rather than with its own latency, and displays each
//! blurb minutes late, narrating a program that finished long ago. Dropping
//! bounds the spend at one call per session at a time AND keeps every blurb
//! that does appear true of something recent, because the next round
//! describes itself.
//!
//! SECOND INVARIANT: **nothing here persists.** Blurbs are ephemeral
//! `session.activity` events — no table, no column, no cache. A reconnecting
//! client has none until the next round, which is correct: a stale "running
//! the test suite" restored from a database would be a claim about a process
//! that is not running.
//!
//! WHY IT IS A BUS LISTENER, wired only at boot: the blurb is a function of
//! an event that is already published, so subscribing costs the turn runner
//! nothing and a turn test that builds its own ctx never gets one.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use futures::FutureExt;

use crate::bus::Bus;
use crate::schema::events::{BoughEvent, EventInput, EventType, SessionActivityData};
use crate::schema::parts::Part;
use crate::types::CheapTier;
use crate::worker::titles::{cheap_text, CheapCallOpts};

// ---------------------------------------------------------------------------
// Prompt shaping (pure)
// ---------------------------------------------------------------------------

pub const ACTIVITY_SYSTEM: &str =
    "You describe what a coding agent is doing, for a live status line. Given the \
     JavaScript program it is about to run, reply with one present-participle phrase \
     of at most six words — 'running the test suite', 'rewriting the patch parser'. \
     No quotes, no trailing period, no preamble, no code.";

/// The longest blurb a one-line status is asked to render.
pub const MAX_BLURB: usize = 60;

/// How much of the program the model is shown. The head says what it is going
/// to do.
pub const MAX_CODE_CHARS: usize = 1500;

/// The program as prompt text. Truncated from the HEAD, unlike ghost text's
/// tail-keeping and for the opposite reason: a program's opening lines are
/// its intent, and the last 1,500 characters of a long one are usually output
/// formatting.
pub fn program_gist(code: &str) -> String {
    let chars: Vec<char> = code.chars().collect();
    let head = if chars.len() > MAX_CODE_CHARS {
        format!("{}\n…", chars[..MAX_CODE_CHARS].iter().collect::<String>())
    } else {
        code.to_string()
    };
    format!("The program:\n{head}\n\nWhat is it doing?")
}

/// First real line, unquoted, capped, trailing period dropped; `None` if
/// unusable.
pub fn sanitize_blurb(raw: &str) -> Option<String> {
    static QUOTES_LEAD: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new("^[\"'`]+").unwrap());
    static QUOTES_TRAIL: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new("[\"'`.]+$").unwrap());
    let line = raw
        .trim()
        .split('\n')
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let clean = QUOTES_LEAD.replace(line, "");
    let clean = QUOTES_TRAIL.replace(&clean, "");
    let clean: String = clean.chars().take(MAX_BLURB).collect();
    let clean = clean.trim();
    if clean.is_empty() {
        None
    } else {
        Some(clean.to_string())
    }
}

// ---------------------------------------------------------------------------
// The cheap-tier method
// ---------------------------------------------------------------------------

/// `CheapTier::activity`. Resolves the sanitized blurb, or `None` — never
/// errors.
///
/// `max_tokens` is 32, the smallest of the three: six words is the whole
/// answer, and a cap this tight is also the cheapest guard against a model
/// that decides to explain the program instead of naming it.
pub async fn cheap_activity(recent: &str, opts: &CheapCallOpts) -> Option<String> {
    if recent.trim().is_empty() {
        return None;
    }
    let raw = cheap_text(ACTIVITY_SYSTEM, recent, 32, opts).await?;
    sanitize_blurb(&raw)
}

// ---------------------------------------------------------------------------
// The watcher
// ---------------------------------------------------------------------------

/// The `run_steps` code in a part, or `None` for every other part. Pure.
pub fn program_of(part: Option<&Part>) -> Option<String> {
    let Some(Part::ToolCall { name, input, .. }) = part else {
        return None;
    };
    if name != "run_steps" {
        return None;
    }
    match input.get("code").and_then(|v| v.as_str()) {
        Some(code) if !code.trim().is_empty() => Some(code.to_string()),
        _ => None,
    }
}

/// What the watcher needs off the app context. `cheap` absent = the feature
/// is off.
#[derive(Clone)]
pub struct ActivityCtx {
    pub bus: Arc<Bus>,
    pub cheap: Option<Arc<dyn CheapTier>>,
}

/// Start publishing activity blurbs. Returns the unsubscribe (idempotent).
///
/// Two triggers and one ledger:
///
///   - a `message.part` carrying a `run_steps` call starts a blurb, UNLESS
///     this session already has one in flight, in which case the round is
///     dropped;
///   - `turn.finished` clears the session's blurb (`activity: null`), because
///     a status line that keeps claiming work after the turn ended is worse
///     than an empty one.
///
/// The listener body is synchronous and does no I/O: the bus fans out
/// synchronously, so anything slow here would be latency charged to whoever
/// published — which for `message.part` is the turn runner, mid-stream. All
/// it does is start a task nobody holds.
///
/// The clear is deferred to a spawned task so the `session.activity` event
/// cannot be stamped and delivered from INSIDE the `turn.finished` fan-out,
/// which would put it ahead of the event that caused it for every subscriber
/// registered after this one.
pub fn watch_activity(ctx: &ActivityCtx) -> impl Fn() + Send + Sync {
    // Sessions with a call in flight. Membership IS the drop rule.
    let inflight: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    // Bumped whenever a session's turn ends. A blurb carries the value it
    // started at, so an answer that arrives after its turn finished is
    // discarded instead of repainting a status line for work that is over.
    let epoch: Arc<Mutex<HashMap<String, u64>>> = Arc::new(Mutex::new(HashMap::new()));

    let cheap_opt = ctx.cheap.clone();
    let bus_for_listener = ctx.bus.clone();
    let id = ctx.bus.subscribe(Arc::new(move |e: &BoughEvent| {
        let Some(cheap) = cheap_opt.clone() else {
            return;
        };
        let Some(session_id) = e.session_id.clone() else {
            return;
        };

        if e.r#type == EventType::TurnFinished {
            *epoch.lock().unwrap().entry(session_id.clone()).or_insert(0) += 1;
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let bus = bus_for_listener.clone();
                handle.spawn(async move {
                    bus.publish(activity_event(&session_id, None));
                });
            }
            return;
        }

        if e.r#type != EventType::MessagePart {
            return;
        }
        let part = e
            .data
            .get("part")
            .and_then(|v| serde_json::from_value::<Part>(v.clone()).ok());
        let Some(code) = program_of(part.as_ref()) else {
            return;
        };

        // THE DROP RULE. Not a queue, not a debounce, and not a replacement
        // of the pending call: the round is simply not described, and the
        // next one will describe itself.
        {
            let mut set = inflight.lock().unwrap();
            if set.contains(&session_id) {
                return;
            }
            set.insert(session_id.clone());
        }
        let started = *epoch.lock().unwrap().get(&session_id).unwrap_or(&0);

        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            inflight.lock().unwrap().remove(&session_id);
            return;
        };
        let bus = bus_for_listener.clone();
        let inflight = inflight.clone();
        let epoch = epoch.clone();
        handle.spawn(async move {
            // `catch_unwind` on a method the type says cannot fail, because
            // an injected implementation is not bound by the type and a panic
            // is a missing blurb, not a broken watcher.
            let activity = std::panic::AssertUnwindSafe(cheap.activity(&program_gist(&code)))
                .catch_unwind()
                .await
                .ok()
                .flatten();
            if let Some(activity) = activity {
                let current = *epoch.lock().unwrap().get(&session_id).unwrap_or(&0);
                if current == started {
                    bus.publish(activity_event(&session_id, Some(activity)));
                }
            }
            // Released on the SAME watcher on every path — a failure must not
            // silence the session forever.
            inflight.lock().unwrap().remove(&session_id);
        });
    }));
    let bus = ctx.bus.clone();
    move || bus.unsubscribe(id)
}

fn activity_event(session_id: &str, activity: Option<String>) -> EventInput {
    EventInput {
        r#type: EventType::SessionActivity,
        session_id: Some(session_id.to_string()),
        // Only the blurb slot: a cheap-tier answer says nothing about which
        // command the program is blocked on, so `command` is left absent and
        // whatever `hostfn::shell` last published survives.
        data: serde_json::to_value(SessionActivityData {
            session_id: session_id.to_string(),
            activity: Some(activity),
            command: None,
        })
        .unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// Tests — ported from src/worker/activity.test.ts
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::system_clock;
    use crate::worker::test_support::{GatedTier, StubTier};
    use serde_json::json;
    use std::sync::atomic::Ordering;

    async fn settle() {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    /// The event the turn runner publishes when a `run_steps` call finalizes.
    fn run_steps(session_id: &str, code: &str) -> EventInput {
        EventInput {
            r#type: EventType::MessagePart,
            session_id: Some(session_id.to_string()),
            data: json!({
                "messageId": "m1",
                "part": { "type": "tool_call", "id": "t1", "name": "run_steps", "input": { "code": code } },
            }),
        }
    }

    /// `turn.finished` for a session — the watcher's clear trigger.
    fn turn_finished(session_id: &str) -> EventInput {
        EventInput {
            r#type: EventType::TurnFinished,
            session_id: Some(session_id.to_string()),
            data: json!({ "turnId": "t1", "sessionId": session_id, "status": "done" }),
        }
    }

    struct Rig {
        bus: Arc<Bus>,
        events: Arc<Mutex<Vec<BoughEvent>>>,
        stop: Box<dyn Fn() + Send + Sync>,
    }

    impl Rig {
        fn activities(&self) -> Vec<SessionActivityData> {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter(|e| e.r#type == EventType::SessionActivity)
                .map(|e| serde_json::from_value(e.data.clone()).unwrap())
                .collect()
        }
    }

    fn rig(cheap: Option<Arc<dyn CheapTier>>) -> Rig {
        let bus = Arc::new(Bus::new(system_clock()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        bus.subscribe(Arc::new(move |e: &BoughEvent| {
            sink.lock().unwrap().push(e.clone())
        }));
        let stop = watch_activity(&ActivityCtx {
            bus: bus.clone(),
            cheap,
        });
        Rig {
            bus,
            events,
            stop: Box::new(stop),
        }
    }

    fn sad(session_id: &str, activity: Option<&str>) -> SessionActivityData {
        SessionActivityData {
            session_id: session_id.to_string(),
            activity: Some(activity.map(String::from)),
            command: None,
        }
    }

    // ---- shaping (pure) -----------------------------------------------------

    #[test]
    fn program_of_picks_out_run_steps_code_and_nothing_else() {
        let call = |name: &str, input: serde_json::Value| Part::ToolCall {
            id: "t".into(),
            name: name.into(),
            input,
        };
        assert_eq!(
            program_of(Some(&call(
                "run_steps",
                json!({"code": "await bash('ls')"})
            ))),
            Some("await bash('ls')".to_string())
        );
        // `stop` is the other tool the model sees, and it describes nothing.
        assert_eq!(program_of(Some(&call("stop", json!({})))), None);
        assert_eq!(
            program_of(Some(&Part::Text {
                text: "hello".into()
            })),
            None
        );
        assert_eq!(program_of(Some(&call("run_steps", json!({})))), None);
        assert_eq!(
            program_of(Some(&call("run_steps", json!({"code": "   "})))),
            None
        );
        assert_eq!(program_of(None), None);
    }

    #[test]
    fn program_gist_truncates_from_the_head_the_opening_lines_are_the_intent() {
        let code = format!(
            "// INTENT\n{}\n// FORMATTING",
            "x".repeat(MAX_CODE_CHARS + 500)
        );
        let gist = program_gist(&code);
        assert!(gist.contains("// INTENT"));
        assert!(!gist.contains("// FORMATTING"));
        assert!(gist.ends_with("What is it doing?"));
    }

    #[test]
    fn sanitize_blurb_takes_the_first_line_unquoted_and_uncapitalized_period() {
        assert_eq!(
            sanitize_blurb("\"running the test suite.\"").as_deref(),
            Some("running the test suite")
        );
        assert_eq!(
            sanitize_blurb("\n\nrewriting the patch parser\nthen running tests").as_deref(),
            Some("rewriting the patch parser")
        );
        assert_eq!(sanitize_blurb("  "), None);
        let long = "y".repeat(MAX_BLURB + 40);
        assert_eq!(sanitize_blurb(&long).unwrap().chars().count(), MAX_BLURB);
    }

    #[tokio::test]
    async fn cheap_activity_is_none_for_empty_input_without_calling_anything() {
        struct MustNotBeCalled;
        #[async_trait::async_trait]
        impl crate::types::LlmClient for MustNotBeCalled {
            async fn run(
                &self,
                _p: crate::types::LlmParams,
                _t: crate::types::OnText,
                _c: tokio_util::sync::CancellationToken,
            ) -> Result<crate::types::LlmResult, crate::errors::BoughError> {
                panic!("must not be called")
            }
        }
        let opts = CheapCallOpts {
            llm: Some(std::sync::Arc::new(MustNotBeCalled)),
            ..Default::default()
        };
        assert_eq!(cheap_activity("   ", &opts).await, None);
    }

    // ---- the watcher --------------------------------------------------------

    #[tokio::test]
    async fn a_run_steps_round_publishes_one_session_activity_blurb() {
        let r = rig(Some(Arc::new(StubTier::activity("running the test suite"))));
        r.bus.publish(run_steps("s1", "await bash('deno test')"));
        settle().await;
        assert_eq!(
            r.activities(),
            vec![sad("s1", Some("running the test suite"))]
        );
        (r.stop)();
    }

    #[tokio::test]
    async fn the_drop_rule_a_burst_of_12_rounds_on_one_session_buys_exactly_one_call() {
        let tier = Arc::new(GatedTier::new());
        let r = rig(Some(tier.clone()));
        // The burst lands while the first call is still open. A tier that
        // resolved immediately would find the slot free every time and pass
        // with no rule at all.
        for i in 0..12 {
            r.bus.publish(run_steps("s1", &format!("round {i}")));
        }
        settle().await;
        assert_eq!(
            tier.calls.load(Ordering::SeqCst),
            1,
            "eleven rounds were DROPPED, not queued"
        );
        assert!(
            r.activities().is_empty(),
            "nothing is published until it answers"
        );

        tier.release("running the test suite");
        settle().await;
        assert_eq!(
            r.activities(),
            vec![sad("s1", Some("running the test suite"))]
        );

        // …and the slot is released, so the session is describable again.
        // This is the half that makes "drop" survivable.
        r.bus.publish(run_steps("s1", "await bash('git commit')"));
        settle().await;
        assert_eq!(tier.calls.load(Ordering::SeqCst), 2);
        (r.stop)();
    }

    #[tokio::test]
    async fn the_drop_rule_is_per_session_a_burst_on_one_does_not_silence_another() {
        // The busy session's call never settles, so its slot stays held.
        let tier = Arc::new(GatedTier::hang_when("busy", "listing"));
        let r = rig(Some(tier.clone()));
        for i in 0..5 {
            r.bus
                .publish(run_steps("s-busy", &format!("busy round {i}")));
        }
        r.bus.publish(run_steps("s-other", "await bash('ls')"));
        settle().await;

        // One call for the wedged session (four dropped) and one for the
        // other, which was never blocked by it: the ledger is keyed by
        // session, not global.
        assert_eq!(tier.calls.load(Ordering::SeqCst), 2);
        assert_eq!(r.activities(), vec![sad("s-other", Some("listing"))]);
        (r.stop)();
    }

    #[tokio::test]
    async fn turn_finished_clears_the_blurb_and_a_late_answer_for_that_turn_is_discarded() {
        let tier = Arc::new(GatedTier::new());
        let r = rig(Some(tier.clone()));
        r.bus.publish(run_steps("s1", "await bash('deno test')"));
        r.bus.publish(turn_finished("s1"));
        settle().await;
        assert_eq!(r.activities(), vec![sad("s1", None)]);

        // The answer arrives after the turn ended. It describes nothing
        // current, so it is dropped rather than repainting a status line for
        // finished work.
        tier.release("running the test suite");
        settle().await;
        assert_eq!(r.activities(), vec![sad("s1", None)]);
        (r.stop)();
    }

    #[tokio::test]
    async fn a_null_blurb_publishes_nothing_at_all() {
        let r = rig(Some(Arc::new(StubTier::none())));
        r.bus.publish(run_steps("s1", "await bash('ls')"));
        settle().await;
        assert!(r.activities().is_empty());
        (r.stop)();
    }

    // ---- failure is a non-event (the AC) ------------------------------------

    #[tokio::test]
    async fn a_panicking_cheap_tier_leaves_the_rounds_events_untouched() {
        struct Panicking;
        #[async_trait::async_trait]
        impl CheapTier for Panicking {
            async fn title(&self, _f: &str) -> Option<String> {
                None
            }
            async fn ghost_text(&self, _p: &str) -> Option<String> {
                None
            }
            async fn activity(&self, _r: &str) -> Option<String> {
                panic!("provider is down")
            }
        }
        let r = rig(Some(Arc::new(Panicking)));
        let seen = Arc::new(Mutex::new(Vec::<EventType>::new()));
        let sink = seen.clone();
        r.bus.subscribe(Arc::new(move |e: &BoughEvent| {
            sink.lock().unwrap().push(e.r#type)
        }));

        r.bus.publish(run_steps("s1", "await bash('deno test')"));
        settle().await;
        assert!(
            r.activities().is_empty(),
            "no blurb, and no error event either"
        );
        // The listener registered after the watcher still received the round.
        assert_eq!(*seen.lock().unwrap(), vec![EventType::MessagePart]);
        (r.stop)();
    }

    #[tokio::test]
    async fn a_failure_releases_the_slot_on_the_same_watcher_not_just_a_fresh_one() {
        // First call answers nothing (the failure shape a Rust tier can
        // express), second answers — the failed call must give its slot back.
        let tier = Arc::new(StubTier::activity_after_none("committing"));
        let r = rig(Some(tier.clone()));
        r.bus.publish(run_steps("s1", "round one"));
        settle().await;
        r.bus.publish(run_steps("s1", "round two"));
        settle().await;
        assert_eq!(
            tier.activity_calls.load(Ordering::SeqCst),
            2,
            "the failed call gave its slot back"
        );
        assert_eq!(r.activities(), vec![sad("s1", Some("committing"))]);
        (r.stop)();
    }

    #[tokio::test]
    async fn no_cheap_tier_at_all_means_no_listener_work_and_no_events() {
        let r = rig(None);
        r.bus.publish(run_steps("s1", "await bash('ls')"));
        r.bus.publish(turn_finished("s1"));
        settle().await;
        assert!(r.activities().is_empty());
        (r.stop)();
    }

    #[tokio::test]
    async fn unsubscribing_stops_the_watcher() {
        let tier = Arc::new(StubTier::activity("x"));
        let r = rig(Some(tier.clone()));
        (r.stop)();
        r.bus.publish(run_steps("s1", "await bash('ls')"));
        settle().await;
        assert_eq!(tier.activity_calls.load(Ordering::SeqCst), 0);
    }
}
