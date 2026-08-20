//! `bough exec [flags] "do the thing"` — the headless one-shot client (port of
//! `src/cli/exec.ts`).
//!
//! THE INVARIANT THIS FILE HOLDS: **the event stream is opened BEFORE the
//! prompt is posted.** The server answers `POST /sessions/:id/messages` with a
//! 202 and runs the turn behind it, reporting over `/events` — and `/events`
//! has no replay by design (`seq` is a dedupe key, not a resume cursor). A
//! subscriber that attaches after the turn has already published
//! `turn.finished` will never see it, because there is nothing to catch up on
//! and nothing to ask for. The failure mode is not a dropped line of output:
//! the CLI waits out its full `--timeout` and exits 1 on a turn that actually
//! succeeded — and it only does that for turns fast enough to finish inside
//! the post, which is to say for the cheapest and most-tested prompts,
//! intermittently. The tests below pin it with a turn starter that publishes
//! `turn.finished` synchronously inside the post handler.
//!
//! Second invariant: **every effect is injected.** [`run_exec`] takes a fetch,
//! a stdout writer, a stderr writer, stdin, the environment and the cwd, and it
//! RETURNS an exit code rather than exiting. [`real_deps`] is the only impure
//! constructor.
//!
//! Third: **argument parsing is pure and total.** [`parse_exec_args`] never
//! reads the environment, never exits, never panics.
//!
//! Fourth: **a timeout STOPS the turn it abandoned.** The timeout path raises
//! `POST /sessions/:id/interrupt` on a short deadline of its own and reports
//! what actually happened. The exit code does not move: a turn this client gave
//! up on did not complete, whether or not the stop landed.
//!
//! Exit codes are the contract with whatever shell or CI job wraps this:
//!
//!   0  the turn completed (`turn.finished` with status `done`)
//!   1  the turn did not complete — `error`, `interrupted`, `orphaned`, or the
//!      `--timeout` elapsed first
//!   2  usage problem (bad flag, no prompt) or connection problem (no server on
//!      the port, or it refused the session)

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures::stream::{BoxStream, StreamExt};
use serde_json::{json, Value};
use tokio::time::Instant;

// ---- arguments ---------------------------------------------------------------

/// The default wall clock for a whole turn. Generous: a real turn runs minutes.
pub const DEFAULT_TIMEOUT_SECONDS: f64 = 900.0;
/// Where the server is when neither `--port` nor `BOUGH_PORT` says otherwise.
pub const DEFAULT_PORT: u32 = 4321;

pub const USAGE: &str = concat!(
    "usage: bough exec [-w DIR] [-m MODEL] [--json] [--timeout SECS] [--port N] \"prompt\"\n",
    "       (or pipe the prompt on stdin, with `-` or no positional argument)\n",
    "\n",
    "  -w, --workspace DIR   the checkout the turn runs in (default: cwd)\n",
    "  -m, --model MODEL     override the model for this turn\n",
    "      --json            one JSON envelope per line instead of streamed text\n",
    "      --timeout SECS    wall clock for the whole turn (default: 900)\n",
    "      --port N          server port (default: BOUGH_PORT, then 4321)\n",
    "  -h, --help            this message\n",
    "\n",
    "programs run as you, with your authority — there is no sandbox."
);

/// A well-formed invocation. `prompt` is still unresolved — `-` means "read stdin".
#[derive(Clone, Debug, PartialEq)]
pub struct ExecArgs {
    /// The positional, verbatim. Empty or `-` defers to stdin.
    pub prompt: String,
    pub workspace: Option<String>,
    pub model: Option<String>,
    pub json: bool,
    /// Already in milliseconds, already validated positive and finite.
    pub timeout_ms: u64,
    /// Absent = fall back to `BOUGH_PORT`, then [`DEFAULT_PORT`].
    pub port: Option<u32>,
}

/// What [`parse_exec_args`] returns. Help is stdout + exit 0; a usage ERROR is
/// stderr + exit 2 — they are different answers to different questions, and
/// treating `--help` as an unknown flag makes the first thing anyone types at a
/// new CLI print an error and fail a shell `&&`.
#[derive(Clone, Debug, PartialEq)]
pub enum ExecParse {
    Args(Box<ExecArgs>),
    Help,
    UsageError(String),
}

const VALUE_FLAGS: [&str; 4] = ["workspace", "model", "timeout", "port"];

/// Pure, total argument parsing.
///
/// Two decisions worth naming. **A second positional is an error, not something
/// to ignore.** `bough exec write the tests` is a forgotten pair of quotes, and
/// taking the first would run the one-word prompt "write" and report success.
/// **Unknown flags are errors too**: a typo'd `--jsno` that silently streams is
/// worse than one that stops.
pub fn parse_exec_args(argv: &[String]) -> ExecParse {
    let mut positional: Vec<String> = Vec::new();
    let mut workspace: Option<String> = None;
    let mut model: Option<String> = None;
    let mut timeout: Option<String> = None;
    let mut port_value: Option<String> = None;
    let mut json = false;
    let mut only_positional = false;

    let mut i = 0usize;
    while i < argv.len() {
        let token = argv[i].clone();
        if only_positional {
            positional.push(token);
            i += 1;
            continue;
        }
        if token == "--" {
            only_positional = true;
            i += 1;
            continue;
        }

        let name: String;
        let mut inline: Option<String> = None;
        if let Some(body) = token.strip_prefix("--") {
            match body.split_once('=') {
                Some((n, v)) => {
                    name = n.to_string();
                    inline = Some(v.to_string());
                }
                None => name = body.to_string(),
            }
        } else if token.starts_with('-') && token.chars().count() > 1 {
            // A bare `-` is the stdin sentinel, not a flag — hence `> 1`.
            let body = &token[1..];
            let (short, short_inline) = match body.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (body, None),
            };
            if short == "h" {
                return ExecParse::Help;
            }
            name = match short {
                "w" => "workspace".to_string(),
                "m" => "model".to_string(),
                _ => return ExecParse::UsageError(format!("unknown flag -{short}\n{USAGE}")),
            };
            inline = short_inline;
        } else {
            positional.push(token);
            i += 1;
            continue;
        }

        if name == "help" {
            return ExecParse::Help;
        }
        if name == "json" {
            if inline.is_some() {
                return ExecParse::UsageError(format!("--json takes no value\n{USAGE}"));
            }
            json = true;
            i += 1;
            continue;
        }
        if !VALUE_FLAGS.contains(&name.as_str()) {
            return ExecParse::UsageError(format!("unknown flag --{name}\n{USAGE}"));
        }
        let value = match inline {
            Some(v) => v,
            None => {
                // Consume the next token even if it starts with `-`: a model id
                // or a path may legitimately do so, and refusing one here would
                // be a rule the user cannot work around.
                if i + 1 >= argv.len() {
                    return ExecParse::UsageError(format!("--{name} needs a value\n{USAGE}"));
                }
                i += 1;
                argv[i].clone()
            }
        };
        match name.as_str() {
            "workspace" => workspace = Some(value),
            "model" => model = Some(value),
            "timeout" => timeout = Some(value),
            "port" => port_value = Some(value),
            _ => unreachable!("VALUE_FLAGS is exhaustive"),
        }
        i += 1;
    }

    if positional.len() > 1 {
        return ExecParse::UsageError(format!(
            "expected one prompt, got {} arguments — quote it as a single string\n{USAGE}",
            positional.len()
        ));
    }

    let mut timeout_ms = (DEFAULT_TIMEOUT_SECONDS * 1000.0) as u64;
    if let Some(raw) = &timeout {
        match js_number(raw) {
            Some(seconds) if seconds.is_finite() && seconds > 0.0 => {
                timeout_ms = (seconds * 1000.0).round() as u64;
            }
            _ => {
                return ExecParse::UsageError(format!(
                    "--timeout wants a positive number of seconds, got {raw}"
                ))
            }
        }
    }

    let mut port: Option<u32> = None;
    if let Some(raw) = &port_value {
        match js_number(raw) {
            Some(n) if n.fract() == 0.0 && (1.0..=65535.0).contains(&n) => port = Some(n as u32),
            _ => {
                return ExecParse::UsageError(format!("--port wants a port number, got {raw}"));
            }
        }
    }

    ExecParse::Args(Box::new(ExecArgs {
        prompt: positional.first().cloned().unwrap_or_default(),
        workspace,
        model,
        json,
        timeout_ms,
        port,
    }))
}

/// JavaScript's `Number(string)` for the cases these flags can hit: whitespace
/// is trimmed, an empty string is 0, anything unparseable is NaN (here `None`).
/// The exact semantics are load-bearing — `--port ""` must be refused with the
/// port message, not accepted as a default.
fn js_number(raw: &str) -> Option<f64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Some(0.0);
    }
    trimmed.parse::<f64>().ok()
}

// ---- the SSE frame reader ------------------------------------------------------

/// One parsed SSE frame: the `event:` name and the decoded `data:` payload.
#[derive(Clone, Debug, PartialEq)]
pub struct SseFrame {
    pub name: String,
    pub data: Value,
}

/// Incremental SSE parsing, split out so it is testable on strings.
///
/// Frames are separated by a blank line and a chunk boundary can fall anywhere,
/// including mid-frame and mid-line, so nothing may be interpreted until its
/// terminator has arrived. Parsing per line — tracking the last `event:` seen
/// across frames — quietly mislabels a payload whenever the field order varies
/// or a comment frame lands between the two lines.
///
/// A frame whose `data:` is not JSON is dropped rather than thrown on: comment
/// frames (`: connected`, `: ping`) carry no data at all, and one malformed
/// payload must not end a turn that is otherwise streaming fine.
#[derive(Default)]
pub struct SseReader {
    buffer: String,
}

impl SseReader {
    pub fn new() -> Self {
        SseReader::default()
    }

    pub fn push(&mut self, chunk: &str) -> Vec<SseFrame> {
        self.buffer.push_str(&chunk.replace("\r\n", "\n"));
        let mut frames = Vec::new();
        while let Some(cut) = self.buffer.find("\n\n") {
            let block = self.buffer[..cut].to_string();
            self.buffer = self.buffer[cut + 2..].to_string();
            if let Some(frame) = parse_sse_block(&block) {
                frames.push(frame);
            }
        }
        frames
    }
}

fn parse_sse_block(block: &str) -> Option<SseFrame> {
    let mut name = "message".to_string();
    let mut data_lines: Vec<&str> = Vec::new();
    for line in block.split('\n') {
        if line.starts_with(':') {
            continue; // comment — heartbeats land here
        }
        if let Some(rest) = line.strip_prefix("event:") {
            name = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim_start());
        }
    }
    if data_lines.is_empty() {
        return None;
    }
    let joined = data_lines.join("\n");
    match serde_json::from_str::<Value>(&joined) {
        Ok(data) => Some(SseFrame { name, data }),
        Err(_) => None,
    }
}

// ---- the injected effects ------------------------------------------------------

/// One raw HTTP exchange, as the fetch seam sees it. Body is JSON text; the
/// transport sets `content-type: application/json` iff it is set.
#[derive(Clone, Debug)]
pub struct ExecRequest {
    pub method: String,
    pub url: String,
    pub body: Option<String>,
}

/// A response whose body is still a STREAM. `/events` is the whole reason this
/// seam is not "status + text": the turn is read off the body as it arrives.
pub struct ExecResponse {
    pub status: u16,
    pub chunks: BoxStream<'static, Result<String, String>>,
}

impl ExecResponse {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Drain the body to a string. Errors mid-body end the read — what has
    /// arrived is what the caller gets, which is all these call sites need.
    pub async fn text(mut self) -> String {
        let mut out = String::new();
        while let Some(next) = self.chunks.next().await {
            match next {
                Ok(part) => out.push_str(&part),
                Err(_) => break,
            }
        }
        out
    }
}

pub type ExecFuture = Pin<Box<dyn Future<Output = Result<ExecResponse, String>> + Send>>;
pub type ExecFetch = Arc<dyn Fn(ExecRequest) -> ExecFuture + Send + Sync>;
pub type StdinFuture = Pin<Box<dyn Future<Output = Result<String, String>> + Send>>;

/// Everything [`run_exec`] touches that is not a pure function. Production
/// wires these to the real process in [`real_deps`]; a test wires them to a
/// fake server and a string buffer.
#[derive(Clone)]
pub struct ExecDeps {
    pub fetch: ExecFetch,
    /// stdout. Assistant text goes here verbatim, and nothing else does.
    pub write: Arc<dyn Fn(&str) + Send + Sync>,
    /// stderr. Diagnostics, retry notices, usage errors — never the answer.
    pub warn: Arc<dyn Fn(&str) + Send + Sync>,
    /// The whole of piped stdin. Only called when the prompt defers to it.
    pub read_stdin: Arc<dyn Fn() -> StdinFuture + Send + Sync>,
    pub stdin_is_terminal: Arc<dyn Fn() -> bool + Send + Sync>,
    pub env: Arc<dyn Fn(&str) -> Option<String> + Send + Sync>,
    pub cwd: Arc<dyn Fn() -> String + Send + Sync>,
    /// Resolves and validates `--workspace`. `Err` if it is not a directory.
    pub real_path: Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>,
}

/// The `--json` result envelope. One line, printed once, on a finished turn.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct ExecEnvelope {
    pub session: String,
    /// A [`bough_core::schema::parts::TurnStatus`] string, or the literal
    /// `"timeout"`.
    pub status: String,
    /// The exit code this envelope corresponds to being 0.
    pub ok: bool,
    /// The assistant text `--json` suppressed from stdout. Relocated, not dropped.
    pub text: String,
    /// Present when the turn errored — the server's own message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Absent if the post-turn fetch failed; the envelope is still printed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
    /// This session plus every subagent and workflow agent collapsed under it.
    #[serde(rename = "treeUsage", default, skip_serializing_if = "Option::is_none")]
    pub tree_usage: Option<Value>,
}

/// How long the abandon-time interrupt gets. Short: nobody waits on the answer.
const INTERRUPT_TIMEOUT_MS: u64 = 5_000;

// ---- the run -------------------------------------------------------------------

/// The whole client. Returns the process exit code; never exits, never writes
/// to a real stream, never reads a global.
/// Prefix the turn with its own wall clock.
///
/// `--timeout` was only ever enforced out here: the client stops waiting and
/// stops the turn, and the model was never told a deadline existed. An agent
/// that does not know it has 810 seconds cannot decide to spend them, so it
/// stops when it feels finished -- which across a benchmark suite meant turns
/// ending at a fraction of their budget with the work unverified, and other
/// turns walking into a cut-off they could have seen coming.
///
/// Stated as a fact, not an exhortation: how to spend the time is the model's
/// business, but it cannot be its business while the number is a secret.
fn with_deadline(prompt: &str, timeout_ms: u64) -> String {
    let seconds = timeout_ms / 1000;
    if seconds == 0 {
        return prompt.to_string();
    }
    format!(
        "[this turn has {seconds} seconds of wall clock. When it runs out the turn is \
cut off wherever it is; work already written to disk survives, unfinished reasoning \
does not.]\n\n{prompt}"
    )
}

pub async fn run_exec(argv: &[String], deps: &ExecDeps) -> i32 {
    let parsed = match parse_exec_args(argv) {
        ExecParse::Help => {
            (deps.write)(&format!("{USAGE}\n"));
            return 0;
        }
        ExecParse::UsageError(message) => {
            (deps.warn)(&message);
            return 2;
        }
        ExecParse::Args(args) => *args,
    };

    // The prompt: the positional, or stdin when it is `-` or absent with stdin
    // piped. An absent positional on a TERMINAL is the empty invocation —
    // `bough exec` alone — and reading stdin there would hang on the user's
    // keyboard with no prompt shown, so it is a usage error instead.
    let mut prompt = parsed.prompt.trim().to_string();
    if prompt == "-" || (prompt.is_empty() && !(deps.stdin_is_terminal)()) {
        match (deps.read_stdin)().await {
            Ok(text) => prompt = text.trim().to_string(),
            Err(message) => {
                (deps.warn)(&format!("cannot read the prompt from stdin: {message}"));
                return 2;
            }
        }
    }
    if prompt.is_empty() {
        (deps.warn)(USAGE);
        return 2;
    }

    let raw_env_port = (deps.env)("BOUGH_PORT");
    let port: u32 = match parsed.port {
        Some(p) => p,
        None => {
            let n = match &raw_env_port {
                Some(v) => js_number(v),
                None => Some(DEFAULT_PORT as f64),
            };
            match n {
                Some(n) if n.fract() == 0.0 && (1.0..=65535.0).contains(&n) => n as u32,
                _ => {
                    (deps.warn)(&format!(
                        "BOUGH_PORT is not a port number: {}",
                        raw_env_port.unwrap_or_default()
                    ));
                    return 2;
                }
            }
        }
    };
    let api = format!("http://127.0.0.1:{port}");

    // One deadline over the whole run, not just the wait: a server that accepts
    // the connection and then never answers must not hang forever either.
    // `timed_out` distinguishes "the deadline fired" from "the socket died",
    // which is the difference between exit 1 and exit 2.
    let deadline = Instant::now() + Duration::from_millis(parsed.timeout_ms);
    let mut timed_out = false;

    let workspace = match &parsed.workspace {
        Some(dir) => match (deps.real_path)(dir) {
            Ok(resolved) => resolved,
            Err(message) => {
                (deps.warn)(&format!("--workspace {dir}: {message}"));
                return 2;
            }
        },
        None => (deps.cwd)(),
    };

    // 1. The session.
    let mut body = json!({
        "title": format!("exec: {}", take_chars(&prompt, 48)),
        "workspace": workspace,
    });
    if let Some(model) = &parsed.model {
        body["model"] = json!(model);
    }
    let created = fetch_at(
        deps,
        deadline,
        &mut timed_out,
        ExecRequest {
            method: "POST".into(),
            url: format!("{api}/sessions"),
            body: Some(body.to_string()),
        },
    )
    .await;
    let session_id = match created {
        Ok(res) => {
            if !res.ok() {
                let status = res.status;
                let text = res.text().await;
                (deps.warn)(&format!(
                    "bough refused the session: {status} {}",
                    text.trim()
                ));
                return 2;
            }
            let text = res.text().await;
            match serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.get("id").and_then(Value::as_str).map(str::to_string))
            {
                Some(id) => id,
                None => {
                    (deps.warn)("bough refused the session: the response carried no id");
                    return 2;
                }
            }
        }
        Err(message) => {
            (deps.warn)(&if timed_out {
                format!("timed out connecting to bough on :{port}")
            } else {
                format!("cannot reach bough on :{port} — is the server running? ({message})")
            });
            return 2;
        }
    };

    // 2. THE ORDERING. The stream is opened, and its bus subscription is live,
    //    before the prompt exists server-side.
    let mut events = match fetch_at(
        deps,
        deadline,
        &mut timed_out,
        ExecRequest {
            method: "GET".into(),
            url: format!("{api}/events?sessionId={session_id}"),
            body: None,
        },
    )
    .await
    {
        Ok(res) => {
            if !res.ok() {
                (deps.warn)(&format!("bough refused the event stream: {}", res.status));
                return 2;
            }
            res
        }
        Err(message) => {
            (deps.warn)(&if timed_out {
                format!("timed out opening the event stream on :{port}")
            } else {
                format!("cannot open the bough event stream on :{port} ({message})")
            });
            return 2;
        }
    };

    // 3. The prompt. A turn that finishes inside this call is already in the
    //    stream's queue by the time we read it — that is the point of step 2.
    match fetch_at(
        deps,
        deadline,
        &mut timed_out,
        ExecRequest {
            method: "POST".into(),
            url: format!("{api}/sessions/{session_id}/messages"),
            body: Some(json!({ "text": with_deadline(&prompt, parsed.timeout_ms) }).to_string()),
        },
    )
    .await
    {
        Ok(res) => {
            if !res.ok() {
                let status = res.status;
                let text = res.text().await;
                (deps.warn)(&format!(
                    "bough refused the message: {status} {}",
                    text.trim()
                ));
                return 2;
            }
        }
        Err(message) => {
            (deps.warn)(&if timed_out {
                format!("timed out posting the prompt to :{port}")
            } else {
                format!("cannot post the prompt to bough on :{port} ({message})")
            });
            return 2;
        }
    }

    // 4. Consume until the turn ends.
    let mut feed = SseReader::new();
    let mut status = "timeout".to_string();
    let mut error: Option<String> = None;
    let mut text = String::new();
    let mut streamed = false;

    'outer: loop {
        let next = match tokio::time::timeout_at(deadline, events.chunks.next()).await {
            Ok(next) => next,
            Err(_) => {
                // The deadline fired. Both this and a dead connection leave
                // `status` at its default and are reported below.
                timed_out = true;
                break;
            }
        };
        let chunk = match next {
            Some(Ok(chunk)) => chunk,
            _ => break,
        };
        for frame in feed.push(&chunk) {
            let data = payload_of(&frame.data);
            match frame.name.as_str() {
                "message.delta" => {
                    let delta = data
                        .as_ref()
                        .and_then(|d| d.get("delta"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if delta.is_empty() {
                        continue;
                    }
                    text.push_str(delta);
                    if !parsed.json {
                        (deps.write)(delta);
                        streamed = true;
                    }
                }
                "message.retry" => {
                    // The message re-streams from the top, so whatever reached
                    // stdout is about to be repeated. stdout cannot be
                    // un-written, so the boundary is announced on stderr and the
                    // captured text is dropped — the envelope must carry the
                    // answer, not the false start.
                    let attempt = data
                        .as_ref()
                        .and_then(|d| d.get("attempt"))
                        .map(render_scalar)
                        .unwrap_or_else(|| "?".to_string());
                    let reason = data
                        .as_ref()
                        .and_then(|d| d.get("reason"))
                        .and_then(Value::as_str)
                        .unwrap_or("no reason given")
                        .to_string();
                    text.clear();
                    (deps.warn)(&format!("[retry {attempt}: {reason}]"));
                }
                "ask.question" => {
                    // NOBODY IS HERE TO ANSWER. A program that calls `ask()` —
                    // or a workflow launch, which raises an approval card by
                    // default — parked forever under this client: exec had no
                    // case for the event, so the turn sat held until `--timeout`
                    // and exited 1 on work that was one answer from finishing.
                    // Declining is the documented dismissal, so the program gets
                    // an error it can act on and the turn ends on its own terms.
                    let Some(held) = data.as_ref() else { continue };
                    if held.get("status").and_then(Value::as_str) != Some("pending") {
                        continue;
                    }
                    let question = held
                        .get("question")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let first_line = question.split('\n').next().unwrap_or_default();
                    (deps.warn)(&format!(
                        "[declined a question — bough exec is not interactive: {}]",
                        take_chars(first_line, 120)
                    ));
                    let qid = held.get("id").and_then(Value::as_str).unwrap_or_default();
                    let qsid = held
                        .get("sessionId")
                        .and_then(Value::as_str)
                        .unwrap_or(&session_id);
                    // Fire and forget: a failed decline leaves the old behaviour
                    // (a hold that waits out the deadline), and blocking the
                    // stream on it would be worse.
                    let pending = (deps.fetch)(ExecRequest {
                        method: "POST".into(),
                        url: format!(
                            "{api}/sessions/{}/questions/{}",
                            encode_component(qsid),
                            encode_component(qid)
                        ),
                        body: Some(json!({ "decline": true }).to_string()),
                    });
                    tokio::spawn(async move {
                        let _ = pending.await;
                    });
                }
                "turn.finished" => {
                    status = data
                        .as_ref()
                        .and_then(|d| d.get("status"))
                        .and_then(Value::as_str)
                        .unwrap_or("done")
                        .to_string();
                    error = data
                        .as_ref()
                        .and_then(|d| d.get("error"))
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    break 'outer;
                }
                _ => {}
            }
        }
    }
    drop(events);

    if status == "timeout" {
        // Stop what we walked away from. On its OWN deadline, because the run's
        // deadline has already fired by definition. Best-effort in both
        // directions: a stop that fails is reported and changes nothing else,
        // since the turn is unfinished either way.
        let stopped = stop_turn(&api, &session_id, deps).await;
        let verb = if stopped {
            "interrupted"
        } else {
            "could NOT interrupt"
        };
        (deps.warn)(&if timed_out {
            format!(
                "timed out after {}s — {verb} the turn in session {session_id}",
                render_seconds(parsed.timeout_ms)
            )
        } else {
            format!(
                "the event stream closed before the turn finished — {verb} the turn in session {session_id}"
            )
        });
    } else if let Some(message) = &error {
        (deps.warn)(&format!("turn {status}: {message}"));
    }

    let ok = status == "done";

    if parsed.json {
        // Usage comes from `GET /sessions/:id` after the turn, not from the
        // stream: it is the reconnect endpoint and therefore the authoritative
        // record, and the cache splits that decide the cost are only summed once
        // the turn ends. Best-effort — an envelope without usage still tells the
        // caller what happened, and a failed metrics fetch must not change the
        // exit code.
        let mut envelope = ExecEnvelope {
            session: session_id.clone(),
            status: status.clone(),
            ok,
            text: text.clone(),
            error: error.clone(),
            usage: None,
            tree_usage: None,
        };
        let fetched = fetch_at(
            deps,
            deadline,
            &mut timed_out,
            ExecRequest {
                method: "GET".into(),
                url: format!("{api}/sessions/{session_id}"),
                body: None,
            },
        )
        .await;
        if let Ok(res) = fetched {
            if res.ok() {
                let body = res.text().await;
                if let Ok(value) = serde_json::from_str::<Value>(&body) {
                    if let Some(Value::Object(mut usage)) = value.get("usage").cloned() {
                        let tree = usage.remove("tree");
                        envelope.usage = Some(Value::Object(usage));
                        envelope.tree_usage = tree;
                    }
                }
            }
            // A non-2xx body is dropped with the response — its absence from the
            // envelope is the report.
        }
        (deps.write)(&format!(
            "{}\n",
            serde_json::to_string(&envelope).unwrap_or_else(|_| "{}".into())
        ));
    } else if streamed {
        // Deltas end mid-line far more often than not; land the prompt cleanly.
        (deps.write)("\n");
    }

    if ok {
        0
    } else {
        1
    }
}

/// One request under the run's deadline. `Err` is either the transport's own
/// failure text or the deadline firing, which the caller distinguishes through
/// the `timed_out` flag exactly as the TS `AbortController` does.
async fn fetch_at(
    deps: &ExecDeps,
    deadline: Instant,
    timed_out: &mut bool,
    req: ExecRequest,
) -> Result<ExecResponse, String> {
    match tokio::time::timeout_at(deadline, (deps.fetch)(req)).await {
        Ok(result) => result,
        Err(_) => {
            *timed_out = true;
            Err("the deadline elapsed".to_string())
        }
    }
}

/// Raise the user interrupt on a turn this client is giving up on.
///
/// Returns whether a turn was actually signalled. `false` covers three
/// different things — the request failed, the server said nothing was running,
/// the deadline elapsed — and the caller deliberately does not distinguish
/// them: all three mean "do not claim it was stopped", which is the only claim
/// worth being careful about.
async fn stop_turn(api: &str, session_id: &str, deps: &ExecDeps) -> bool {
    let req = ExecRequest {
        method: "POST".into(),
        url: format!("{api}/sessions/{session_id}/interrupt"),
        body: None,
    };
    let call = async {
        let res = (deps.fetch)(req).await.ok()?;
        if !res.ok() {
            return None;
        }
        let body = res.text().await;
        serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v.get("interrupted").and_then(Value::as_bool))
    };
    matches!(
        tokio::time::timeout(Duration::from_millis(INTERRUPT_TIMEOUT_MS), call).await,
        Ok(Some(true))
    )
}

/// The event payload sits at `envelope.data` — the SSE frame carries the whole
/// stamped `BoughEvent`, not the bare payload.
fn payload_of(envelope: &Value) -> Option<Value> {
    envelope.get("data").cloned()
}

/// `attempt` is a number on the wire; render it the way `${}` would.
fn render_scalar(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "?".to_string(),
        other => other.to_string(),
    }
}

/// `parsed.timeoutMs / 1000` as JavaScript prints it: `0.15`, not `0.15000`.
fn render_seconds(ms: u64) -> String {
    format!("{}", ms as f64 / 1000.0)
}

/// `String.prototype.slice(0, n)` over CHARACTERS, so a multi-byte prompt is
/// never cut mid-scalar (which in Rust is a panic, not a mojibake).
fn take_chars(value: &str, n: usize) -> String {
    value.chars().take(n).collect()
}

/// `encodeURIComponent` for the ids that go into the decline URL.
fn encode_component(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

// ---- the process -----------------------------------------------------------------

/// The real process, wired up once. The only impure thing in this file.
pub fn real_deps() -> ExecDeps {
    let client = reqwest::Client::new();
    ExecDeps {
        fetch: Arc::new(move |req: ExecRequest| {
            let client = client.clone();
            Box::pin(async move {
                let method = reqwest::Method::from_bytes(req.method.as_bytes())
                    .map_err(|e| e.to_string())?;
                let mut builder = client.request(method, &req.url);
                if let Some(body) = req.body {
                    builder = builder
                        .header("content-type", "application/json")
                        .body(body);
                }
                let res = builder.send().await.map_err(|e| e.to_string())?;
                let status = res.status().as_u16();
                // Decoded incrementally: a multi-byte character split across two
                // network chunks must survive, and SSE framing is done on text.
                let mut pending: Vec<u8> = Vec::new();
                let chunks = res.bytes_stream().map(move |item| match item {
                    Ok(bytes) => {
                        pending.extend_from_slice(&bytes);
                        let valid = match std::str::from_utf8(&pending) {
                            Ok(_) => pending.len(),
                            Err(e) => e.valid_up_to(),
                        };
                        let head: Vec<u8> = pending.drain(..valid).collect();
                        Ok(String::from_utf8_lossy(&head).into_owned())
                    }
                    Err(e) => Err(e.to_string()),
                });
                Ok(ExecResponse {
                    status,
                    chunks: Box::pin(chunks),
                })
            }) as ExecFuture
        }),
        write: Arc::new(|text: &str| {
            use std::io::Write;
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            let _ = lock.write_all(text.as_bytes());
            let _ = lock.flush();
        }),
        warn: Arc::new(|text: &str| eprintln!("{text}")),
        read_stdin: Arc::new(|| {
            Box::pin(async {
                use tokio::io::AsyncReadExt;
                let mut text = String::new();
                tokio::io::stdin()
                    .read_to_string(&mut text)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(text)
            }) as StdinFuture
        }),
        stdin_is_terminal: Arc::new(|| {
            use std::io::IsTerminal;
            std::io::stdin().is_terminal()
        }),
        env: Arc::new(|name: &str| std::env::var(name).ok()),
        cwd: Arc::new(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
        }),
        real_path: Arc::new(|path: &str| {
            std::fs::canonicalize(path)
                .map(|p| p.to_string_lossy().into_owned())
                .map_err(|e| e.to_string())
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, RwLock};

    use bough_core::bus::Bus;
    use bough_core::db::sqlite_db::{DbOptions, SqliteDb};
    use bough_core::schema::events::{EventInput, EventType};
    use bough_core::schema::parts::{Message, Session};
    use bough_core::turn::queue::TurnRegistry;
    use bough_core::types::{system_clock, AppCtx, HostState, SharedDb, TurnStarter};
    use bough_server::app::{create_handler, CreateHandlerOptions, Dispatcher};

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    fn args_of(parse: ExecParse) -> ExecArgs {
        match parse {
            ExecParse::Args(a) => *a,
            other => panic!("expected args, got {other:?}"),
        }
    }

    // ---- the fixture: the REAL route table over an in-memory database ------

    /// What the fabricated turn does when a prompt lands.
    type FakeTurn = Arc<dyn Fn(&AppCtx, &Session) + Send + Sync>;

    struct FakeStarter(FakeTurn);
    impl TurnStarter for FakeStarter {
        fn start_turn(&self, ctx: &AppCtx, session: &Session, _message: &Message) {
            (self.0)(ctx, session);
        }
    }

    /// Publishes some assistant text, then finishes the turn — all
    /// synchronously, inside the post handler. A client that subscribes AFTER
    /// the post observes nothing at all; that is what makes the ordering test
    /// able to fail.
    fn instant_turn(text: &str, status: &str, error: Option<&str>) -> FakeTurn {
        let text = text.to_string();
        let status = status.to_string();
        let error = error.map(str::to_string);
        Arc::new(move |ctx: &AppCtx, session: &Session| {
            let message_id = uuid::Uuid::new_v4().to_string();
            if !text.is_empty() {
                ctx.bus.publish(EventInput {
                    r#type: EventType::MessageDelta,
                    session_id: Some(session.id.clone()),
                    data: json!({ "messageId": message_id, "delta": text }),
                });
            }
            let mut data = json!({
                "turnId": uuid::Uuid::new_v4().to_string(),
                "sessionId": session.id,
                "status": status,
            });
            if let Some(e) = &error {
                data["error"] = json!(e);
            }
            ctx.bus.publish(EventInput {
                r#type: EventType::TurnFinished,
                session_id: Some(session.id.clone()),
                data,
            });
        })
    }

    struct Fixture {
        deps: ExecDeps,
        /// Method + path of every request, in the order the client made them.
        calls: Arc<Mutex<Vec<String>>>,
        out: Arc<Mutex<String>>,
        err: Arc<Mutex<String>>,
        ctx: AppCtx,
    }

    impl Fixture {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        fn out(&self) -> String {
            self.out.lock().unwrap().clone()
        }
        fn err(&self) -> String {
            self.err.lock().unwrap().clone()
        }
    }

    #[derive(Default)]
    struct FixtureOptions {
        turn: Option<FakeTurn>,
        stdin: Option<String>,
        is_terminal: Option<bool>,
        env: Vec<(String, String)>,
        /// Replaces the fetch seam entirely (the "no server" cases).
        fetch: Option<ExecFetch>,
        /// Replaces `real_path` (the "server refuses the session" case).
        real_path: Option<Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>>,
    }

    fn fixture(options: FixtureOptions) -> Fixture {
        let db: SharedDb = Arc::new(Mutex::new(
            SqliteDb::new(":memory:", DbOptions::default()).unwrap(),
        ));
        let ctx = AppCtx {
            db,
            bus: Arc::new(Bus::new(system_clock())),
            llm: None,
            model: Some("test-model".into()),
            effort: None,
            now: system_clock(),
            cheap: None,
            host: Arc::new(HostState::new()),
            starter: Arc::new(RwLock::new(None)),
            turn_registry: Arc::new(TurnRegistry::new()),
            model_defaults_path: Some(
                std::env::temp_dir()
                    .join(format!("bough-exec-test-{}", uuid::Uuid::new_v4()))
                    .join("model.json"),
            ),
        };
        if let Some(turn) = options.turn {
            *ctx.starter.write().unwrap() = Some(Arc::new(FakeStarter(turn)));
        }
        let handler: Arc<Dispatcher> =
            Arc::new(create_handler(ctx.clone(), CreateHandlerOptions::default()));

        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let out = Arc::new(Mutex::new(String::new()));
        let err = Arc::new(Mutex::new(String::new()));

        let recorded = calls.clone();
        let real_fetch: ExecFetch = Arc::new(move |req: ExecRequest| {
            let handler = handler.clone();
            let recorded = recorded.clone();
            Box::pin(async move {
                let url: reqwest::Url = req.url.parse().map_err(|e| format!("{e}"))?;
                recorded
                    .lock()
                    .unwrap()
                    .push(format!("{} {}", req.method, url.path()));
                let path_and_query = match url.query() {
                    Some(q) => format!("{}?{}", url.path(), q),
                    None => url.path().to_string(),
                };
                let mut builder = axum::extract::Request::builder()
                    .method(req.method.as_str())
                    .uri(path_and_query);
                if req.body.is_some() {
                    builder = builder.header("content-type", "application/json");
                }
                let request = builder
                    .body(axum::body::Body::from(req.body.unwrap_or_default()))
                    .expect("a well-formed request");
                let res = handler.call(request).await;
                let status = res.status().as_u16();
                let chunks = res.into_body().into_data_stream().map(|item| match item {
                    Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
                    Err(e) => Err(e.to_string()),
                });
                Ok(ExecResponse {
                    status,
                    chunks: Box::pin(chunks),
                })
            }) as ExecFuture
        });

        let stdin = options.stdin.unwrap_or_default();
        let is_terminal = options.is_terminal.unwrap_or(true);
        let env = options.env;
        let sink_out = out.clone();
        let sink_err = err.clone();

        let deps = ExecDeps {
            fetch: options.fetch.unwrap_or(real_fetch),
            write: Arc::new(move |text: &str| sink_out.lock().unwrap().push_str(text)),
            warn: Arc::new(move |text: &str| {
                let mut buf = sink_err.lock().unwrap();
                buf.push_str(text);
                buf.push('\n');
            }),
            read_stdin: Arc::new(move || {
                let text = stdin.clone();
                Box::pin(async move { Ok(text) }) as StdinFuture
            }),
            stdin_is_terminal: Arc::new(move || is_terminal),
            env: Arc::new(move |name: &str| {
                env.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
            }),
            cwd: Arc::new(|| "/tmp".to_string()),
            real_path: options.real_path.unwrap_or_else(|| {
                Arc::new(|path: &str| {
                    std::fs::canonicalize(path)
                        .map(|p| p.to_string_lossy().into_owned())
                        .map_err(|e| e.to_string())
                })
            }),
        };

        Fixture {
            deps,
            calls,
            out,
            err,
            ctx,
        }
    }

    fn refusing_fetch(message: &'static str) -> ExecFetch {
        Arc::new(move |_req| Box::pin(async move { Err(message.to_string()) }) as ExecFuture)
    }

    // ---- THE ordering test -------------------------------------------------

    #[tokio::test]
    async fn a_turn_that_finishes_inside_the_post_is_still_seen_stream_before_post() {
        let f = fixture(FixtureOptions {
            turn: Some(instant_turn("the answer", "done", None)),
            ..Default::default()
        });
        let code = run_exec(&argv(&["--timeout", "1", "do the thing"]), &f.deps).await;
        assert_eq!(
            code,
            0,
            "expected a completed turn; stderr was: {}",
            f.err()
        );
        assert_eq!(f.out(), "the answer\n");

        // The ordering itself, stated as a fact about the call sequence.
        let calls = f.calls();
        let events = calls.iter().position(|c| c == "GET /events");
        let post = calls
            .iter()
            .position(|c| c.starts_with("POST /sessions/") && c.ends_with("/messages"));
        let events = events.unwrap_or_else(|| panic!("no /events call at all: {calls:?}"));
        let post = post.unwrap_or_else(|| panic!("no message post at all: {calls:?}"));
        assert!(
            events < post,
            "the event stream must be open BEFORE the prompt is posted, got: {calls:?}"
        );
        drop(f);
    }

    /// The inverted client: post, THEN subscribe. Not production code — it
    /// exists to demonstrate that the test above fails when the ordering is
    /// reversed, which is the only thing that makes that test worth having.
    async fn post_first(deps: &ExecDeps) -> (bool, String) {
        let base = "http://127.0.0.1:4321";
        let created = (deps.fetch)(ExecRequest {
            method: "POST".into(),
            url: format!("{base}/sessions"),
            body: Some(json!({ "title": "inverted", "workspace": "/tmp" }).to_string()),
        })
        .await
        .expect("session");
        let body: Value = serde_json::from_str(&created.text().await).unwrap();
        let id = body["id"].as_str().unwrap().to_string();

        let _ = (deps.fetch)(ExecRequest {
            method: "POST".into(),
            url: format!("{base}/sessions/{id}/messages"),
            body: Some(json!({ "text": "do the thing" }).to_string()),
        })
        .await
        .expect("post");

        let mut events = (deps.fetch)(ExecRequest {
            method: "GET".into(),
            url: format!("{base}/events?sessionId={id}"),
            body: None,
        })
        .await
        .expect("events");

        let mut feed = SseReader::new();
        let mut saw_finish = false;
        let mut text = String::new();
        // Bounded, because the whole point is that nothing is coming.
        let deadline = Instant::now() + Duration::from_millis(150);
        loop {
            let Ok(Some(Ok(chunk))) = tokio::time::timeout_at(deadline, events.chunks.next()).await
            else {
                break;
            };
            for frame in feed.push(&chunk) {
                let data = payload_of(&frame.data).unwrap_or(Value::Null);
                if frame.name == "message.delta" {
                    text.push_str(
                        data.get("delta")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    );
                }
                if frame.name == "turn.finished" {
                    saw_finish = true;
                }
            }
            if saw_finish {
                break;
            }
        }
        (saw_finish, text)
    }

    #[tokio::test]
    async fn proof_the_ordering_test_discriminates_post_then_subscribe_sees_nothing() {
        let f = fixture(FixtureOptions {
            turn: Some(instant_turn("the answer", "done", None)),
            ..Default::default()
        });
        let (saw_finish, text) = post_first(&f.deps).await;
        assert!(
            !saw_finish,
            "the inverted ordering observed turn.finished — this fixture no longer proves anything"
        );
        assert_eq!(
            text, "",
            "the inverted ordering observed the assistant text"
        );
    }

    // ---- exit codes --------------------------------------------------------

    #[tokio::test]
    async fn exit_0_a_completed_turn() {
        let f = fixture(FixtureOptions {
            turn: Some(instant_turn("done here", "done", None)),
            ..Default::default()
        });
        assert_eq!(run_exec(&argv(&["--timeout", "1", "go"]), &f.deps).await, 0);
        assert_eq!(f.out(), "done here\n");
    }

    #[tokio::test]
    async fn exit_1_an_errored_turn_with_the_servers_reason_on_stderr() {
        let f = fixture(FixtureOptions {
            turn: Some(instant_turn(
                "partial",
                "error",
                Some("context window exceeded: 200000"),
            )),
            ..Default::default()
        });
        assert_eq!(run_exec(&argv(&["--timeout", "1", "go"]), &f.deps).await, 1);
        assert!(
            f.err().contains("context window exceeded: 200000"),
            "{}",
            f.err()
        );
        // The partial answer still reaches stdout — it is what the model said.
        assert_eq!(f.out(), "partial\n");
    }

    #[tokio::test]
    async fn exit_1_an_interrupted_or_orphaned_turn_is_not_a_completed_turn() {
        for status in ["interrupted", "orphaned"] {
            let f = fixture(FixtureOptions {
                turn: Some(instant_turn("", status, None)),
                ..Default::default()
            });
            assert_eq!(
                run_exec(&argv(&["--timeout", "1", "go"]), &f.deps).await,
                1,
                "{status}"
            );
        }
    }

    #[tokio::test]
    async fn exit_1_the_timeout_elapses_and_the_abandoned_turn_is_interrupted() {
        // A turn that starts and never reports. The registry is claimed against,
        // so the interrupt the client raises has a real turn to signal — which is
        // the whole point: a `--timeout` that walks away without stopping the
        // turn leaves it running and spending, and the next command against that
        // session queues behind it.
        let claimed: Arc<Mutex<Option<tokio_util::sync::CancellationToken>>> =
            Arc::new(Mutex::new(None));
        let sink = claimed.clone();
        let f = fixture(FixtureOptions {
            turn: Some(Arc::new(move |ctx: &AppCtx, session: &Session| {
                let claim = ctx.turn_registry.begin(&session.id).unwrap();
                *sink.lock().unwrap() = Some(claim.cancel.clone());
                // The claim is deliberately leaked: the turn is "running" until
                // the interrupt lands, which is what this test is about.
                std::mem::forget(claim);
            })),
            ..Default::default()
        });
        assert_eq!(
            run_exec(&argv(&["--timeout", "0.15", "go"]), &f.deps).await,
            1
        );
        assert!(f.err().contains("timed out after 0.15s"), "{}", f.err());
        assert!(f.err().contains("interrupted the turn"), "{}", f.err());
        assert!(
            f.calls()
                .iter()
                .any(|c| c.starts_with("POST /sessions/") && c.ends_with("/interrupt")),
            "the client must raise the interrupt it promises: {:?}",
            f.calls()
        );
        let token = claimed.lock().unwrap().clone().expect("a claimed turn");
        assert!(token.is_cancelled(), "the turn's token must be cancelled");
    }

    #[tokio::test]
    async fn exit_1_a_stop_that_finds_nothing_running_says_so_and_still_exits_1() {
        // The race: the turn ended between the stream closing and the stop being
        // raised. The client must not claim it interrupted anything, and must not
        // change its mind about the exit code.
        let f = fixture(FixtureOptions {
            turn: Some(Arc::new(|_ctx: &AppCtx, _s: &Session| {})),
            ..Default::default()
        });
        assert_eq!(
            run_exec(&argv(&["--timeout", "0.15", "go"]), &f.deps).await,
            1
        );
        assert!(f.err().contains("could NOT interrupt"), "{}", f.err());
    }

    #[tokio::test]
    async fn exit_2_no_server_on_the_port() {
        let f = fixture(FixtureOptions {
            fetch: Some(refusing_fetch("connection refused")),
            ..Default::default()
        });
        assert_eq!(run_exec(&argv(&["--port", "4399", "go"]), &f.deps).await, 2);
        assert!(
            f.err().contains("cannot reach bough on :4399"),
            "{}",
            f.err()
        );
        assert!(f.err().contains("connection refused"), "{}", f.err());
    }

    #[tokio::test]
    async fn exit_2_the_server_refuses_the_session() {
        // A workspace that is not a directory is a 400 from `POST /sessions` —
        // a usage problem, reported as one rather than as a turn failure.
        let f = fixture(FixtureOptions {
            real_path: Some(Arc::new(|_p| Ok("/definitely/not/a/directory".to_string()))),
            ..Default::default()
        });
        assert_eq!(run_exec(&argv(&["-w", "/tmp", "go"]), &f.deps).await, 2);
        assert!(
            f.err().contains("bough refused the session: 400"),
            "{}",
            f.err()
        );
    }

    #[tokio::test]
    async fn exit_2_no_prompt_and_no_piped_stdin_to_take_one_from() {
        let f = fixture(FixtureOptions {
            is_terminal: Some(true),
            ..Default::default()
        });
        assert_eq!(run_exec(&[], &f.deps).await, 2);
        assert_eq!(f.err().trim(), USAGE);
        assert!(
            f.calls().is_empty(),
            "nothing is created before the prompt is known: {:?}",
            f.calls()
        );
    }

    #[tokio::test]
    async fn exit_2_an_unknown_flag_stops_rather_than_streaming() {
        let f = fixture(FixtureOptions::default());
        assert_eq!(run_exec(&argv(&["--jsno", "go"]), &f.deps).await, 2);
        assert!(f.err().contains("unknown flag --jsno"), "{}", f.err());
    }

    // ---- --json ------------------------------------------------------------

    #[tokio::test]
    async fn json_suppresses_streaming_and_prints_one_envelope_carrying_the_text() {
        let f = fixture(FixtureOptions {
            turn: Some(instant_turn("hello there", "done", None)),
            ..Default::default()
        });
        assert_eq!(
            run_exec(&argv(&["--json", "--timeout", "1", "go"]), &f.deps).await,
            0
        );
        let out = f.out();
        let lines: Vec<&str> = out.trim_end().split('\n').collect();
        assert_eq!(lines.len(), 1, "expected exactly one line, got: {out:?}");
        let envelope: ExecEnvelope = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(envelope.status, "done");
        assert!(envelope.ok);
        // Suppressed from stdout, not discarded: `--json` still answers.
        assert_eq!(envelope.text, "hello there");
        assert!(!envelope.session.is_empty());
        // Usage rides along from `GET /sessions/:id`, the authoritative record.
        assert!(
            envelope
                .usage
                .as_ref()
                .and_then(|u| u.get("inputTokens"))
                .is_some_and(Value::is_number),
            "{:?}",
            envelope.usage
        );
        assert!(
            envelope
                .tree_usage
                .as_ref()
                .and_then(|u| u.get("costUsd"))
                .is_some_and(Value::is_number),
            "{:?}",
            envelope.tree_usage
        );
        let wanted = format!("GET /sessions/{}", envelope.session);
        assert_eq!(f.calls().iter().filter(|c| **c == wanted).count(), 1);
    }

    #[tokio::test]
    async fn json_on_a_failed_turn_is_still_one_envelope_with_ok_false() {
        let f = fixture(FixtureOptions {
            turn: Some(instant_turn("", "error", Some("provider 500"))),
            ..Default::default()
        });
        assert_eq!(
            run_exec(&argv(&["--json", "--timeout", "1", "go"]), &f.deps).await,
            1
        );
        let envelope: ExecEnvelope = serde_json::from_str(f.out().trim()).unwrap();
        assert!(!envelope.ok);
        assert_eq!(envelope.status, "error");
        assert_eq!(envelope.error.as_deref(), Some("provider 500"));
    }

    // ---- the prompt --------------------------------------------------------

    fn first_message_text(ctx: &AppCtx) -> String {
        let db = ctx.db.lock().unwrap();
        let sessions = db.list_sessions().unwrap();
        let messages = db.messages_for(&sessions[0].id).unwrap();
        serde_json::to_value(&messages[0].parts[0]).unwrap()["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    #[tokio::test]
    async fn the_prompt_comes_from_stdin_when_the_positional_is_a_dash() {
        let f = fixture(FixtureOptions {
            turn: Some(instant_turn("ok", "done", None)),
            stdin: Some("  from a pipe  \n".into()),
            is_terminal: Some(true),
            ..Default::default()
        });
        assert_eq!(run_exec(&argv(&["--timeout", "1", "-"]), &f.deps).await, 0);
        // The posted text carries the turn's deadline ahead of the prompt;
        // what this test pins is the stdin handling, not the prefix.
        assert!(first_message_text(&f.ctx).ends_with("from a pipe"));
    }

    #[tokio::test]
    async fn the_prompt_comes_from_stdin_when_it_is_absent_and_stdin_is_piped() {
        let f = fixture(FixtureOptions {
            turn: Some(instant_turn("ok", "done", None)),
            stdin: Some("piped prompt".into()),
            is_terminal: Some(false),
            ..Default::default()
        });
        assert_eq!(run_exec(&argv(&["--timeout", "1"]), &f.deps).await, 0);
        // The posted text carries the turn's deadline ahead of the prompt;
        // what this test pins is the stdin handling, not the prefix.
        assert!(first_message_text(&f.ctx).ends_with("piped prompt"));
    }

    #[tokio::test]
    async fn the_turn_is_told_how_much_wall_clock_it_has() {
        let f = fixture(FixtureOptions {
            turn: Some(instant_turn("ok", "done", None)),
            ..Default::default()
        });
        assert_eq!(
            run_exec(&argv(&["--timeout", "42", "do the thing"]), &f.deps).await,
            0
        );
        let text = first_message_text(&f.ctx);
        // The number matters: a deadline the model cannot read is the bug this
        // fixes, and the prompt itself must still arrive intact after it.
        assert!(text.contains("42 seconds"), "{text}");
        assert!(text.ends_with("do the thing"), "{text}");
    }

    // ---- session shape -----------------------------------------------------

    #[tokio::test]
    async fn w_and_m_land_on_the_created_session_and_the_default_workspace_is_the_cwd() {
        let dir = std::env::temp_dir().join(format!("bough-exec-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let real = std::fs::canonicalize(&dir).unwrap();
        let f = fixture(FixtureOptions {
            turn: Some(instant_turn("ok", "done", None)),
            ..Default::default()
        });
        let code = run_exec(
            &argv(&[
                "-w",
                dir.to_str().unwrap(),
                "-m",
                "openai:gpt-5",
                "--timeout",
                "1",
                "go",
            ]),
            &f.deps,
        )
        .await;
        assert_eq!(code, 0, "{}", f.err());
        let session = {
            let db = f.ctx.db.lock().unwrap();
            db.list_sessions().unwrap()[0].clone()
        };
        assert_eq!(session.workspace.as_deref(), real.to_str());
        // `originDir` is the stable project record and mirrors the workspace.
        assert_eq!(session.origin_dir.as_deref(), real.to_str());
        assert_eq!(session.model.as_deref(), Some("openai:gpt-5"));
        assert_eq!(session.title, "exec: go");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn port_beats_bough_port_which_beats_the_built_in_default() {
        // Observed through the only thing that varies with the port: the URL the
        // client reports it could not reach.
        let cases: Vec<(Vec<&str>, Vec<(String, String)>, &str)> = vec![
            (
                vec!["--port", "4500", "go"],
                vec![("BOUGH_PORT".to_string(), "4600".to_string())],
                ":4500",
            ),
            (
                vec!["go"],
                vec![("BOUGH_PORT".to_string(), "4600".to_string())],
                ":4600",
            ),
            (vec!["go"], vec![], ":4321"),
        ];
        for (cli, env, expected) in cases {
            let f = fixture(FixtureOptions {
                env,
                fetch: Some(refusing_fetch("nope")),
                ..Default::default()
            });
            assert_eq!(run_exec(&argv(&cli), &f.deps).await, 2);
            assert!(f.err().contains(expected), "{} / {expected}", f.err());
        }
    }

    #[tokio::test]
    async fn a_bough_port_that_is_not_a_port_is_a_usage_error_before_any_request() {
        let f = fixture(FixtureOptions {
            env: vec![("BOUGH_PORT".to_string(), "banana".to_string())],
            ..Default::default()
        });
        assert_eq!(run_exec(&argv(&["go"]), &f.deps).await, 2);
        assert!(
            f.err().contains("BOUGH_PORT is not a port number: banana"),
            "{}",
            f.err()
        );
        assert!(f.calls().is_empty());
    }

    // ---- pure parsing ------------------------------------------------------

    #[test]
    fn parse_exec_args_the_flag_set_in_both_spellings() {
        let parsed = args_of(parse_exec_args(&argv(&[
            "-w",
            "/w",
            "--model=m",
            "--json",
            "--timeout",
            "30",
            "--port=4400",
            "the prompt",
        ])));
        assert_eq!(
            parsed,
            ExecArgs {
                prompt: "the prompt".into(),
                workspace: Some("/w".into()),
                model: Some("m".into()),
                json: true,
                timeout_ms: 30_000,
                port: Some(4400),
            }
        );
    }

    #[test]
    fn parse_exec_args_defaults() {
        let parsed = args_of(parse_exec_args(&argv(&["hi"])));
        assert!(!parsed.json);
        assert_eq!(parsed.timeout_ms, 900_000);
        assert_eq!(parsed.port, None);
        assert_eq!(parsed.workspace, None);
    }

    #[test]
    fn parse_exec_args_dash_is_the_stdin_sentinel_not_a_flag() {
        assert_eq!(args_of(parse_exec_args(&argv(&["-"]))).prompt, "-");
    }

    #[test]
    fn parse_exec_args_double_dash_ends_flag_parsing() {
        let parsed = args_of(parse_exec_args(&argv(&["--json", "--", "--not-a-flag"])));
        assert_eq!(parsed.prompt, "--not-a-flag");
        assert!(parsed.json);
    }

    #[test]
    fn parse_exec_args_a_value_flag_may_take_a_dash_leading_value() {
        let parsed = args_of(parse_exec_args(&argv(&["-m", "-weird-model", "go"])));
        assert_eq!(parsed.model.as_deref(), Some("-weird-model"));
        assert_eq!(parsed.prompt, "go");
    }

    #[test]
    fn parse_exec_args_a_forgotten_pair_of_quotes_is_an_error() {
        match parse_exec_args(&argv(&["write", "the", "tests"])) {
            ExecParse::UsageError(message) => {
                assert!(message.contains("quote it as a single string"), "{message}")
            }
            other => panic!("expected a usage error, got {other:?}"),
        }
    }

    #[test]
    fn parse_exec_args_rejects_the_malformed_rest() {
        let cases: Vec<(Vec<&str>, &str)> = vec![
            (vec!["--nope"], "unknown flag --nope"),
            (vec!["-q", "x"], "unknown flag -q"),
            (vec!["--json=1", "go"], "--json takes no value"),
            (vec!["--timeout"], "--timeout needs a value"),
            (vec!["--timeout", "0", "go"], "positive number of seconds"),
            (vec!["--timeout", "abc", "go"], "positive number of seconds"),
            (vec!["--port", "0", "go"], "wants a port number"),
            (vec!["--port", "99999", "go"], "wants a port number"),
            (vec!["--port", "x", "go"], "wants a port number"),
        ];
        for (cli, pattern) in cases {
            match parse_exec_args(&argv(&cli)) {
                ExecParse::UsageError(message) => {
                    assert!(message.contains(pattern), "{cli:?}: {message}")
                }
                other => panic!("{cli:?}: expected a usage error, got {other:?}"),
            }
        }
    }

    // ---- the SSE reader ----------------------------------------------------

    #[test]
    fn sse_reader_a_frame_split_across_chunks_is_not_read_until_it_is_whole() {
        let mut feed = SseReader::new();
        assert_eq!(feed.push("event: turn.fin"), vec![]);
        assert_eq!(feed.push("ished\ndata: {\"a\":1}"), vec![]);
        assert_eq!(
            feed.push("\n\n"),
            vec![SseFrame {
                name: "turn.finished".into(),
                data: json!({ "a": 1 })
            }]
        );
    }

    #[test]
    fn sse_reader_field_order_does_not_matter_and_comments_carry_nothing() {
        let mut feed = SseReader::new();
        let frames =
            feed.push(": connected\n\ndata: {\"a\":1}\nevent: message.delta\n\n: ping\n\n");
        assert_eq!(
            frames,
            vec![SseFrame {
                name: "message.delta".into(),
                data: json!({ "a": 1 })
            }]
        );
    }

    #[test]
    fn sse_reader_several_frames_in_one_chunk_and_a_malformed_one_is_dropped() {
        let mut feed = SseReader::new();
        let frames = feed.push(
            "event: a\ndata: 1\n\nevent: b\ndata: {oops\n\nevent: c\ndata: {\"ok\":true}\n\n",
        );
        assert_eq!(
            frames,
            vec![
                SseFrame {
                    name: "a".into(),
                    data: json!(1)
                },
                SseFrame {
                    name: "c".into(),
                    data: json!({ "ok": true })
                },
            ]
        );
    }

    #[test]
    fn sse_reader_crlf_framing_parses_the_same_as_lf() {
        let mut feed = SseReader::new();
        assert_eq!(
            feed.push("event: x\r\ndata: 2\r\n\r\n"),
            vec![SseFrame {
                name: "x".into(),
                data: json!(2)
            }]
        );
    }

    // ---- retry -------------------------------------------------------------

    #[tokio::test]
    async fn a_retry_announces_itself_and_drops_the_false_start_from_the_envelope() {
        let f = fixture(FixtureOptions {
            turn: Some(Arc::new(|ctx: &AppCtx, session: &Session| {
                let message_id = uuid::Uuid::new_v4().to_string();
                let sid = session.id.clone();
                ctx.bus.publish(EventInput {
                    r#type: EventType::MessageDelta,
                    session_id: Some(sid.clone()),
                    data: json!({ "messageId": message_id, "delta": "half an ans" }),
                });
                ctx.bus.publish(EventInput {
                    r#type: EventType::MessageRetry,
                    session_id: Some(sid.clone()),
                    data: json!({
                        "messageId": message_id,
                        "attempt": 2,
                        "reason": "tool input truncated mid-stream"
                    }),
                });
                ctx.bus.publish(EventInput {
                    r#type: EventType::MessageDelta,
                    session_id: Some(sid.clone()),
                    data: json!({ "messageId": message_id, "delta": "the real answer" }),
                });
                ctx.bus.publish(EventInput {
                    r#type: EventType::TurnFinished,
                    session_id: Some(sid.clone()),
                    data: json!({
                        "turnId": uuid::Uuid::new_v4().to_string(),
                        "sessionId": sid,
                        "status": "done"
                    }),
                });
            })),
            ..Default::default()
        });
        assert_eq!(
            run_exec(&argv(&["--json", "--timeout", "1", "go"]), &f.deps).await,
            0
        );
        let envelope: ExecEnvelope = serde_json::from_str(f.out().trim()).unwrap();
        assert_eq!(envelope.text, "the real answer");
        assert!(
            f.err()
                .contains("[retry 2: tool input truncated mid-stream]"),
            "{}",
            f.err()
        );
    }

    // ---- help and usage ----------------------------------------------------

    #[test]
    fn help_is_answered_not_rejected() {
        // It used to fall through to "unknown flag --help" and exit 2, so the
        // first thing anyone types at a new CLI printed an error.
        for cli in [vec!["--help"], vec!["-h"], vec!["-w", "/tmp", "--help"]] {
            assert_eq!(
                parse_exec_args(&argv(&cli)),
                ExecParse::Help,
                "{cli:?} should be a help request"
            );
        }
        // A prompt that merely CONTAINS the word is still a prompt.
        assert_ne!(
            parse_exec_args(&argv(&["help me refactor this"])),
            ExecParse::Help
        );
    }

    #[test]
    fn the_usage_text_names_every_flag_it_accepts_and_the_no_sandbox_posture() {
        for flag in [
            "--workspace",
            "--model",
            "--json",
            "--timeout",
            "--port",
            "--help",
        ] {
            assert!(USAGE.contains(flag), "usage does not document {flag}");
        }
        // A headless client is exactly where this is easiest to forget.
        assert!(USAGE.contains("no sandbox"));
    }

    // ---- the ask() decline -------------------------------------------------

    /// NOBODY IS HERE TO ANSWER. `exec` had no case for `ask.question`, so a
    /// program that called `ask()` — or any workflow launch, which raises an
    /// approval card by default — parked until `--timeout` elapsed and then
    /// exited 1 on work that was one answer from done.
    #[tokio::test]
    async fn a_question_raised_under_exec_is_declined_not_waited_out() {
        let f = fixture(FixtureOptions {
            turn: Some(Arc::new(|ctx: &AppCtx, session: &Session| {
                let sid = session.id.clone();
                ctx.bus.publish(EventInput {
                    r#type: EventType::AskQuestion,
                    session_id: Some(sid.clone()),
                    data: json!({
                        "id": "q-1",
                        "sessionId": sid,
                        "messageId": uuid::Uuid::new_v4().to_string(),
                        "question": "Run the workflow \"audit\"?\nit fans out agents",
                        "status": "pending",
                        "ts": 1
                    }),
                });
                // The program's `ask()` rejects and the turn ends. A real turn
                // does this because the decline reaches it; the fixture states
                // the outcome directly.
                ctx.bus.publish(EventInput {
                    r#type: EventType::TurnFinished,
                    session_id: Some(sid.clone()),
                    data: json!({
                        "turnId": uuid::Uuid::new_v4().to_string(),
                        "sessionId": sid,
                        "status": "done"
                    }),
                });
            })),
            ..Default::default()
        });
        assert_eq!(run_exec(&argv(&["do the thing"]), &f.deps).await, 0);
        // It said so, on stderr, quoting the first line of what it refused.
        assert!(f.err().contains("declined a question"), "{}", f.err());
        assert!(f.err().contains("Run the workflow"), "{}", f.err());
        // And it actually posted the decline rather than only complaining.
        tokio::task::yield_now().await;
        assert!(
            f.calls()
                .iter()
                .any(|c| c.starts_with("POST") && c.contains("/questions/q-1")),
            "{:?}",
            f.calls()
        );
    }
}
