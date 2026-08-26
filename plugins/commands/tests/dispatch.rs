//! §11 / §2.2: a human command parses, resolves most-specific-first, validates its arguments and
//! runs — WITHOUT a model turn. The last test is the one that matters most: a dispatch appends no
//! step and starts no wake (P3-D8), asserted against a real ledger handle and a real agent.

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_agents::{
    Agent, AgentCell, AgentDriver, AgentError, AgentFactory, AgentsHandle, Attach, CancelCause,
    CreateAgent, InboxReceipt, Message, WakeCause, WakeKind, WakeRequest,
};
use bough_plugin_commands::{
    no_args, parse, positional, Command, CommandCx, CommandError, CommandName, CommandOutput,
    CommandScope, CommandSpec, CommandsConfig, CommandsHandle, Invocation, OutputRender,
};
use bough_plugin_ledger::{AgentName, LedgerHandle, StepQuery, TrajId};
use bough_plugin_ledger_memory::store::MemoryStore;

/// A command that answers with what it was given, so a test can see the invocation arrived whole.
struct Echo(&'static str);

#[async_trait::async_trait]
impl Command for Echo {
    async fn run(&self, inv: Invocation, _cx: CommandCx) -> Result<CommandOutput, CommandError> {
        Ok(CommandOutput {
            text: format!("{}:{}", self.0, inv.args.join(",")),
            render: OutputRender::Plain,
            cites: vec![],
        })
    }
}

fn spec(name: &str, scope: CommandScope, args: schemars::Schema, tag: &'static str) -> CommandSpec {
    CommandSpec {
        name: CommandName::new(name),
        summary: "a test command".into(),
        usage: format!("/{name} <agent>"),
        args,
        scope,
        run: Arc::new(Echo(tag)),
    }
}

fn handle(ctx: &Context) -> CommandsHandle {
    CommandsHandle::new(
        ctx.clone(),
        Arc::new(CommandsConfig {
            prefix: '/',
            suggestions: true,
        }),
    )
}

fn cx(ctx: &Context, agent: Option<Agent>) -> CommandCx {
    CommandCx {
        ctx: ctx.clone(),
        agent,
        at: chrono::Utc::now(),
    }
}

#[tokio::test]
async fn a_slash_line_parses_into_a_name_and_args() {
    let inv = parse("/focus sol \"two words\"", '/').expect("a command line");
    assert_eq!(inv.name, CommandName::new("focus"));
    assert_eq!(inv.args, vec!["sol", "two words"]);
    assert_eq!(inv.raw, "/focus sol \"two words\"");
}

#[tokio::test]
async fn a_doubled_prefix_is_literal_text_and_does_not_parse() {
    assert_eq!(parse("//focus sol", '/'), None);
    assert_eq!(parse("plain text", '/'), None);
    // The prefix is configurable, and the doubling rule follows it.
    assert!(parse(":focus", ':').is_some());
    assert_eq!(parse("::focus", ':'), None);
}

#[tokio::test]
async fn an_unknown_name_reports_unknown_with_a_suggestion() {
    let ctx = Context::root(KernelCore::new());
    let h = handle(&ctx);
    let _e = h
        .register(&ctx, spec("focus", CommandScope::Global, no_args(), "g"))
        .await
        .expect("registers");
    let err = h
        .dispatch(parse("/fcus", '/').expect("a command line"), cx(&ctx, None))
        .await
        .expect_err("no such command");
    match &err {
        CommandError::Unknown { name, did_you_mean } => {
            assert_eq!(name, "fcus");
            assert_eq!(did_you_mean.as_deref(), Some("focus"));
        }
        other => panic!("wrong refusal: {other}"),
    }
    assert!(err.to_string().contains("did you mean `focus`"), "{err}");
}

#[tokio::test]
async fn bad_args_report_the_usage_string() {
    let ctx = Context::root(KernelCore::new());
    let h = handle(&ctx);
    let _e = h
        .register(
            &ctx,
            spec(
                "focus",
                CommandScope::Global,
                positional(&["agent"], 1),
                "g",
            ),
        )
        .await
        .expect("registers");
    let err = h
        .dispatch(
            parse("/focus", '/').expect("a command line"),
            cx(&ctx, None),
        )
        .await
        .expect_err("the agent name is required");
    match &err {
        CommandError::BadArgs { usage, .. } => assert_eq!(usage, "/focus <agent>"),
        other => panic!("wrong refusal: {other}"),
    }
    assert!(
        err.to_string().starts_with("usage: /focus <agent>"),
        "{err}"
    );
    // With the argument it runs.
    let out = h
        .dispatch(
            parse("/focus sol", '/').expect("a command line"),
            cx(&ctx, None),
        )
        .await
        .expect("runs");
    assert_eq!(out.text, "g:sol");
}

#[tokio::test]
async fn a_scoped_command_shadows_its_global_twin_for_that_agent_only() {
    let f = Fixture::mounted().await;
    let h = handle(&f.ctx);
    let _g = h
        .register(
            &f.ctx,
            spec("goal", CommandScope::Global, no_args(), "global"),
        )
        .await
        .expect("registers");
    let _s = h
        .register(
            &f.ctx,
            spec(
                "goal",
                CommandScope::Agent(AgentName::new("sol")),
                no_args(),
                "scoped",
            ),
        )
        .await
        .expect("registers");

    let sol = f.agent("sol").await;
    let terra = f.agent("terra").await;
    let line = || parse("/goal", '/').expect("a command line");

    assert_eq!(
        h.dispatch(line(), cx(&f.ctx, Some(sol.clone())))
            .await
            .expect("runs")
            .text,
        "scoped:",
        "sol sees its own"
    );
    assert_eq!(
        h.dispatch(line(), cx(&f.ctx, Some(terra.clone())))
            .await
            .expect("runs")
            .text,
        "global:",
        "another agent sees the global twin"
    );
    assert_eq!(
        h.dispatch(line(), cx(&f.ctx, None))
            .await
            .expect("runs")
            .text,
        "global:",
        "with no agent focused there is no scope to shadow from"
    );
    // Listing follows the same rule, so `/help` cannot show two `goal`s.
    let listed = h.list(Some(&AgentName::new("sol")));
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].scope, CommandScope::Agent(AgentName::new("sol")));
}

#[tokio::test]
async fn unloading_the_registering_row_removes_the_command() {
    let ctx = Context::root(KernelCore::new());
    let h = handle(&ctx);
    let effect = h
        .register(&ctx, spec("quit", CommandScope::Global, no_args(), "g"))
        .await
        .expect("registers");
    assert!(h.resolve(&CommandName::new("quit"), None).is_some());
    effect.dispose().await;
    assert!(
        h.resolve(&CommandName::new("quit"), None).is_none(),
        "unloading the registering row leaves no trace (§0.2)"
    );
    assert!(h.list(None).is_empty());
    // And the name is free again.
    let _again = h
        .register(&ctx, spec("quit", CommandScope::Global, no_args(), "g"))
        .await
        .expect("the name is free after unload");
}

#[tokio::test]
async fn a_dispatch_appends_no_step_and_starts_no_wake() {
    let f = Fixture::mounted().await;
    let h = handle(&f.ctx);
    let _e = h
        .register(&f.ctx, spec("agents", CommandScope::Global, no_args(), "g"))
        .await
        .expect("registers");
    let sol = f.agent("sol").await;
    let before = f.steps().await;

    h.dispatch(
        parse("/agents", '/').expect("a command line"),
        cx(&f.ctx, Some(sol.clone())),
    )
    .await
    .expect("runs");

    assert_eq!(
        f.steps().await,
        before,
        "a slash command is rendered locally and is not model-visible (P3-D8)"
    );
    assert_eq!(sol.status(), bough_plugin_agents::Status::Idle);
    assert!(!sol.has_pending_wake(), "no wake was armed");
    assert!(sol.inbox().is_empty(), "nothing was spliced into the inbox");
}

// ---------------------------------------------------------------------------------------------
// A real agent over a real (in-memory) ledger, with a driver that does nothing but record.

struct Fixture {
    ctx: Context,
    ledger: LedgerHandle,
    agents: AgentsHandle,
    _disposers: parking_lot::Mutex<Vec<bough_plugin_agents::AgentDisposer>>,
}

impl Fixture {
    async fn mounted() -> Fixture {
        let ctx = Context::root(KernelCore::new());
        let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
        let agents = AgentsHandle::new(ctx.clone(), ledger.clone());
        agents
            .set_factory(&ctx, Arc::new(InertFactory) as Arc<dyn AgentFactory>)
            .await
            .expect("the slot is free");
        Fixture {
            ctx,
            ledger,
            agents,
            _disposers: parking_lot::Mutex::new(Vec::new()),
        }
    }

    async fn agent(&self, name: &str) -> Agent {
        let (agent, disposer) = self
            .agents
            .create(CreateAgent::resident(
                AgentName::new(name),
                TrajId::new(format!("lane/{name}")),
                chrono::Utc::now(),
            ))
            .await
            .expect("the transaction commits");
        self._disposers.lock().push(disposer);
        agent
    }

    async fn steps(&self) -> usize {
        self.ledger
            .0
            .steps(&StepQuery::default())
            .await
            .expect("a read")
            .len()
    }
}

struct InertFactory;

#[async_trait::async_trait]
impl AgentFactory for InertFactory {
    fn driver(&self) -> &'static str {
        "inert"
    }
    async fn attach(
        &self,
        cell: AgentCell,
        _mode: Attach,
    ) -> Result<Arc<dyn AgentDriver>, AgentError> {
        Ok(Arc::new(InertDriver { _cell: cell }))
    }
}

struct InertDriver {
    _cell: AgentCell,
}

#[async_trait::async_trait]
impl AgentDriver for InertDriver {
    fn driver(&self) -> &'static str {
        "inert"
    }
    async fn wake_now(&self, _kind: WakeKind, _cause: WakeCause) -> WakeRequest {
        panic!("a dispatch must never ask a driver to wake");
    }
    async fn notify(&self, _receipt: &InboxReceipt, _msg: &Message) {
        panic!("a dispatch must never splice mail");
    }
    async fn cancel(&self, _cause: CancelCause, _keep_inbox: bool) {}
    async fn stop(&self) {}
}

/// The OBJECT half of the argument convention: an `{agent: string}` schema with `required`, the
/// shape `tui-shell`'s built-ins and Phase 6's `bough mcp call` write, validates the same typed
/// line — a missing required argument is `BadArgs` with the usage string, and a spare one is too.
#[tokio::test]
async fn an_object_args_schema_validates_the_same_typed_line() {
    let ctx = Context::root(KernelCore::new());
    let h = handle(&ctx);
    let object = schemars::Schema::try_from(serde_json::json!({
        "type": "object",
        "properties": { "agent": { "type": "string" } },
        "required": ["agent"],
        "additionalProperties": false,
    }))
    .expect("a schema");
    let _e = h
        .register(&ctx, spec("focus", CommandScope::Global, object, "g"))
        .await
        .expect("registers");

    assert!(matches!(
        h.dispatch(parse("/focus", '/').unwrap(), cx(&ctx, None))
            .await,
        Err(CommandError::BadArgs { .. })
    ));
    assert!(matches!(
        h.dispatch(parse("/focus sol extra", '/').unwrap(), cx(&ctx, None))
            .await,
        Err(CommandError::BadArgs { .. })
    ));
    assert_eq!(
        h.dispatch(parse("/focus sol", '/').unwrap(), cx(&ctx, None))
            .await
            .expect("runs")
            .text,
        "g:sol"
    );
    // And a no-args command refuses an argument it cannot bind.
    let _q = h
        .register(&ctx, spec("quit", CommandScope::Global, no_args(), "g"))
        .await
        .expect("registers");
    assert!(matches!(
        h.dispatch(parse("/quit now", '/').unwrap(), cx(&ctx, None))
            .await,
        Err(CommandError::BadArgs { .. })
    ));
}
