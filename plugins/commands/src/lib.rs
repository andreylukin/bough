//! Invariant: this crate is the commands SERVICE DEFINITION (§11, §0.2). It owns the `commands`
//! key, the registry, the pure `parse`, scoped resolution with most-specific-wins, `dispatch` and
//! the one observability event — and NOT ONE surface. It never renders, never touches a terminal
//! and never starts a wake: a dispatch appends no step (P3-D8) and reaches a model only by way of
//! `Agent::inject` / `Agent::steer`, which are durable already.
//!
//! It holds live state (the registry), so it IS a catalog row and provides its own key.

pub mod invariant;
pub mod parse;

use std::sync::Arc;

use std::sync::atomic::{AtomicU64, Ordering};

use bough_kernel::{
    ConfigError, Context, EffectHandle, EmitEvent, Inject, InvariantSpec, Plugin, PluginError,
    ServiceKey,
};
use bough_plugin_agents::Agent;
use bough_plugin_ledger::{AgentName, Cite};
use chrono::{DateTime, Utc};

pub use parse::parse;

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "commands";

/// The `commands` service key.
pub struct Commands;

impl ServiceKey for Commands {
    type Value = CommandsHandle;
    const NAME: &'static str = "commands";
}

bough_util::brand_id! {
    /// A command's name, without the prefix: `focus`, `help`, `oldfeed`.
    pub struct CommandName;
}

/// Where a command is visible. Most-specific-wins on a name clash.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CommandScope {
    /// Every agent.
    Global,
    /// One agent only.
    Agent(AgentName),
}

/// One registered command.
#[derive(Clone)]
pub struct CommandSpec {
    pub name: CommandName,
    /// One line, for `/help` and completion.
    pub summary: String,
    /// `"/focus <agent>"`.
    pub usage: String,
    /// Structured args, so Phase 6's `bough mcp call` can validate without a parser of its own.
    pub args: schemars::Schema,
    pub scope: CommandScope,
    pub run: Arc<dyn Command>,
}

/// The listing shape `list()` returns: a spec without its runnable half.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandInfo {
    pub name: CommandName,
    pub summary: String,
    pub usage: String,
    pub scope: CommandScope,
}

/// What a command does.
#[async_trait::async_trait]
pub trait Command: Send + Sync + 'static {
    async fn run(&self, inv: Invocation, cx: CommandCx) -> Result<CommandOutput, CommandError>;
}

/// One typed command line, as `parse` produced it.
#[derive(Clone, Debug, PartialEq)]
pub struct Invocation {
    pub name: CommandName,
    /// The whole line as typed, prefix included.
    pub raw: String,
    /// Shell-style split of the remainder; quoted runs stay whole.
    pub args: Vec<String>,
}

/// What a command produced. Rendered locally; never a step (P3-D8).
#[derive(Clone, Debug, PartialEq)]
pub struct CommandOutput {
    pub text: String,
    pub render: OutputRender,
    /// A command MAY cite; the pane renders cites under the output.
    pub cites: Vec<Cite>,
}

/// How a surface renders a command's output.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutputRender {
    Plain,
    KeyValue,
    Terminal,
}

/// Everything a dispatch can go wrong as.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum CommandError {
    #[error("unknown command `{name}`{}", suggestion_suffix(.did_you_mean))]
    Unknown {
        name: String,
        did_you_mean: Option<String>,
    },
    #[error("usage: {usage}")]
    BadArgs { usage: String, detail: String },
    #[error("{0}")]
    Failed(String),
}

/// `" — did you mean `focus`?"`, or nothing.
pub fn suggestion_suffix(did_you_mean: &Option<String>) -> String {
    match did_you_mean {
        Some(s) => format!(" — did you mean `{s}`?"),
        None => String::new(),
    }
}

/// What a command runs against.
pub struct CommandCx {
    pub ctx: Context,
    /// The focused agent, when there is one. A command may steer/inject through it; that is
    /// durable (`inbox/spliced`) and is the ONLY way a command reaches a model.
    pub agent: Option<Agent>,
    pub at: DateTime<Utc>,
}

/// The concrete handle the key's value is.
#[derive(Clone)]
pub struct CommandsHandle(pub Arc<CommandsInner>);

/// The registry's live state.
///
/// Registration order is kept, and each row carries the id its disposer removes: unloading the
/// registering row removes exactly its own commands and leaves every other row's alone (§0.2).
pub struct CommandsInner {
    ctx: Context,
    cfg: Arc<CommandsConfig>,
    registered: parking_lot::Mutex<Vec<(u64, CommandSpec)>>,
}

static NEXT_REGISTRATION: AtomicU64 = AtomicU64::new(0);

/// How a scope is spelled in the invariant's stream and in a message.
pub fn scope_str(scope: &CommandScope) -> String {
    match scope {
        CommandScope::Global => "global".to_string(),
        CommandScope::Agent(a) => format!("agent:{a}"),
    }
}

/// The schema of a command that takes no arguments.
///
/// ARGUMENT CONVENTION (WP-1). A command's `args` schema may be written EITHER way, and the same
/// typed line validates against both:
///
/// * an OBJECT of named string properties — the shape Phase 6's `bough mcp call` speaks, and the
///   one [`bind_positional`] maps a slash line's positional arguments onto: `required` names in
///   their declared order first, then the remaining properties by name;
/// * an ARRAY of strings — what [`positional`] builds, for a command whose arguments have no
///   useful names.
///
/// Neither side owns a parser: `parse` produces a list of strings and the schema decides what it
/// is a list OF.
pub fn no_args() -> schemars::Schema {
    schemars::Schema::try_from(serde_json::json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false,
    }))
    .expect("a no-args schema is an object")
}

/// A positional-argument schema whose LAST name absorbs everything after it: `/edit c1 tighten
/// the wording` binds `text` to three words rather than being refused for having too many.
///
/// A command that advertises `<text…>` or `<reason…>` in its usage line and then caps the list at
/// one word per name is not the command it documents — the cap is enforced by `jsonschema` before
/// `run` is ever reached, so a handler written to join the rest never sees them.
pub fn positional_rest(names: &[&str], required: usize) -> schemars::Schema {
    schema_for(names, required, None)
}

/// A positional-argument schema: `names` in order, the first `required` of them mandatory, and no
/// argument past the last name.
pub fn positional(names: &[&str], required: usize) -> schemars::Schema {
    schema_for(names, required, Some(names.len()))
}

fn schema_for(names: &[&str], required: usize, max: Option<usize>) -> schemars::Schema {
    let items: Vec<serde_json::Value> = names
        .iter()
        .map(|n| serde_json::json!({ "type": "string", "title": n }))
        .collect();
    let mut schema = serde_json::json!({
        "type": "array",
        "items": { "type": "string" },
        "minItems": required,
    });
    if let Some(max) = max {
        schema["maxItems"] = serde_json::json!(max);
    }
    // `prefixItems` must be non-empty to be a legal schema, so a no-argument command simply says
    // `maxItems: 0` and names nothing.
    if !items.is_empty() {
        schema["prefixItems"] = serde_json::Value::Array(items);
    }
    schemars::Schema::try_from(schema).expect("a positional schema is an object")
}

impl CommandsHandle {
    /// An empty registry.
    pub fn new(ctx: Context, cfg: Arc<CommandsConfig>) -> CommandsHandle {
        CommandsHandle(Arc::new(CommandsInner {
            ctx,
            cfg,
            registered: parking_lot::Mutex::new(Vec::new()),
        }))
    }

    /// An EFFECT (§0.2): unloading the registering row removes the command (V5).
    ///
    /// A duplicate name IN THE SAME SCOPE is refused here, loudly: two rows claiming one name in
    /// one scope is a composition mistake, and resolving it silently by order would make which
    /// command runs depend on load order (§0.2 forbids exactly that).
    pub async fn register(
        &self,
        ctx: &Context,
        spec: CommandSpec,
    ) -> Result<EffectHandle, PluginError> {
        let entry = ctx.entry_id().clone();
        let id = NEXT_REGISTRATION.fetch_add(1, Ordering::Relaxed);
        let name = spec.name.to_string();
        let scope = scope_str(&spec.scope);
        {
            // The check and the push are ONE critical section. Releasing the lock between them let
            // two concurrent registrations of one name in one scope both pass the check and both
            // land, which is the "which one runs depends on load order" this refusal exists to
            // prevent.
            let mut held = self.0.registered.lock();
            if held
                .iter()
                .any(|(_, s)| s.name == spec.name && s.scope == spec.scope)
            {
                return Err(PluginError::new(
                    entry,
                    anyhow::anyhow!(
                        "command `{}` is already registered in scope `{}`",
                        spec.name,
                        scope_str(&spec.scope)
                    ),
                ));
            }
            held.push((id, spec));
        }
        invariant::record(invariant::Obs::Registered {
            name: name.clone(),
            scope: scope.clone(),
        });
        let inner = self.0.clone();
        ctx.effect(move |e| async move {
            e.defer_sync(move || {
                inner.registered.lock().retain(|(i, _)| *i != id);
                invariant::record(invariant::Obs::Unregistered { name, scope });
            });
            Ok(())
        })
        .await
    }

    /// Global commands plus the named agent's scoped ones; most-specific-wins on a name clash.
    pub fn list(&self, scope: Option<&AgentName>) -> Vec<CommandInfo> {
        let mut out: Vec<CommandInfo> = Vec::new();
        for spec in self.visible(scope) {
            out.push(CommandInfo {
                name: spec.name.clone(),
                summary: spec.summary.clone(),
                usage: spec.usage.clone(),
                scope: spec.scope.clone(),
            });
        }
        out.sort_by(|a, b| a.name.as_str().cmp(b.name.as_str()));
        out
    }

    /// The winning spec for this name in this scope.
    pub fn resolve(&self, name: &CommandName, scope: Option<&AgentName>) -> Option<CommandSpec> {
        self.visible(scope).into_iter().find(|s| &s.name == name)
    }

    /// Every command visible in `scope`, the scoped twin shadowing its global one.
    fn visible(&self, scope: Option<&AgentName>) -> Vec<CommandSpec> {
        let held = self.0.registered.lock();
        let mine = |s: &CommandSpec| match (&s.scope, scope) {
            (CommandScope::Global, _) => true,
            (CommandScope::Agent(a), Some(here)) => a == here,
            (CommandScope::Agent(_), None) => false,
        };
        let mut out: Vec<CommandSpec> = Vec::new();
        for (_, spec) in held.iter().filter(|(_, s)| mine(s)) {
            match out.iter().position(|s| s.name == spec.name) {
                // MOST-SPECIFIC-WINS: a scoped command shadows its global twin, for that agent
                // only. Two scoped twins cannot happen — `register` refuses a duplicate per scope.
                Some(i) if matches!(spec.scope, CommandScope::Agent(_)) => out[i] = spec.clone(),
                Some(_) => {}
                None => out.push(spec.clone()),
            }
        }
        out
    }

    /// Resolve, validate args against the schema, run. Appends NO step, starts NO wake, and
    /// emits `commands/dispatched` when it returns.
    pub async fn dispatch(
        &self,
        inv: Invocation,
        cx: CommandCx,
    ) -> Result<CommandOutput, CommandError> {
        let scope: Option<AgentName> = cx.agent.as_ref().map(|a| a.name().clone());
        let at = cx.at;
        let Some(spec) = self.resolve(&inv.name, scope.as_ref()) else {
            invariant::record(invariant::Obs::Dispatched {
                name: inv.name.to_string(),
                scope: scope
                    .as_ref()
                    .map(|a| format!("agent:{a}"))
                    .unwrap_or_else(|| "global".into()),
                resolved: false,
            });
            let known: Vec<CommandName> = self
                .visible(scope.as_ref())
                .iter()
                .map(|s| s.name.clone())
                .collect();
            return Err(CommandError::Unknown {
                name: inv.name.to_string(),
                did_you_mean: if self.0.cfg.suggestions {
                    parse::did_you_mean(inv.name.as_str(), &known)
                } else {
                    None
                },
            });
        };
        invariant::record(invariant::Obs::Dispatched {
            name: spec.name.to_string(),
            scope: scope_str(&spec.scope),
            resolved: true,
        });
        let outcome = match validate_args(&spec, &inv.args) {
            Err(detail) => Err(CommandError::BadArgs {
                usage: spec.usage.clone(),
                detail,
            }),
            Ok(()) => spec.run.clone().run(inv, cx).await,
        };
        self.0.ctx.emit::<CommandDispatched>(DispatchRecord {
            name: spec.name.clone(),
            ok: outcome.is_ok(),
            scope: spec.scope.clone(),
            at,
        });
        outcome
    }

    /// The configured command prefix. One character.
    pub fn prefix(&self) -> char {
        self.0.cfg.prefix
    }
}

/// Map a slash line's positional arguments onto an OBJECT schema's properties.
///
/// The order is the one thing a reader has to be able to predict, so it is stated rather than
/// inherited from a map's iteration: every `required` name in its declared order, then the
/// remaining property names in name order. One property is the overwhelmingly common case and
/// every ordering agrees there.
pub fn bind_positional(
    schema: &serde_json::Value,
    args: &[String],
) -> Result<serde_json::Value, String> {
    let props = schema
        .get("properties")
        .and_then(|p| p.as_object())
        .cloned()
        .unwrap_or_default();
    let mut names: Vec<String> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|r| {
            r.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let mut rest: Vec<String> = props
        .keys()
        .filter(|k| !names.contains(k))
        .cloned()
        .collect();
    rest.sort();
    names.append(&mut rest);
    if args.len() > names.len() {
        return Err(format!(
            "{} argument(s) given, at most {} expected",
            args.len(),
            names.len()
        ));
    }
    let mut object = serde_json::Map::new();
    for (name, value) in names.iter().zip(args.iter()) {
        object.insert(name.clone(), serde_json::Value::String(value.clone()));
    }
    Ok(serde_json::Value::Object(object))
}

/// The positional argument list as the schema sees it, validated with `jsonschema`.
///
/// An uncompilable schema is the REGISTERING row's bug, not the typist's, so it is reported as
/// `BadArgs`' detail rather than swallowed: a command whose schema cannot compile must not run
/// unvalidated.
fn validate_args(spec: &CommandSpec, args: &[String]) -> Result<(), String> {
    let schema = spec.args.as_value();
    let instance = match schema.get("type").and_then(|t| t.as_str()) {
        Some("object") => bind_positional(schema, args)?,
        // An array schema (or one that names no type) validates the argument LIST itself.
        _ => serde_json::Value::Array(
            args.iter()
                .map(|a| serde_json::Value::String(a.clone()))
                .collect(),
        ),
    };
    let validator = jsonschema::validator_for(schema)
        .map_err(|e| format!("the command's own args schema does not compile: {e}"))?;
    let first = validator.iter_errors(&instance).next().map(|e| {
        let at = e.instance_path.to_string();
        let at = if at.is_empty() {
            "the argument list".to_string()
        } else {
            format!("argument `{at}`")
        };
        format!("{at}: {e}")
    });
    match first {
        None => Ok(()),
        Some(detail) => Err(detail),
    }
}

/// What `commands/dispatched` carries.
#[derive(Clone, Debug, PartialEq)]
pub struct DispatchRecord {
    pub name: CommandName,
    pub ok: bool,
    pub scope: CommandScope,
    pub at: DateTime<Utc>,
}

/// `commands/dispatched` — EMIT, observability only.
pub struct CommandDispatched;

impl EmitEvent for CommandDispatched {
    const NAME: &'static str = "commands/dispatched";
    type Payload = DispatchRecord;
}

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommandsConfig {
    /// The command prefix. One character.
    pub prefix: char,
    /// Levenshtein suggestions on an unknown name.
    pub suggestions: bool,
}

/// The row.
pub struct CommandsPlugin;

#[async_trait::async_trait]
impl Plugin for CommandsPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = CommandsConfig;

    fn inject() -> Inject {
        Inject::none()
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        // A whitespace prefix would make every typed line that starts with a space a command, and
        // an alphanumeric one would swallow ordinary messages.
        if cfg.prefix.is_whitespace() || cfg.prefix.is_alphanumeric() {
            return Err(ConfigError::Rejected {
                detail: format!(
                    "prefix `{}` must be a non-alphanumeric, non-whitespace character",
                    cfg.prefix
                ),
            });
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let handle = CommandsHandle::new(ctx.clone(), cfg);
        // The recorded stream is per-process and this row owns it: unloading forgets what it saw,
        // so a reload is never read as a violation of its predecessor.
        ctx.effect(|e| async move {
            e.defer_sync(invariant::clear);
            Ok(())
        })
        .await?;
        ctx.provide::<Commands>(handle)
            .await
            .map_err(|e| PluginError::new(entry, e))?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(CommandsPlugin);

#[cfg(test)]
mod schema_tests {
    use super::*;

    fn spec(args: schemars::Schema) -> CommandSpec {
        CommandSpec {
            name: CommandName::new("edit"),
            usage: "/edit <claim> <text…>".to_string(),
            summary: "edit a claim".to_string(),
            scope: CommandScope::Global,
            args,
            run: Arc::new(NoopCommand) as Arc<dyn Command>,
        }
    }

    struct NoopCommand;

    #[async_trait::async_trait]
    impl Command for NoopCommand {
        async fn run(&self, _i: Invocation, _c: CommandCx) -> Result<CommandOutput, CommandError> {
            Ok(CommandOutput {
                text: String::new(),
                render: OutputRender::Plain,
                cites: Vec::new(),
            })
        }
    }

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    /// `/edit c1 tighten the wording` is the command as its own usage line advertises it. Under
    /// `positional` the schema's `maxItems` refused it before `run` was ever reached, so the
    /// accept/edit/reject gate could only edit a claim to ONE WORD.
    #[test]
    fn a_rest_argument_accepts_many_words() {
        let s = spec(positional_rest(&["claim", "text"], 2));
        assert_eq!(
            validate_args(&s, &args(&["c1", "tighten", "the", "wording"])),
            Ok(())
        );
        // The required minimum still bites.
        assert!(validate_args(&s, &args(&["c1"])).is_err());
    }

    /// And the capped form still caps: `positional` is unchanged for the commands that want it.
    #[test]
    fn a_capped_positional_still_refuses_an_extra_word() {
        let s = spec(positional(&["agent"], 1));
        assert_eq!(validate_args(&s, &args(&["sol"])), Ok(()));
        assert!(validate_args(&s, &args(&["sol", "terra"])).is_err());
    }
}
