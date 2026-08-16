//! Transient-failure retries (port of the retry half of `src/llm/client.ts`).
//!
//! **Retries are part of the boundary, not part of the runner.** Every client
//! is wrapped in [`with_retries`], which is sound because a round has no side
//! effects until `run()` resolves — the turn loop executes tools afterwards —
//! so re-sending identical params can at worst repeat streamed text deltas.
//! That is what `on_retry` is for: the caller resets its streaming buffer and
//! emits `message.retry`.
//!
//! The classification table is the TS contract verbatim; in Rust the network
//! faults (the whole `NETWORK_CODES` dance, plain `TypeError` fetch failures,
//! the Anthropic SDK's name-less connection errors) all arrive as status-less
//! `LlmError`s — the transport edge maps them to the 502 default, which is
//! what makes them retryable here.

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use regex::Regex;
use tokio_util::sync::CancellationToken;

use crate::errors::BoughError;
use crate::types::{LlmClient, LlmParams, LlmResult, OnText};

/// Six attempts is roughly 15–31s of jittered backoff (1+2+4+8+16s,
/// halved-to-full): long enough to ride out a network-path flap, short enough
/// that a truly dead network fails the turn in under a minute.
pub const MAX_ATTEMPTS: u32 = 6;
pub const BASE_DELAY_MS: u64 = 1000;

/// The wall clock the whole ladder may spend asleep, across every attempt.
///
/// The attempt count alone does not bound the ladder: a `Retry-After` lifts
/// the delay above the backoff (that is the point of the hint), so a provider
/// answering `retry-after: 60` turns six attempts into five minutes of
/// silence, and the turn ring used to re-run the whole ladder on top of that.
///
/// SIZED TO HONOUR ONE `Retry-After: 60`, AND NOT A SECOND. The first cut of
/// this was 45s, chosen against a quota that could never be satisfied — the
/// prompt alone outspent it, so waiting was pure loss. But the commoner shape
/// is a per-minute limit that is briefly saturated and clears on its own:
/// three subagents fanning out across a fast model spend a 500k/minute budget
/// in under a minute, and every one of them died to a limit that a single
/// wait would have cleared. One minute of silence to recover a turn with all
/// its work intact is worth it; five minutes to be told the same thing is
/// not.
pub const MAX_BUDGET_MS: u64 = 65_000;

// Enforced at build time rather than in a test, because the number IS the
// contract: Cerebras, OpenAI and Anthropic all answer a saturated per-minute
// limit with `retry-after: 60`.
const _: () = assert!(
    MAX_BUDGET_MS >= 60_000,
    "a per-minute limit clears in a minute; refusing to wait it out fails turns \
     whose work was intact"
);
const _: () = assert!(
    MAX_BUDGET_MS < 120_000,
    "two minutes of silence is a hang, not a wait"
);

fn retryable_status(s: u16) -> bool {
    s == 408 || s == 429 || s >= 500
}

static TOOL_PROTOCOL_400: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)tool_calls|tool_call_id|must be followed by tool").unwrap());

/// The 400 a chat-completions provider throws when an assistant `tool_calls`
/// is not followed by its matching tool message. `to_openai_messages` repairs
/// that encoding itself, so a re-send of the (now well-formed) request
/// succeeds — hence this one 400 is retryable while every other 400 stays
/// fatal, since a real caller mistake must not be retried six times.
pub fn is_tool_protocol_400(err: &BoughError) -> bool {
    matches!(err, BoughError::Llm { status: 400, message, .. } if TOOL_PROTOCOL_400.is_match(message))
}

/// Should this failure be re-attempted? A user abort and a caller mistake
/// never are. Status drives the whole table: 408/429/≥500 retryable (the
/// status-less transport default of 502 counts), plus the one self-healed
/// tool-protocol 400. A missing key (401) will still be missing in 15 seconds.
pub fn is_retryable(err: &BoughError) -> bool {
    match err {
        BoughError::Llm { status, .. } => retryable_status(*status) || is_tool_protocol_400(err),
        other => retryable_status(other.status()),
    }
}

/// What one re-attempt looked like, for the `message.retry` event.
#[derive(Clone, Debug)]
pub struct RetryInfo {
    /// 1-based.
    pub attempt: u32,
    pub max_attempts: u32,
    pub error: BoughError,
    pub delay_ms: u64,
}

/// Observes each re-attempt: called after a retryable failure, before the sleep.
pub type OnRetry = Arc<dyn Fn(RetryInfo) + Send + Sync>;

#[derive(Clone, Default)]
pub struct RetryOpts {
    pub on_retry: Option<OnRetry>,
    pub max_attempts: Option<u32>,
    pub base_delay_ms: Option<u64>,
    /// Total sleep the ladder may spend. See [`MAX_BUDGET_MS`].
    pub budget_ms: Option<u64>,
}

/// The provider's Retry-After, in ms, when the error carries one.
fn retry_after_hint(err: &BoughError) -> Option<u64> {
    match err {
        BoughError::Llm { retry_after_ms, .. } => *retry_after_ms,
        _ => None,
    }
}

/// A cheap uniform sample in [0, 1) — jitter needs no cryptographic quality,
/// and the workspace carries no rand crate.
fn jitter() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    (nanos % 1_000_000) as f64 / 1_000_000.0
}

struct Retry {
    inner: Arc<dyn LlmClient>,
    max_attempts: u32,
    base_delay_ms: u64,
    budget_ms: u64,
    on_retry: Option<OnRetry>,
}

#[async_trait::async_trait]
impl LlmClient for Retry {
    async fn run(
        &self,
        params: LlmParams,
        on_text: OnText,
        cancel: CancellationToken,
    ) -> Result<LlmResult, BoughError> {
        let mut attempt: u32 = 1;
        let mut spent_ms: u64 = 0;
        loop {
            match self
                .inner
                .run(params.clone(), on_text.clone(), cancel.clone())
                .await
            {
                Ok(result) => return Ok(result),
                Err(err) => {
                    // Give up on exhaustion, on an abort raised during the
                    // failing run (rethrow the ORIGINAL error, no sleep), or
                    // on a non-retryable classification.
                    if attempt >= self.max_attempts || cancel.is_cancelled() || !is_retryable(&err)
                    {
                        return Err(err);
                    }
                    let backoff = self.base_delay_ms as f64
                        * 2f64.powi(attempt as i32 - 1)
                        * (0.5 + jitter() / 2.0);
                    let hint = retry_after_hint(&err).unwrap_or(0) as f64;
                    let delay_ms = hint.max(backoff).round() as u64;
                    // A wait the budget cannot cover is not a wait worth
                    // making: the user gets the provider's message now
                    // instead of the same message minutes from now.
                    if spent_ms.saturating_add(delay_ms) > self.budget_ms {
                        return Err(err);
                    }
                    spent_ms += delay_ms;
                    if let Some(on_retry) = &self.on_retry {
                        on_retry(RetryInfo {
                            attempt,
                            max_attempts: self.max_attempts,
                            error: err,
                            delay_ms,
                        });
                    }
                    // Abort-aware sleep: an interrupt during backoff rejects
                    // immediately rather than waiting the delay out.
                    tokio::select! {
                        _ = cancel.cancelled() => {
                            return Err(BoughError::llm_with(
                                "interrupted during retry backoff",
                                499,
                                None,
                            ));
                        }
                        _ = tokio::time::sleep(Duration::from_millis(delay_ms)) => {}
                    }
                    attempt += 1;
                }
            }
        }
    }
}

/// Transparent retries around an `LlmClient`. See the module comment for why
/// this is sound.
pub fn with_retries(inner: Arc<dyn LlmClient>, opts: RetryOpts) -> Arc<dyn LlmClient> {
    Arc::new(Retry {
        inner,
        max_attempts: opts.max_attempts.unwrap_or(MAX_ATTEMPTS),
        base_delay_ms: opts.base_delay_ms.unwrap_or(BASE_DELAY_MS),
        budget_ms: opts.budget_ms.unwrap_or(MAX_BUDGET_MS),
        on_retry: opts.on_retry,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::test_support::{fake_client, params, TOOLS};
    use crate::types::LlmBlock;
    use std::sync::Mutex;

    fn noop_text() -> OnText {
        Arc::new(|_| {})
    }

    fn good() -> LlmResult {
        LlmResult {
            content: vec![LlmBlock::Text { text: "ok".into() }],
            stop_reason: "end_turn".into(),
            usage: None,
        }
    }

    #[tokio::test]
    async fn a_transient_500_is_re_attempted_and_then_succeeds() {
        let (client, _calls) = fake_client(vec![
            Err(BoughError::llm_with("openrouter: 500 upstream", 500, None)),
            Ok(good()),
        ]);
        let seen: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let wrapped = with_retries(
            client,
            RetryOpts {
                base_delay_ms: Some(0),
                max_attempts: Some(3),
                on_retry: Some(Arc::new(move |i| seen2.lock().unwrap().push(i.attempt))),
                budget_ms: None,
            },
        );
        let result = wrapped
            .run(params(&TOOLS), noop_text(), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.stop_reason, "end_turn");
        assert_eq!(*seen.lock().unwrap(), vec![1]);
    }

    #[tokio::test]
    async fn a_400_is_a_caller_mistake_and_is_never_re_attempted() {
        let (client, calls) = fake_client(vec![Err(BoughError::llm_with(
            "openai: 400 bad schema",
            400,
            None,
        ))]);
        let wrapped = with_retries(
            client,
            RetryOpts {
                base_delay_ms: Some(0),
                max_attempts: Some(4),
                on_retry: None,
                budget_ms: None,
            },
        );
        let err = wrapped
            .run(params(&TOOLS), noop_text(), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("400 bad schema"));
        assert_eq!(calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn exhausting_the_attempts_rethrows_the_last_failure() {
        let boom = || BoughError::llm("openrouter: stream truncated before completion");
        let (client, calls) = fake_client(vec![Err(boom()), Err(boom()), Err(boom())]);
        let wrapped = with_retries(
            client,
            RetryOpts {
                base_delay_ms: Some(0),
                max_attempts: Some(3),
                on_retry: None,
                budget_ms: None,
            },
        );
        let err = wrapped
            .run(params(&TOOLS), noop_text(), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("truncated before completion"));
        // 3 scripted failures & maxAttempts=3 sees exactly 3 calls.
        assert_eq!(calls.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn an_aborted_token_stops_the_loop_instead_of_backing_off() {
        // The fake aborts the token during the run; the catch must rethrow the
        // original 503 promptly rather than sleeping out a 50s backoff.
        struct AbortingClient;
        #[async_trait::async_trait]
        impl LlmClient for AbortingClient {
            async fn run(
                &self,
                _params: LlmParams,
                _on_text: OnText,
                cancel: CancellationToken,
            ) -> Result<LlmResult, BoughError> {
                cancel.cancel();
                Err(BoughError::llm_with("openrouter: 503", 503, None))
            }
        }
        let wrapped = with_retries(
            Arc::new(AbortingClient),
            RetryOpts {
                base_delay_ms: Some(50_000),
                max_attempts: Some(6),
                on_retry: None,
                budget_ms: None,
            },
        );
        let start = std::time::Instant::now();
        let err = wrapped
            .run(params(&TOOLS), noop_text(), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("503"));
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "must not back off"
        );
    }

    #[tokio::test]
    async fn retry_after_lifts_the_delay_above_the_backoff() {
        let (client, _calls) = fake_client(vec![
            Err(BoughError::llm_with(
                "openrouter: 429 slow down",
                429,
                Some(50),
            )),
            Ok(good()),
        ]);
        let seen: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let wrapped = with_retries(
            client,
            RetryOpts {
                base_delay_ms: Some(1),
                max_attempts: Some(3),
                on_retry: Some(Arc::new(move |i| seen2.lock().unwrap().push(i.delay_ms))),
                budget_ms: None,
            },
        );
        wrapped
            .run(params(&TOOLS), noop_text(), CancellationToken::new())
            .await
            .unwrap();
        // delay = round(max(retryAfterHint, backoff)); the 50ms Retry-After
        // hint dominates the 1ms base's jittered backoff (≤1ms).
        assert_eq!(*seen.lock().unwrap(), vec![50]);
    }

    #[tokio::test]
    async fn a_retry_after_the_budget_cannot_cover_fails_now_instead_of_minutes_from_now() {
        // Cerebras answers a blown token-per-minute quota with `retry-after:
        // 60`. Sleeping it out six times is five silent minutes ending in the
        // same message, so the ladder declines the wait and surfaces it.
        let (client, calls) = fake_client(vec![
            Err(BoughError::llm_with(
                "cerebras: 429 Tokens per minute limit exceeded",
                429,
                Some(60_000),
            )),
            Ok(good()),
        ]);
        let wrapped = with_retries(
            client,
            RetryOpts {
                base_delay_ms: Some(0),
                max_attempts: Some(6),
                on_retry: None,
                budget_ms: Some(45_000),
            },
        );
        let start = std::time::Instant::now();
        let err = wrapped
            .run(params(&TOOLS), noop_text(), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Tokens per minute limit exceeded"));
        assert_eq!(calls.lock().unwrap().len(), 1, "the wait was never made");
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn one_minute_quota_wait_is_honoured_then_the_ladder_stops() {
        // Milliseconds stand in for the seconds a real quota asks for.
        let boom = || BoughError::llm_with("cerebras: 429 tokens per minute", 429, Some(60));
        let (client, calls) = fake_client(vec![Err(boom()), Err(boom()), Ok(good())]);
        let wrapped = with_retries(
            client,
            RetryOpts {
                base_delay_ms: Some(0),
                max_attempts: Some(6),
                on_retry: None,
                budget_ms: Some(65),
            },
        );
        let err = wrapped
            .run(params(&TOOLS), noop_text(), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("tokens per minute"));
        // One wait made (60 ≤ 65), the second declined (120 > 65) — so two
        // calls, and the scripted success is never reached.
        assert_eq!(calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_saturated_minute_that_clears_recovers_instead_of_failing_the_turn() {
        let (client, calls) = fake_client(vec![
            Err(BoughError::llm_with(
                "cerebras: 429 tokens per minute",
                429,
                Some(60),
            )),
            Ok(good()),
        ]);
        let wrapped = with_retries(
            client,
            RetryOpts {
                base_delay_ms: Some(0),
                max_attempts: Some(6),
                on_retry: None,
                budget_ms: Some(65),
            },
        );
        let result = wrapped
            .run(params(&TOOLS), noop_text(), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.stop_reason, "end_turn");
        assert_eq!(
            calls.lock().unwrap().len(),
            2,
            "waited once, then succeeded"
        );
    }

    #[tokio::test]
    async fn the_budget_is_cumulative_not_per_attempt() {
        // Three 20s waits fit no better than one 60s wait: the ladder spends
        // the first two and stops rather than running the attempt count out.
        // Milliseconds stand in for the seconds a real quota asks for — the
        // ratio is what the arithmetic turns on, and the test stays instant.
        let boom = || BoughError::llm_with("openrouter: 429 slow down", 429, Some(20));
        let (client, calls) = fake_client(vec![Err(boom()), Err(boom()), Err(boom()), Ok(good())]);
        let wrapped = with_retries(
            client,
            RetryOpts {
                base_delay_ms: Some(0),
                max_attempts: Some(6),
                on_retry: None,
                budget_ms: Some(45),
            },
        );
        let err = wrapped
            .run(params(&TOOLS), noop_text(), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("slow down"));
        // 20 + 20 spent, the third would reach 60 > 45 and is declined.
        assert_eq!(calls.lock().unwrap().len(), 3);
    }

    #[test]
    fn is_retryable_transport_faults_yes_aborts_and_caller_mistakes_no() {
        assert!(is_retryable(&BoughError::llm("transport fault"))); // defaults to 502
        assert!(is_retryable(&BoughError::llm_with(
            "rate limited",
            429,
            None
        )));
        assert!(is_retryable(&BoughError::llm_with("slow", 408, None)));
        assert!(!is_retryable(&BoughError::llm_with(
            "bad request",
            400,
            None
        )));
        assert!(!is_retryable(&BoughError::llm_with("no key", 401, None)));
        // The abort raised by a cancelled backoff is 499 — never re-attempted.
        assert!(!is_retryable(&BoughError::llm_with(
            "interrupted during retry backoff",
            499,
            None
        )));
        // Network faults arrive from the transport edge as the 502 default —
        // the Rust spelling of ECONNRESET-on-a-plain-Error.
        assert!(is_retryable(&BoughError::llm(
            "error sending request: connection reset"
        )));
    }

    #[test]
    fn is_tool_protocol_400_the_self_healed_encoding_is_the_one_400_worth_retrying() {
        assert!(is_tool_protocol_400(&BoughError::llm_with(
            "openrouter: 400 tool_call_id not found",
            400,
            None
        )));
        assert!(is_retryable(&BoughError::llm_with(
            "openrouter: 400 must be followed by tool messages",
            400,
            None
        )));
        assert!(!is_tool_protocol_400(&BoughError::llm_with(
            "openrouter: 400 model not found",
            400,
            None
        )));
    }
}
