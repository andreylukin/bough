//! Invariant: the filter is a CONJUNCTION of five dimensions, each of which is a DISJUNCTION of
//! its members, and an empty dimension is "no filter" (the `StepQuery` precedent). Every time
//! bound is applied HERE, in [`Filter::matches`] — never pushed into a `StepQuery`, which has no
//! time bounds at all (decision D-C4). A pushed-down bound the store cannot honour would silently
//! widen the result and the pane's header would be lying about what is on screen.
//!
//! Everything in this module is pure and takes its `now` as an argument.

use std::collections::BTreeSet;

use bough_plugin_ledger::{AgentName, Class, Order, Ref, StepQuery, StepType, TrajId};
use chrono::{DateTime, Duration, Utc};

use crate::error::FilterError;
use crate::Row;

/// The composable filter. EVERY populated field is a conjunct; within a field the members are a
/// disjunction, so `agent:sol agent:terra type:tool/call` is (sol ∨ terra) ∧ tool/call.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    pub agents: BTreeSet<AgentName>,
    pub refs: BTreeSet<Ref>,
    pub kinds: BTreeSet<StepType>,
    pub class: Option<Class>,
    /// Inclusive.
    pub since: Option<DateTime<Utc>>,
    /// Exclusive.
    pub until: Option<DateTime<Utc>>,
}

impl Filter {
    /// PURE: the ∧ of the five dimensions.
    pub fn matches(&self, row: &Row) -> bool {
        if !self.agents.is_empty() && !self.agents.contains(&row.agent) {
            return false;
        }
        if !self.kinds.is_empty() && !self.kinds.contains(&row.step.kind) {
            return false;
        }
        if !self.refs.is_empty() && !self.refs.iter().any(|r| row.step.refs.contains(r)) {
            return false;
        }
        if let Some(class) = self.class {
            if row.step.class != class {
                return false;
            }
        }
        // The time bounds live here and nowhere else (D-C4): `since` inclusive, `until` exclusive,
        // so two adjacent windows partition the stream with no row counted twice.
        if let Some(since) = self.since {
            if row.step.at < since {
                return false;
            }
        }
        if let Some(until) = self.until {
            if row.step.at >= until {
                return false;
            }
        }
        true
    }

    pub fn is_empty(&self) -> bool {
        self == &Filter::default()
    }

    /// What the pane's header prints, against the clock the caller holds.
    ///
    /// MERGE (D-C-WP2-1 closed): this took no `now` and could only print absolute instants, so a
    /// filter typed as `since:2h` read back as `since:2026-08-28T06:11:04+00:00`. The note asked
    /// for the argument at merge and here it is — and `render_filter`'s `now`, which was `_now`
    /// and did nothing, means the same thing now.
    pub fn describe(&self, now: DateTime<Utc>) -> String {
        let terms = self.terms_at(now);
        if terms.is_empty() {
            return "everything".to_string();
        }
        terms.join(" \u{2227} ")
    }

    /// The parts a `StepQuery` can honour: trajs, kinds, class — and NOTHING else. `since`/`until`
    /// are not pushed because `StepQuery` has no time bounds (D-C4); `agents` is not pushed
    /// because an agent is named by the trajectory the caller already resolved; `refs` is not
    /// pushed because the row set the pane checks its own invariant against must be the set the
    /// query returned, and a ref pushed down would narrow it behind [`Filter::matches`]'s back.
    pub fn to_query(&self, trajs: Vec<TrajId>, window: usize) -> StepQuery {
        StepQuery {
            trajs,
            kinds: self.kinds.iter().cloned().collect(),
            class: self.class,
            // Newest-first is how the window is TAKEN; `timeline` puts the rows back in order.
            order: Order::SeqDesc,
            limit: Some(window),
            ..Default::default()
        }
    }

    /// The words this filter is spelled with, in a fixed order, with absolute instants.
    fn terms(&self) -> Vec<String> {
        let mut out = Vec::new();
        for a in &self.agents {
            out.push(format!("agent:{a}"));
        }
        for r in &self.refs {
            out.push(format!("ref:{r}"));
        }
        for k in &self.kinds {
            out.push(format!("type:{k}"));
        }
        if let Some(c) = self.class {
            out.push(format!("class:{}", c.as_str()));
        }
        if let Some(t) = self.since {
            out.push(format!("since:{}", t.to_rfc3339()));
        }
        if let Some(t) = self.until {
            out.push(format!("until:{}", t.to_rfc3339()));
        }
        out
    }

    /// The same words, with `since`/`until` in the RELATIVE spelling a person typed whenever the
    /// instant is a whole span behind `now`.
    ///
    /// EXACT multiples only. `parse_filter` resolves `2h` against the `now` it is given, so a
    /// rounded span would not re-parse to the same instant and the editor line would drift every
    /// time it was re-rendered. Anything else stays RFC3339, which is always true.
    fn terms_at(&self, now: DateTime<Utc>) -> Vec<String> {
        let mut out = self.terms();
        for (key, at) in [("since", self.since), ("until", self.until)] {
            let Some(at) = at else { continue };
            let Some(span) = span_behind(now, at) else {
                continue;
            };
            let absolute = format!("{key}:{}", at.to_rfc3339());
            if let Some(slot) = out.iter_mut().find(|t| **t == absolute) {
                *slot = format!("{key}:{span}");
            }
        }
        out
    }
}

/// `now - at` as `Nd` / `Nh` / `Nm` / `Ns`, when it is an exact multiple of one of them and not in
/// the future. `None` otherwise, and the caller prints the instant.
fn span_behind(now: DateTime<Utc>, at: DateTime<Utc>) -> Option<String> {
    let d = now - at;
    let secs = d.num_seconds();
    if secs <= 0 || d.subsec_nanos() != 0 {
        return None;
    }
    for (unit, per) in [("d", 86_400), ("h", 3_600), ("m", 60)] {
        if secs % per == 0 {
            return Some(format!("{}{unit}", secs / per));
        }
    }
    // Seconds only UNDER a minute. Every instant is a whole multiple of one second, so allowing
    // them at any size would spell a two-hour-and-a-bit filter `since:7290s` — a round trip that
    // is exact and a word nobody typed.
    (secs < 60).then(|| format!("{secs}s"))
}

/// PURE: the filter grammar.
///
/// `agent:sol ref:pr/1204 type:tool/call class:evidence since:2h until:2026-08-27T10:00:00Z`
///
/// `since`/`until` take an RFC3339 instant or a relative span (`30s`, `15m`, `2h`, `3d`), which
/// resolves against the `now` passed in. An unknown word is an error naming the word (§16).
pub fn parse_filter(q: &str, now: DateTime<Utc>) -> Result<Filter, FilterError> {
    let mut f = Filter::default();
    for word in q.split_whitespace() {
        let Some((key, value)) = word.split_once(':') else {
            return Err(FilterError::UnknownWord(word.to_string()));
        };
        if value.is_empty() {
            return Err(FilterError::BadValue {
                word: word.to_string(),
                detail: "the value is empty".to_string(),
            });
        }
        match key {
            // `ref:` values contain colons themselves (`step:<id>`), so the SPLIT is on the first
            // colon and the rest of the word is the value, verbatim.
            "agent" => {
                f.agents.insert(AgentName::new(value));
            }
            "ref" => {
                f.refs.insert(Ref::new(value));
            }
            "type" => {
                f.kinds.insert(StepType::new(value));
            }
            "class" => {
                f.class = Some(match value {
                    "evidence" => Class::Evidence,
                    "thought" => Class::Thought,
                    _ => {
                        return Err(FilterError::BadValue {
                            word: word.to_string(),
                            detail: "class is `evidence` or `thought`".to_string(),
                        })
                    }
                });
            }
            "since" => f.since = Some(parse_time(word, value, now)?),
            "until" => f.until = Some(parse_time(word, value, now)?),
            _ => return Err(FilterError::UnknownWord(word.to_string())),
        }
    }
    // An empty window is a filter that can never match anything; saying so beats a pane that
    // renders nothing and lets the reader guess why.
    if let (Some(since), Some(until)) = (f.since, f.until) {
        if since > until {
            return Err(FilterError::EmptyWindow {
                since: since.to_rfc3339(),
                until: until.to_rfc3339(),
            });
        }
    }
    Ok(f)
}

/// PURE: round-trips through [`parse_filter`] for every filter [`parse_filter`] can produce.
/// Time bounds render ABSOLUTE, so the round trip does not depend on the `now` it is given.
pub fn render_filter(f: &Filter, now: DateTime<Utc>) -> String {
    f.terms_at(now).join(" ")
}

/// An RFC3339 instant, or a relative span resolved against `now`.
fn parse_time(word: &str, value: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, FilterError> {
    if let Ok(t) = DateTime::parse_from_rfc3339(value) {
        return Ok(t.with_timezone(&Utc));
    }
    let bad = |detail: &str| FilterError::BadValue {
        word: word.to_string(),
        detail: detail.to_string(),
    };
    let (digits, unit) = value.split_at(value.len() - 1);
    let n: i64 = digits
        .parse()
        .map_err(|_| bad("an RFC3339 instant or a span like `15m`, `2h`, `3d`"))?;
    let span = match unit {
        "s" => Duration::seconds(n),
        "m" => Duration::minutes(n),
        "h" => Duration::hours(n),
        "d" => Duration::days(n),
        _ => return Err(bad("the span unit is one of s, m, h, d")),
    };
    Ok(now - span)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::row;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-27T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn an_empty_filter_matches_everything() {
        let f = Filter::default();
        assert!(f.is_empty());
        assert!(f.matches(&row("sol", "t1", 1, "tool/call", "12:00:00")));
        assert!(f.matches(&row("terra", "t2", 9, "wake/start", "03:00:00")));
        assert_eq!(f.describe(Utc::now()), "everything");
    }

    #[test]
    fn the_five_dimensions_are_conjoined() {
        let f = parse_filter(
            "agent:sol ref:pr/1204 type:tool/call class:thought since:2026-08-27T11:00:00Z",
            now(),
        )
        .expect("a well-formed filter");
        let mut hit = row("sol", "t1", 1, "tool/call", "11:30:00");
        hit.step.refs = std::sync::Arc::new([Ref::new("pr/1204")].into_iter().collect());
        assert!(f.matches(&hit));
        // Each dimension alone is enough to reject: the conjunction is real.
        let mut wrong_agent = hit.clone();
        wrong_agent.agent = AgentName::new("terra");
        assert!(!f.matches(&wrong_agent));
        let mut wrong_kind = hit.clone();
        wrong_kind.step.kind = StepType::new("wake/start");
        assert!(!f.matches(&wrong_kind));
        let mut wrong_ref = hit.clone();
        wrong_ref.step.refs = std::sync::Arc::new(BTreeSet::new());
        assert!(!f.matches(&wrong_ref));
        let mut wrong_class = hit.clone();
        wrong_class.step.class = Class::Evidence;
        assert!(!f.matches(&wrong_class));
        let mut too_old = hit.clone();
        too_old.step.at = row("sol", "t1", 1, "tool/call", "10:00:00").step.at;
        assert!(!f.matches(&too_old));
    }

    #[test]
    fn members_within_one_dimension_are_disjoined() {
        let f = parse_filter("agent:sol agent:terra type:tool/call", now()).expect("well-formed");
        assert!(f.matches(&row("sol", "t1", 1, "tool/call", "12:00:00")));
        assert!(f.matches(&row("terra", "t2", 1, "tool/call", "12:00:00")));
        assert!(!f.matches(&row("scout", "t3", 1, "tool/call", "12:00:00")));
        // …and the OTHER dimension still conjoins.
        assert!(!f.matches(&row("sol", "t1", 1, "wake/start", "12:00:00")));
    }

    #[test]
    fn since_is_inclusive_and_until_is_exclusive() {
        let f = parse_filter(
            "since:2026-08-27T11:00:00Z until:2026-08-27T12:00:00Z",
            now(),
        )
        .expect("well-formed");
        assert!(f.matches(&row("sol", "t1", 1, "x", "11:00:00")), "since");
        assert!(f.matches(&row("sol", "t1", 1, "x", "11:59:59")));
        assert!(!f.matches(&row("sol", "t1", 1, "x", "12:00:00")), "until");
        assert!(!f.matches(&row("sol", "t1", 1, "x", "10:59:59")));
    }

    #[test]
    fn a_relative_span_resolves_against_the_now_it_is_given() {
        let f = parse_filter("since:2h", now()).expect("well-formed");
        assert_eq!(f.since, Some(now() - Duration::hours(2)));
        // A DIFFERENT `now` resolves the same word differently, which is what "pure, clock passed
        // in" means.
        let later = now() + Duration::hours(5);
        assert_eq!(
            parse_filter("since:2h", later).expect("well-formed").since,
            Some(later - Duration::hours(2))
        );
        assert_eq!(
            parse_filter("since:30s", now()).unwrap().since,
            Some(now() - Duration::seconds(30))
        );
        assert_eq!(
            parse_filter("since:3d", now()).unwrap().since,
            Some(now() - Duration::days(3))
        );
    }

    #[test]
    fn an_unknown_word_is_an_error_naming_the_word() {
        let err = parse_filter("agent:sol wombat:7", now()).expect_err("wombat is not a filter");
        assert_eq!(err, FilterError::UnknownWord("wombat:7".to_string()));
        assert!(err.to_string().contains("wombat:7"), "{err}");
        // A bare word is not a silently ignored filter either.
        assert_eq!(
            parse_filter("sol", now()).expect_err("bare word"),
            FilterError::UnknownWord("sol".to_string())
        );
    }

    #[test]
    fn since_after_until_is_refused() {
        let err = parse_filter(
            "since:2026-08-27T12:00:00Z until:2026-08-27T11:00:00Z",
            now(),
        )
        .expect_err("an empty window");
        assert!(matches!(err, FilterError::EmptyWindow { .. }), "{err:?}");
    }

    /// D-C-WP2-1: the header says what the person typed. A whole span behind `now` is spelled
    /// back as that span; anything else stays an instant, which is always true.
    #[test]
    fn a_whole_span_is_described_the_way_it_was_typed() {
        let now = DateTime::parse_from_rfc3339("2026-08-28T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let f = parse_filter("agent:sol since:2h", now).expect("it parses");
        assert_eq!(f.describe(now), "agent:sol \u{2227} since:2h");
        assert_eq!(render_filter(&f, now), "agent:sol since:2h");
        // …and it survives the round trip against the same clock.
        assert_eq!(parse_filter(&render_filter(&f, now), now).unwrap(), f);

        // Not a whole span any more: the instant, which is still exactly right.
        let later = now + Duration::seconds(90);
        assert!(
            f.describe(later).contains("since:2026-08-28T10:00:00"),
            "{}",
            f.describe(later)
        );
    }

    #[test]
    fn render_filter_round_trips_through_parse_filter() {
        for q in [
            "",
            "agent:sol",
            "agent:sol agent:terra type:tool/call",
            "ref:step:abc type:wake/start class:evidence",
            "since:2h until:1h",
            "agent:sol ref:pr/1204 type:tool/call class:thought since:3d",
        ] {
            let f = parse_filter(q, now()).expect(q);
            let text = render_filter(&f, now());
            let again = parse_filter(&text, now()).expect(&text);
            assert_eq!(f, again, "round trip of {q:?} via {text:?}");
        }
    }

    #[test]
    fn to_query_pushes_trajs_kinds_and_class_only() {
        let f = parse_filter(
            "agent:sol type:tool/call class:evidence since:2h until:1h",
            now(),
        )
        .expect("well-formed");
        let q = f.to_query(vec![TrajId::new("t1")], 500);
        assert_eq!(q.trajs, vec![TrajId::new("t1")]);
        assert_eq!(q.kinds, vec![StepType::new("tool/call")]);
        assert_eq!(q.class, Some(Class::Evidence));
        assert_eq!(q.limit, Some(500));
        // D-C4: `StepQuery` has no time bounds, and the seq bounds it DOES have are not a
        // stand-in for them. Nothing about `since`/`until` may reach the store.
        assert!(q.refs.is_empty(), "refs stay in `matches`");
        assert_eq!(q.after, None);
        assert_eq!(q.before, None);
        assert_eq!(q.wake, None);
    }
}
