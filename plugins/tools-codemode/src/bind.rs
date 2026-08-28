//! Invariant: the surface the model READS and the surface it GETS are built from one snapshot.
//! Every injected global is a registered `ToolSpec` visible in the agent's scope, and nothing
//! else is injected.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use bough_kernel::Context;
use bough_plugin_js::{HostCall, HostFn, HostRefusal, RefusalKind};
use bough_plugin_ledger::{AgentName, Append, Cite, Class, LedgerHandle, StepType, TrajId, WakeId};
use bough_plugin_tools::{
    FailureClass, ToolCall, ToolCallId, ToolName, ToolOutcomeKind, ToolSpec, ToolsHandle,
};
use tokio_util::sync::CancellationToken;

use crate::vocabulary::{ProgramCallBody, ProgramResultBody};

/// A tool's registered name mapped onto the JS identifier the sandbox injects.
#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    /// The JS name, possibly dotted (`ledger.search`, `bg.output`).
    pub js: String,
    /// The registered `ToolName` the call resolves to.
    pub tool: String,
}

/// Why a name could not be injected.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum BindError {
    #[error("`{0}` is not a legal JS identifier path and cannot be injected")]
    NotAnIdentifier(String),
    #[error("`{js}` is claimed by both `{a}` and `{b}`")]
    Collision { js: String, a: String, b: String },
}

/// Words a global may not be named, because binding them would shadow the language.
const RESERVED: &[&str] = &[
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "let",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
    "console",
    "globalThis",
];

/// Is `s` one legal, non-reserved JS identifier?
fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_' || first == '$') {
        return false;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$') {
        return false;
    }
    !RESERVED.contains(&s)
}

/// Is `s` a dotted path of legal identifiers? Only the ROOT is checked against the reserved
/// list: `ledger.new` is fine as a member, `new(…)` as a global is not.
fn is_identifier_path(s: &str) -> bool {
    let mut parts = s.split('.');
    let Some(root) = parts.next() else {
        return false;
    };
    if !is_identifier(root) {
        return false;
    }
    parts.all(is_member)
}

/// A member name may be a reserved word (`ledger.new` is legal), but must still be an identifier.
fn is_member(s: &str) -> bool {
    let mut c = s.chars();
    match c.next() {
        Some(f) if f.is_ascii_alphabetic() || f == '_' || f == '$' => {
            c.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        }
        _ => false,
    }
}

/// Turn the visible specs plus the row's aliases and namespaces into the binding list.
/// A dotted name builds a namespace object; a name that is both a function and a namespace root
/// becomes a callable object.
///
/// Rules, in order of precedence:
/// 1. an ALIAS (`{js: tool}`) replaces the tool's default name — several aliases for one tool are
///    several bindings, and the default name is then not injected;
/// 2. a NAMESPACE (`{js_ns: name_prefix}`) with a NON-EMPTY prefix claims every remaining tool
///    whose name starts with it (longest prefix wins), rendering `mcp__srv__t` as `mcp.srv.t`;
/// 3. everything else is injected under its registered name.
///
/// A namespace whose prefix is EMPTY claims nothing: an empty prefix matches every name, which
/// would silently swallow the whole surface into one object. Group those with dotted aliases
/// instead (`act.open_pr: open_pr`). This is a deviation from the plan's `{act: ""}` example and
/// is recorded in `docs/codemode-merge-notes.md`.
pub fn bindings(
    specs: &[ToolSpec],
    aliases: &BTreeMap<String, String>,
    namespaces: &BTreeMap<String, String>,
) -> Result<Vec<Binding>, BindError> {
    let visible: BTreeSet<String> = specs.iter().map(|s| s.name.to_string()).collect();
    let mut out: Vec<Binding> = Vec::new();
    let mut aliased: BTreeSet<String> = BTreeSet::new();

    for (js, tool) in aliases {
        if !visible.contains(tool) {
            // An alias for a tool the agent cannot see is simply not there — the same answer a
            // restriction gives, so an alias is never a probe for what exists elsewhere.
            continue;
        }
        aliased.insert(tool.clone());
        out.push(Binding {
            js: js.clone(),
            tool: tool.clone(),
        });
    }

    for spec in specs {
        let name = spec.name.to_string();
        if aliased.contains(&name) {
            continue;
        }
        let ns = namespaces
            .iter()
            .filter(|(_, prefix)| !prefix.is_empty() && name.starts_with(prefix.as_str()))
            .max_by_key(|(_, prefix)| prefix.len());
        let js = match ns {
            Some((ns, prefix)) => {
                let rest = name[prefix.len()..].replace("__", ".");
                format!("{ns}.{rest}")
            }
            None => name.clone(),
        };
        out.push(Binding { js, tool: name });
    }

    for b in &out {
        if !is_identifier_path(&b.js) {
            return Err(BindError::NotAnIdentifier(b.js.clone()));
        }
    }
    out.sort_by(|a, b| a.js.cmp(&b.js));
    for pair in out.windows(2) {
        if pair[0].js == pair[1].js {
            return Err(BindError::Collision {
                js: pair[0].js.clone(),
                a: pair[0].tool.clone(),
                b: pair[1].tool.clone(),
            });
        }
    }
    Ok(out)
}

/// Everything one program's host functions share. One per `run` call.
pub struct ProgramCx {
    pub ctx: Context,
    pub ledger: LedgerHandle,
    pub traj: TrajId,
    pub wake: WakeId,
    pub agent: AgentName,
    pub step_index: u32,
    /// The `run` call this program is.
    pub program: ToolCallId,
    /// The snapshot registry every inner call executes against — the SAME pipeline.
    pub mirror: ToolsHandle,
    pub cancel: CancellationToken,
    pub max_calls: u32,
    pub tags_required: bool,
    next: AtomicU32,
    /// The seam's barrier rule, reproduced inside one program: a concurrency-safe call takes a
    /// READ, everything else takes a WRITE.
    gate: tokio::sync::RwLock<()>,
    state: parking_lot::Mutex<ProgramState>,
}

/// What the program accumulated, read once it ends.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProgramState {
    /// The `index` of every `program/call` appended, in issue order.
    pub calls: Vec<u32>,
    /// The `index` of every `program/result` appended, in append order.
    pub results: Vec<u32>,
    pub cites: Vec<Cite>,
    /// `run` reports `concludes_wake` only if an inner result did.
    pub concludes_wake: bool,
    /// Set when `max_calls_per_program` was breached; a terminal error for the program.
    pub cap_breach: Option<String>,
}

impl ProgramCx {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ctx: Context,
        ledger: LedgerHandle,
        traj: TrajId,
        wake: WakeId,
        agent: AgentName,
        step_index: u32,
        program: ToolCallId,
        mirror: ToolsHandle,
        cancel: CancellationToken,
        max_calls: u32,
        tags_required: bool,
    ) -> Arc<ProgramCx> {
        Arc::new(ProgramCx {
            ctx,
            ledger,
            traj,
            wake,
            agent,
            step_index,
            program,
            mirror,
            cancel,
            max_calls,
            tags_required,
            next: AtomicU32::new(0),
            gate: tokio::sync::RwLock::new(()),
            state: parking_lot::Mutex::new(ProgramState::default()),
        })
    }

    pub fn state(&self) -> ProgramState {
        self.state.lock().clone()
    }
}

/// Build the `HostFn` for one binding. Each body mints the deterministic `{run}.{n}` call id,
/// appends `program/call`, runs the mirror's pipeline, appends `program/result`, and answers.
pub fn host_fn(b: &Binding, spec: &ToolSpec, cx: Arc<ProgramCx>) -> HostFn {
    HostFn {
        arity: arity_of(spec),
        name: b.js.clone(),
        body: Arc::new(Injected {
            spec: spec.clone(),
            cx,
        }),
    }
}

/// How many arguments the JS signature takes: one per declared property, so `bash(cmd, tags)`
/// reports 2 and a schema-less tool reports 1 (the options object).
///
/// A shell tool whose schema carries no `tags` property still takes the tag argument — it is a
/// CODE-MODE parameter, not one of the tool's (see [`shell_tags`]) — so its arity is one more
/// than its schema declares.
fn arity_of(spec: &ToolSpec) -> u8 {
    let declared = properties(spec.input_schema.as_value())
        .map(|p| p.len().min(u8::MAX as usize) as u8)
        .unwrap_or(1);
    if takes_a_tag_argument(spec) {
        declared.saturating_add(1)
    } else {
        declared
    }
}

/// `bash`/`sh`, as the surface spells them.
fn is_shell(name: &str) -> bool {
    name == "bash" || name == "sh"
}

/// Does this tool take its tags as an EXTRA JS argument rather than a schema property?
///
/// The tags on a shell command are a harness fact, not a tool argument: they index the command in
/// the cross-session tag history, and the tool that runs it neither needs nor declares them.
/// `tools-baseline`'s `bash` is `{command, cwd}`, so without this the second positional argument
/// of `bash("echo hi", "echo:probe:demo")` bound to `cwd`, `tags_of` found nothing, and every
/// shell call in the sandbox was refused with `tags_required` on
/// (`docs/codemode-merge-notes.md` §9). A shell tool that DOES declare `tags` keeps binding it
/// positionally, so a future Provider can own the field.
fn takes_a_tag_argument(spec: &ToolSpec) -> bool {
    is_shell(spec.name.as_str())
        && !properties(spec.input_schema.as_value())
            .map(|p| p.contains_key("tags"))
            .unwrap_or(false)
}

/// Split the tag argument the surface documents into tags.
///
/// `"git:push:main"` is three tags; an array is taken as written; anything else is no tags. Both
/// spellings are accepted because `surface/shell.md` teaches the colon-separated string (main's
/// own spelling, restored verbatim) while a tool that declares `tags` declares an array.
pub fn parse_tags(v: &serde_json::Value) -> Vec<String> {
    match v {
        serde_json::Value::String(s) => s
            .split(':')
            .filter(|p| !p.is_empty())
            .map(|p| p.to_string())
            .collect(),
        serde_json::Value::Array(a) => a.iter().flat_map(parse_tags).collect(),
        _ => Vec::new(),
    }
}

/// Take the trailing tag argument off a shell call, returning the tags it carried.
///
/// Only when the tool does not declare `tags` itself, and only from the LAST argument: a program
/// that passed no tags leaves `args` untouched and is refused by the tag rule, which is the point.
fn shell_tags(spec: &ToolSpec, args: &mut Vec<serde_json::Value>) -> Vec<String> {
    if !takes_a_tag_argument(spec) {
        return Vec::new();
    }
    // `sh([{cmd, tag}, …])` carries its tags per leg, inside its one argument.
    if spec.name.as_str() == "sh" {
        return args
            .first()
            .and_then(|a| a.as_array())
            .map(|legs| {
                legs.iter()
                    .flat_map(|leg| {
                        leg.get("tag")
                            .or_else(|| leg.get("tags"))
                            .map(parse_tags)
                            .unwrap_or_default()
                    })
                    .collect()
            })
            .unwrap_or_default();
    }
    // `bash(cmd, tags)`: the surface puts the tags SECOND, before any further schema property
    // (`cwd`), so that is where they are taken from. A second argument that is neither a string
    // nor an array is not a tag argument and is left where the program put it.
    if args.len() >= 2 && (args[1].is_string() || args[1].is_array()) {
        let tag = args.remove(1);
        return parse_tags(&tag);
    }
    Vec::new()
}

fn properties(schema: &serde_json::Value) -> Option<&serde_json::Map<String, serde_json::Value>> {
    schema.get("properties").and_then(|p| p.as_object())
}

/// The order positional JS arguments map onto schema properties: the `required` list first, in
/// the order the schema declares it, then every other property in name order. Deterministic and
/// independent of `serde_json`'s map ordering, which is what makes `bash(cmd, tags)` legal.
pub fn positional_order(schema: &serde_json::Value) -> Vec<String> {
    let Some(props) = properties(schema) else {
        return Vec::new();
    };
    let mut order: Vec<String> = Vec::new();
    if let Some(req) = schema.get("required").and_then(|r| r.as_array()) {
        for name in req.iter().filter_map(|v| v.as_str()) {
            if props.contains_key(name) {
                order.push(name.to_string());
            }
        }
    }
    let mut rest: Vec<String> = props
        .keys()
        .filter(|k| !order.contains(k))
        .cloned()
        .collect();
    rest.sort();
    order.extend(rest);
    order
}

/// Turn the JS call's positional arguments into the tool's object arguments.
///
/// One object argument that already carries every required field is passed through untouched —
/// `claim({kind, title})` — and anything else is zipped onto [`positional_order`].
pub fn positional_args(
    schema: &serde_json::Value,
    args: Vec<serde_json::Value>,
) -> serde_json::Value {
    let order = positional_order(schema);
    if args.len() == 1 {
        if let Some(obj) = args[0].as_object() {
            let required: Vec<&str> = schema
                .get("required")
                .and_then(|r| r.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            let complete = required.iter().all(|r| obj.contains_key(*r));
            // A single object that satisfies the schema is the object; one that does not is a
            // first positional argument (an options bag for a one-parameter tool).
            if complete
                && (required.len() != 1 || order.first().map(|f| obj.contains_key(f)) == Some(true))
            {
                return serde_json::Value::Object(obj.clone());
            }
        }
    }
    let mut out = serde_json::Map::new();
    for (name, value) in order.iter().zip(args) {
        if !value.is_null() {
            out.insert(name.clone(), value);
        }
    }
    serde_json::Value::Object(out)
}

/// The tags a `bash`/`sh` leg carried, for the `program/call` step.
fn tags_of(args: &serde_json::Value) -> Vec<String> {
    args.get("tags").map(parse_tags).unwrap_or_default()
}

/// One injected global.
struct Injected {
    spec: ToolSpec,
    cx: Arc<ProgramCx>,
}

#[async_trait::async_trait]
impl HostCall for Injected {
    async fn call(&self, args: Vec<serde_json::Value>) -> Result<serde_json::Value, HostRefusal> {
        let cx = &self.cx;
        let name = self.spec.name.to_string();
        let mut args = args;
        // Off the front, before binding: the tag argument is code mode's, not the tool's.
        let extra = shell_tags(&self.spec, &mut args);
        let argv = positional_args(self.spec.input_schema.as_value(), args);
        let tags = if extra.is_empty() {
            tags_of(&argv)
        } else {
            extra
        };

        // The tag rule is a REFUSAL, not a step: a leg that never ran is not a call.
        if cx.tags_required && (name == "bash" || name == "sh") && !(3..=5).contains(&tags.len()) {
            return Err(HostRefusal {
                kind: RefusalKind::Denied,
                message: format!(
                    "`{name}` needs 3–5 tags naming what this command is about; it carried {}",
                    tags.len()
                ),
            });
        }

        let index = cx.next.fetch_add(1, Ordering::SeqCst);
        if index >= cx.max_calls {
            let message = format!(
                "the program made more than {} tool calls; split the work across rounds",
                cx.max_calls
            );
            cx.state.lock().cap_breach = Some(message.clone());
            return Err(HostRefusal {
                kind: RefusalKind::Blocked,
                message,
            });
        }

        let call_id = ToolCallId::new(crate::run::inner_call_id(cx.program.as_str(), index));
        let body = ProgramCallBody {
            program: cx.program.clone(),
            index,
            call: call_id.clone(),
            name: self.spec.name.clone(),
            args: argv.clone(),
            render: self.spec.render,
            tags,
            step_index: cx.step_index,
        };
        append(cx, "program/call", Class::Thought, to_body(&body)?, vec![]).await?;
        cx.state.lock().calls.push(index);

        let call = ToolCall {
            id: call_id.clone(),
            name: self.spec.name.clone(),
            args: argv.clone(),
            agent: cx.agent.clone(),
            wake: cx.wake.clone(),
            step_index: cx.step_index,
        };
        let safe = self.spec.tool.is_concurrency_safe(&argv);
        let started = std::time::Instant::now();
        let result = {
            // The seam's barrier rule, inside one program: `Promise.all` over safe calls
            // overlaps, and anything else is exclusive for its whole run.
            let _guard: Guard = if safe {
                Guard::Read(cx.gate.read().await)
            } else {
                Guard::Write(cx.gate.write().await)
            };
            cx.mirror
                .execute_under(&cx.ctx, vec![call], cx.cancel.clone())
                .await
                .into_iter()
                .next()
        };
        let Some(result) = result else {
            return Err(HostRefusal {
                kind: RefusalKind::Error,
                message: format!("the pipeline returned no result for `{name}`"),
            });
        };
        let ms = started.elapsed().as_millis() as u64;

        let outcome = outcome_of(&result);
        let cites = result.cites.clone();
        let body = ProgramResultBody {
            program: cx.program.clone(),
            index,
            call: call_id,
            name: self.spec.name.clone(),
            outcome,
            content: result.content.clone(),
            value: result.value.clone(),
            attached: result.attached.clone(),
            concludes_wake: result.concludes_wake,
            step_index: cx.step_index,
            ms,
        };
        // `program/result` is EITHER, exactly as `tool/result` is: the tool decides by supplying
        // cites and the ledger's evidence-requires-cites rule does the rest.
        let class = if cites.is_empty() {
            Class::Thought
        } else {
            Class::Evidence
        };
        append(cx, "program/result", class, to_body(&body)?, cites.clone()).await?;
        {
            let mut state = cx.state.lock();
            state.results.push(index);
            state.concludes_wake |= result.concludes_wake;
            for c in cites {
                if !state.cites.contains(&c) {
                    state.cites.push(c);
                }
            }
        }

        if result.ok {
            // A tool that produced a VALUE answers with it; one that produced text answers with
            // the text, so `await bash(...)` is a string.
            Ok(result
                .value
                .clone()
                .unwrap_or(serde_json::Value::String(result.content)))
        } else {
            let failure = result.failure.clone();
            Err(HostRefusal {
                kind: failure
                    .as_ref()
                    .map(|f| refusal_of(f.kind))
                    .unwrap_or(RefusalKind::Error),
                message: failure.map(|f| f.message).unwrap_or(result.content),
            })
        }
    }
}

/// A read or a write on the program's gate, held for one inner call. The guard is held for its
/// lifetime and never read — holding it IS the barrier.
#[allow(dead_code)]
enum Guard<'a> {
    Read(tokio::sync::RwLockReadGuard<'a, ()>),
    Write(tokio::sync::RwLockWriteGuard<'a, ()>),
}

fn to_body<T: serde::Serialize>(body: &T) -> Result<serde_json::Value, HostRefusal> {
    serde_json::to_value(body).map_err(|e| HostRefusal {
        kind: RefusalKind::Error,
        message: format!("a program step could not be serialised: {e}"),
    })
}

/// Append one of this crate's sub-steps. A ledger that refuses the step is a REFUSAL the program
/// sees: an inner call whose record did not commit must not look like it ran.
async fn append(
    cx: &ProgramCx,
    kind: &str,
    class: Class,
    body: serde_json::Value,
    cites: Vec<Cite>,
) -> Result<(), HostRefusal> {
    cx.ledger
        .0
        .append(Append {
            traj: cx.traj.clone(),
            wake: cx.wake.clone(),
            kind: StepType::new(kind),
            class,
            body,
            cites,
            at: chrono::Utc::now(),
            id: None,
        })
        .await
        .map(|_| ())
        .map_err(|e| HostRefusal {
            kind: RefusalKind::Error,
            message: format!("`{kind}` could not be appended: {e}"),
        })
}

/// What a `ToolResult` became, in the step vocabulary.
pub fn outcome_of(result: &bough_plugin_tools::ToolResult) -> ToolOutcomeKind {
    if result.ok {
        return ToolOutcomeKind::Ok;
    }
    match result.failure.as_ref().map(|f| f.kind) {
        Some(FailureClass::Denied) => ToolOutcomeKind::Denied,
        Some(FailureClass::Blocked) => ToolOutcomeKind::Blocked,
        Some(FailureClass::Unknown) => ToolOutcomeKind::Unknown,
        _ => ToolOutcomeKind::Error,
    }
}

/// The one mapping between the tools seam's failure taxonomy and the sandbox's.
pub fn refusal_of(class: FailureClass) -> RefusalKind {
    match class {
        FailureClass::NotFound => RefusalKind::NotFound,
        FailureClass::Denied => RefusalKind::Denied,
        FailureClass::Blocked => RefusalKind::Blocked,
        FailureClass::Timeout => RefusalKind::Timeout,
        FailureClass::Cancelled => RefusalKind::Cancelled,
        // Crash repair's synthesised outcome: a program never sees it live, and if it ever did
        // it is an error like any other rather than a seventh kind in the sandbox.
        FailureClass::Unknown | FailureClass::Error => RefusalKind::Error,
    }
}

/// The names the sandbox will inject, for a surface to render (WP-5 reads this).
pub fn injected_names(bindings: &[Binding]) -> Vec<ToolName> {
    bindings.iter().map(|b| ToolName::new(&b.tool)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_tools::{RenderIntent, Tool, ToolCx, ToolFailure, ToolOutcome, ToolScope};

    struct Noop;
    #[async_trait::async_trait]
    impl Tool for Noop {
        async fn call(
            &self,
            _call: Arc<ToolCall>,
            _cx: ToolCx,
        ) -> Result<ToolOutcome, ToolFailure> {
            Ok(ToolOutcome::default())
        }
    }

    fn spec(name: &str) -> ToolSpec {
        spec_with(name, serde_json::json!({"type": "object"}))
    }

    fn spec_with(name: &str, schema: serde_json::Value) -> ToolSpec {
        ToolSpec {
            name: ToolName::new(name),
            description: String::new(),
            input_schema: schemars::Schema::try_from(schema).unwrap(),
            render: RenderIntent::Generic,
            scope: ToolScope::Global,
            tool: Arc::new(Noop),
        }
    }

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn a_name_that_is_not_a_js_identifier_is_refused() {
        let err = bindings(&[spec("read-file")], &BTreeMap::new(), &BTreeMap::new())
            .expect_err("a hyphenated tool name cannot be a global");
        assert_eq!(err, BindError::NotAnIdentifier("read-file".into()));
        // A reserved word is refused for the same reason: binding it would shadow the language.
        assert!(matches!(
            bindings(&[spec("class")], &BTreeMap::new(), &BTreeMap::new()),
            Err(BindError::NotAnIdentifier(_))
        ));
    }

    #[test]
    fn an_alias_replaces_the_default_name_and_a_namespace_groups_a_prefix() {
        let specs = vec![
            spec("propose_claim"),
            spec("bash"),
            spec("mcp__linear__issues"),
        ];
        let out = bindings(
            &specs,
            &map(&[("claim", "propose_claim")]),
            &map(&[("mcp", "mcp__")]),
        )
        .unwrap();
        let js: Vec<&str> = out.iter().map(|b| b.js.as_str()).collect();
        assert_eq!(js, vec!["bash", "claim", "mcp.linear.issues"]);
        assert!(
            !out.iter().any(|b| b.js == "propose_claim"),
            "an aliased tool is injected under the alias ALONE"
        );
        assert_eq!(
            out.iter().find(|b| b.js == "claim").unwrap().tool,
            "propose_claim"
        );
    }

    #[test]
    fn an_empty_namespace_prefix_claims_nothing() {
        let out = bindings(&[spec("open_pr")], &BTreeMap::new(), &map(&[("act", "")])).unwrap();
        assert_eq!(out[0].js, "open_pr");
    }

    #[test]
    fn two_names_claiming_one_identifier_is_a_collision() {
        let err = bindings(
            &[spec("bash"), spec("shell")],
            &map(&[("bash", "shell")]),
            &BTreeMap::new(),
        )
        .expect_err("an alias colliding with a registered name must be reported");
        assert!(matches!(err, BindError::Collision { js, .. } if js == "bash"));
    }

    #[test]
    fn an_alias_for_an_invisible_tool_is_simply_absent() {
        let out = bindings(
            &[spec("bash")],
            &map(&[("claim", "propose_claim")]),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].js, "bash");
    }

    #[test]
    fn positional_arguments_zip_onto_required_then_the_rest_in_name_order() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"cmd": {"type": "string"}, "tags": {"type": "array"}, "cwd": {"type": "string"}},
            "required": ["cmd", "tags"]
        });
        assert_eq!(positional_order(&schema), vec!["cmd", "tags", "cwd"]);
        let argv = positional_args(
            &schema,
            vec![serde_json::json!("ls"), serde_json::json!(["a", "b", "c"])],
        );
        assert_eq!(
            argv,
            serde_json::json!({"cmd": "ls", "tags": ["a","b","c"]})
        );
    }

    #[test]
    fn one_object_argument_that_satisfies_the_schema_is_passed_through() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"kind": {"type": "string"}, "title": {"type": "string"}},
            "required": ["kind", "title"]
        });
        let obj = serde_json::json!({"kind": "lane", "title": "t"});
        assert_eq!(positional_args(&schema, vec![obj.clone()]), obj);
    }

    /// `docs/codemode-merge-notes.md` §9: the shell surface was unsatisfiable, because the only
    /// registered `bash` is `{command, cwd}` and the tag argument bound to `cwd`.
    #[test]
    fn a_tag_argument_is_taken_off_a_bash_that_does_not_declare_tags() {
        let baseline = spec_with(
            "bash",
            serde_json::json!({"type":"object",
                "properties":{"command":{"type":"string"},"cwd":{"type":"string"}},
                "required":["command"]}),
        );
        let mut args = vec![
            serde_json::json!("echo hi"),
            serde_json::json!("echo:probe:demo"),
        ];
        let tags = shell_tags(&baseline, &mut args);
        assert_eq!(tags, vec!["echo", "probe", "demo"]);
        assert_eq!(
            positional_args(baseline.input_schema.as_value(), args),
            serde_json::json!({"command": "echo hi"}),
            "the tags must not reach the tool as `cwd`"
        );
        assert_eq!(
            arity_of(&baseline),
            3,
            "two properties plus the tag argument"
        );
    }

    #[test]
    fn an_array_of_tags_is_taken_as_written_and_a_missing_one_yields_none() {
        let baseline = spec_with(
            "bash",
            serde_json::json!({"type":"object",
                "properties":{"command":{"type":"string"}},"required":["command"]}),
        );
        let mut arr = vec![
            serde_json::json!("ls"),
            serde_json::json!(["repo", "layout", "probe"]),
        ];
        assert_eq!(
            shell_tags(&baseline, &mut arr),
            vec!["repo", "layout", "probe"]
        );
        let mut bare = vec![serde_json::json!("ls")];
        assert!(
            shell_tags(&baseline, &mut bare).is_empty(),
            "an untagged call keeps its arguments and is refused by the tag rule"
        );
        assert_eq!(bare.len(), 1);
    }

    #[test]
    fn sh_carries_its_tags_per_leg() {
        let sh = spec_with(
            "sh",
            serde_json::json!({"type":"object","properties":{"legs":{"type":"array"}},
                "required":["legs"]}),
        );
        let mut args = vec![serde_json::json!([
            {"cmd": "cargo fmt --check", "tag": "cargo:fmt:check"},
            {"cmd": "git status", "tag": "git:status:worktree"},
        ])];
        assert_eq!(
            shell_tags(&sh, &mut args),
            vec!["cargo", "fmt", "check", "git", "status", "worktree"]
        );
        assert_eq!(args.len(), 1, "sh's legs are its one argument and stay put");
    }

    /// A shell Provider that DOES declare `tags` keeps binding it positionally: the code-mode
    /// parameter exists only to cover the tools that do not.
    #[test]
    fn a_bash_that_declares_tags_still_binds_them_positionally() {
        let owned = spec_with(
            "bash",
            serde_json::json!({"type":"object",
                "properties":{"cmd":{"type":"string"},"tags":{"type":"array"}},
                "required":["cmd","tags"]}),
        );
        let mut args = vec![serde_json::json!("ls"), serde_json::json!(["a", "b", "c"])];
        assert!(shell_tags(&owned, &mut args).is_empty());
        let argv = positional_args(owned.input_schema.as_value(), args);
        assert_eq!(tags_of(&argv), vec!["a", "b", "c"]);
        assert_eq!(arity_of(&owned), 2);
    }

    #[test]
    fn a_colon_separated_tag_string_on_a_declared_property_parses_too() {
        assert_eq!(
            tags_of(&serde_json::json!({"tags": "git:push:main"})),
            vec!["git", "push", "main"]
        );
    }

    #[test]
    fn arity_is_one_per_declared_property() {
        let s = spec_with(
            "bash",
            serde_json::json!({"type":"object","properties":{"cmd":{},"tags":{}},"required":["cmd","tags"]}),
        );
        assert_eq!(arity_of(&s), 2);
        assert_eq!(
            arity_of(&spec("inbox")),
            1,
            "a schema-less tool takes one bag"
        );
    }
}
