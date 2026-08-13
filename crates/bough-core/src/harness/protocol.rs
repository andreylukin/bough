//! The worker protocol declarations (port of `src/harness/protocol.ts`).
//!
//! The invariant: **host names are declared here exactly once, and both sides
//! import them.** The host side dispatches on them, the worker side binds them
//! as program parameters, and the pre-flight syntax check uses the same list
//! to reject a program that shadows one (`let bash = 1`). The list is CLOSED.
//! The wire is string-only, both directions. Declaring a name here does not
//! grant it: a host function exists in a program only when the turn bridges it
//! AND the system prompt documents it.
//!
//! There is NO `history`, `image`, `fetch`, or `recall` verb (the stale TS
//! header comment was not ported; commit 50d65da0 removed `history`).

use serde::{Deserialize, Serialize};

/// The closed, ordered 19-name list, exactly as on the wire.
pub const HOST_FN_NAMES: [&str; 19] = [
    // shell
    "bash",
    "sh",
    "bashBg",
    "bashOutput",
    "bashWait",
    "bashKill",
    // files — the one editing idiom
    "view",
    "patch",
    "write",
    // delegation
    "agent",
    "spawn",
    "join",
    "adopt",
    // orchestration
    "workflow",
    // session verbs
    "ask",
    "state",
    "schedule",
    "artifact",
    "mcp",
];

/// The typed mirror of [`HOST_FN_NAMES`]. `types::HostFns::get` matches it
/// exhaustively (no default arm) — the drift pin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HostFnName {
    Bash,
    Sh,
    BashBg,
    BashOutput,
    BashWait,
    BashKill,
    View,
    Patch,
    Write,
    Agent,
    Spawn,
    Join,
    Adopt,
    Workflow,
    Ask,
    State,
    Schedule,
    Artifact,
    Mcp,
}

impl HostFnName {
    pub fn as_str(&self) -> &'static str {
        match self {
            HostFnName::Bash => "bash",
            HostFnName::Sh => "sh",
            HostFnName::BashBg => "bashBg",
            HostFnName::BashOutput => "bashOutput",
            HostFnName::BashWait => "bashWait",
            HostFnName::BashKill => "bashKill",
            HostFnName::View => "view",
            HostFnName::Patch => "patch",
            HostFnName::Write => "write",
            HostFnName::Agent => "agent",
            HostFnName::Spawn => "spawn",
            HostFnName::Join => "join",
            HostFnName::Adopt => "adopt",
            HostFnName::Workflow => "workflow",
            HostFnName::Ask => "ask",
            HostFnName::State => "state",
            HostFnName::Schedule => "schedule",
            HostFnName::Artifact => "artifact",
            HostFnName::Mcp => "mcp",
        }
    }

    pub fn parse(name: &str) -> Option<HostFnName> {
        Some(match name {
            "bash" => HostFnName::Bash,
            "sh" => HostFnName::Sh,
            "bashBg" => HostFnName::BashBg,
            "bashOutput" => HostFnName::BashOutput,
            "bashWait" => HostFnName::BashWait,
            "bashKill" => HostFnName::BashKill,
            "view" => HostFnName::View,
            "patch" => HostFnName::Patch,
            "write" => HostFnName::Write,
            "agent" => HostFnName::Agent,
            "spawn" => HostFnName::Spawn,
            "join" => HostFnName::Join,
            "adopt" => HostFnName::Adopt,
            "workflow" => HostFnName::Workflow,
            "ask" => HostFnName::Ask,
            "state" => HostFnName::State,
            "schedule" => HostFnName::Schedule,
            "artifact" => HostFnName::Artifact,
            "mcp" => HostFnName::Mcp,
            _ => return None,
        })
    }
}

/// The program's parameter names: every host function, plus `console` and
/// `require` (both worker-side constructs, not bridged calls — `console`
/// streams to the UI AND batches into the tool result; `require` is the other
/// door to `node:*`/`npm:` reach that weak models write constantly).
pub fn program_params() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = HOST_FN_NAMES.to_vec();
    v.push("console");
    v.push("require");
    v
}

/// One function an extension file exports, as the worker found it.
///
/// The worker reports these instead of Rust parsing JavaScript: the engine
/// that will bind the name is the one that says what the name is. Both
/// consumers — the prompt section and the worker's parameter list — read this
/// one list, which is why this surface has no hand-synced second half
/// (`crate::extensions`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ExtensionFn {
    pub name: String,
    /// The declared parameter list, e.g. `(owner, repo)`. `()` when the
    /// engine's `toString` shape was not one we could read.
    pub signature: String,
    /// The `doc` property on the exported function, if it set one. Optional
    /// because requiring it would make the zero-config case a failure case.
    #[serde(default)]
    pub doc: Option<String>,
    /// Which file it came from — the answer to "why is this in my prompt".
    pub file: String,
}

/// The verbs each method-object host function fans out to. One bridged
/// function carries all of them (`state("get", argsJson)`); declared here so
/// the host dispatcher and the worker's method-object construction cannot
/// drift.
pub const STATE_VERBS: [&str; 4] = ["get", "set", "list", "delete"];
pub const SCHEDULE_VERBS: [&str; 5] = ["list", "add", "enable", "disable", "remove"];
pub const WORKFLOW_VERBS: [&str; 7] = [
    "start", "rerun", "stop", "pause", "resume", "status", "list",
];
/// `call` invokes a tool with a real object; `list` is the live catalog. Both
/// exist so that reaching an MCP server never requires composing JSON inside a
/// shell word — the failure that accounted for 267 of 1,848 field calls.
pub const MCP_VERBS: [&str; 2] = ["call", "list"];

// ---- program worker: host → worker ------------------------------------------

/// Host → program worker, as NDJSON lines on the sidecar's stdin.
///
/// `Check` is the one Rust-era addition to the TS wire: the pre-flight parse is
/// delegated to the sidecar (a `check` message before `run`) so the
/// shadow/unterminated-string error messages come from the very engine that
/// will compile the program (ARCHITECTURE §4.1, option (b) in the spec).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToProgramWorker {
    /// Load extension files and bind their exports into the program's scope.
    /// Sent BEFORE `check`, because the names they add are names the program
    /// may legally use — pre-flighting first would reject a valid program.
    /// Answered with `extensions_result`.
    Extensions { files: Vec<String> },
    /// Start the program. Sent once, after the pre-flight check passes.
    Run { code: String },
    /// Parse-only pre-flight; answered with `check_result`, never executes.
    Check { code: String },
    /// The result of one bridged call. `ok: false` rejects the program's
    /// promise with `value` as the message — host-function failures are
    /// ordinary catchable exceptions inside the program, never a killed worker.
    HostResult { id: u64, ok: bool, value: String },
    /// Stop. The worker kills the processes it spawned and acks with
    /// `aborted`; only then does the host terminate it. Reverse order orphans
    /// processes.
    Abort,
}

// ---- program worker: worker → host ------------------------------------------

/// Program worker → host, as NDJSON lines on the sidecar's stdout.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromProgramWorker {
    /// One bridged call. `fn` stays a raw string: the worker global is
    /// program-reachable, so the host validates it against [`HOST_FN_NAMES`]
    /// before dispatching. `args` are strings by convention.
    Host {
        id: u64,
        #[serde(rename = "fn")]
        fn_name: String,
        #[serde(default)]
        args: Vec<serde_json::Value>,
    },
    /// One `console.*` line, as printed. Streamed live AND kept in the batch.
    Log {
        line: String,
    },
    /// Children swept; safe to terminate. The host waits briefly for this.
    Aborted,
    Done {
        logs: Vec<String>,
    },
    Error {
        message: String,
        logs: Vec<String>,
    },
    /// Answer to `extensions`: what got bound, and what did not. `errors` is
    /// never fatal — a broken extension file costs the program that file's
    /// functions, not its turn.
    ExtensionsResult {
        #[serde(default)]
        fns: Vec<ExtensionFn>,
        #[serde(default)]
        errors: Vec<String>,
    },
    /// Answer to `check`: absent `message` = the program parses. `name` is the
    /// engine's error class (`SyntaxError` in practice).
    CheckResult {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        message: Option<String>,
    },
}

/// What `run_program` resolves to. `logs` is what the model receives as the
/// tool result.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ProgramResult {
    pub ok: bool,
    /// `console.*` output, in order. Partial output survives an interrupt.
    pub logs: Vec<String>,
    /// Present when `ok` is false: the thrown error with its stack, the
    /// timeout notice, or the interrupt notice. Timeout and interrupt must be
    /// distinguishable, and must say what partial work survived (spec §6).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// `Some(true)` only when the program was stopped by a user interrupt —
    /// never set on a timeout.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interrupted: Option<bool>,
}

// ---- workflow worker --------------------------------------------------------

/// The workflow worker bridges only these three. `parallel` and `pipeline` are
/// NOT here — they are pure combinators over `agent`, implemented worker-side,
/// so they never cross the wire (spec §8).
pub const WORKFLOW_HOST_FN_NAMES: [&str; 3] = ["agent", "phase", "log"];

/// The script's parameter names: the three verbs plus its input value.
pub fn workflow_script_params() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = WORKFLOW_HOST_FN_NAMES.to_vec();
    v.push("args");
    v
}

/// Host → workflow worker. `args_json` is the run's input, handed over
/// verbatim as `args`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToWorkflowWorker {
    Run {
        code: String,
        #[serde(rename = "argsJson")]
        args_json: String,
    },
    Check {
        code: String,
    },
    HostResult {
        id: u64,
        ok: bool,
        value: String,
    },
    Abort,
}

/// Workflow worker → host. `pos` is the call's STRUCTURAL COORDINATE in the
/// script — dot-joined slot indexes (format `\d+(\.\d+)*`), present on `agent`
/// calls only. The host treats it as opaque ordering, compared component-wise
/// as numbers, never as text, and falls back to its own counter when absent.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromWorkflowWorker {
    Host {
        id: u64,
        #[serde(rename = "fn")]
        fn_name: String,
        #[serde(default)]
        args: Vec<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pos: Option<String>,
    },
    Aborted,
    /// The script returned. `result_json` is its return value.
    Done {
        #[serde(rename = "resultJson")]
        result_json: String,
    },
    Error {
        message: String,
        logs: Vec<String>,
    },
    CheckResult {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        message: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_fn_names_19_and_round_trip() {
        assert_eq!(HOST_FN_NAMES.len(), 19);
        for n in HOST_FN_NAMES {
            assert_eq!(HostFnName::parse(n).unwrap().as_str(), n);
        }
        // The removed verb stays removed.
        assert!(HostFnName::parse("history").is_none());
        assert!(HostFnName::parse("lsp").is_none());
    }

    #[test]
    fn program_params_adds_console_and_require() {
        let p = program_params();
        assert_eq!(p.len(), 21);
        assert!(p.contains(&"console") && p.contains(&"require"));
    }

    #[test]
    fn wire_shapes_match_the_ts_protocol_verbatim() {
        // Host → worker.
        assert_eq!(
            serde_json::to_string(&ToProgramWorker::Run { code: "x".into() }).unwrap(),
            r#"{"type":"run","code":"x"}"#
        );
        assert_eq!(
            serde_json::to_string(&ToProgramWorker::HostResult {
                id: 7,
                ok: true,
                value: "hi".into()
            })
            .unwrap(),
            r#"{"type":"host_result","id":7,"ok":true,"value":"hi"}"#
        );
        assert_eq!(
            serde_json::to_string(&ToProgramWorker::Abort).unwrap(),
            r#"{"type":"abort"}"#
        );

        // Worker → host, exactly as the TS worker posts them.
        let host: FromProgramWorker =
            serde_json::from_str(r#"{"type":"host","id":7,"fn":"bash","args":["echo hi",""]}"#)
                .unwrap();
        assert_eq!(
            host,
            FromProgramWorker::Host {
                id: 7,
                fn_name: "bash".into(),
                args: vec!["echo hi".into(), "".into()],
            }
        );
        let log: FromProgramWorker =
            serde_json::from_str(r#"{"type":"log","line":"one line as printed"}"#).unwrap();
        assert_eq!(
            log,
            FromProgramWorker::Log {
                line: "one line as printed".into()
            }
        );
        let aborted: FromProgramWorker = serde_json::from_str(r#"{"type":"aborted"}"#).unwrap();
        assert_eq!(aborted, FromProgramWorker::Aborted);
        let done: FromProgramWorker =
            serde_json::from_str(r#"{"type":"done","logs":["a","b"]}"#).unwrap();
        assert_eq!(
            done,
            FromProgramWorker::Done {
                logs: vec!["a".into(), "b".into()]
            }
        );
        let err: FromProgramWorker =
            serde_json::from_str(r#"{"type":"error","message":"boom","logs":["a"]}"#).unwrap();
        assert_eq!(
            err,
            FromProgramWorker::Error {
                message: "boom".into(),
                logs: vec!["a".into()]
            }
        );

        // Workflow worker `pos` rides on `host` for `agent` only.
        let wf: FromWorkflowWorker = serde_json::from_str(
            r#"{"type":"host","id":1,"fn":"agent","args":["t","{}"],"pos":"0.1.1.0"}"#,
        )
        .unwrap();
        assert_eq!(
            wf,
            FromWorkflowWorker::Host {
                id: 1,
                fn_name: "agent".into(),
                args: vec!["t".into(), "{}".into()],
                pos: Some("0.1.1.0".into()),
            }
        );
    }

    #[test]
    fn program_result_field_presence_matches_ts() {
        // `error`/`interrupted` are absent when unset — the turn runner
        // persists this shape and the TS tests pin `undefined`, not `null`.
        let ok = ProgramResult {
            ok: true,
            logs: vec!["a".into()],
            error: None,
            interrupted: None,
        };
        assert_eq!(
            serde_json::to_string(&ok).unwrap(),
            r#"{"ok":true,"logs":["a"]}"#
        );
        let stopped = ProgramResult {
            ok: false,
            logs: vec![],
            error: Some("program interrupted by the user".into()),
            interrupted: Some(true),
        };
        assert_eq!(
            serde_json::to_string(&stopped).unwrap(),
            r#"{"ok":false,"logs":[],"error":"program interrupted by the user","interrupted":true}"#
        );
    }

    #[test]
    fn workflow_lists_are_the_spec_lists() {
        assert_eq!(WORKFLOW_HOST_FN_NAMES, ["agent", "phase", "log"]);
        assert_eq!(
            workflow_script_params(),
            vec!["agent", "phase", "log", "args"]
        );
        assert_eq!(STATE_VERBS.len(), 4);
        assert_eq!(SCHEDULE_VERBS.len(), 5);
        assert_eq!(WORKFLOW_VERBS.len(), 7);
    }
}
