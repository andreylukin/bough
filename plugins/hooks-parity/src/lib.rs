//! Invariant: this row makes the user's Claude Code + Codex hook settings REAL on the tools seam
//! (drivability §5). `PreToolUse` runs inside the `tools/pre-execute` waterfall — a hook's deny
//! or ask lands BEFORE the call executes — and `PostToolUse` inside `tools/post-execute`, where a
//! hook can block a result or attach context. Discovery is per CALL, from the call's own working
//! directory: a command run in a repo picks up that repo's hooks even when the resident was
//! started somewhere else entirely. The guard stays monotone: nothing here can widen a decision.
//!
//! What is NOT here yet: the post-hoc parity events (`Stop`, `SessionStart`,
//! `UserPromptSubmit`, …) — they have no per-call cwd and belong with the ledger-step machinery
//! (`hooks-exec`), not the tools seam.

pub mod invariant;
pub mod outcome;
pub mod run;
pub mod settings;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_tools::{
    AttachedContext, Decision, PostExecute, PreExecute, ToolCall, ToolResult, Tools,
    ToolsPostExecute, ToolsPreExecute, Workspace,
};

use outcome::PreVerdict;

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "hooks-parity";

/// The row's config. Every deployment-varying value is here (§0.2).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HooksParityConfig {
    /// Read Claude Code settings: `~/.claude/settings.json` and each ancestor's
    /// `.claude/settings.json` + `.claude/settings.local.json`.
    #[serde(default = "yes")]
    pub claude: bool,
    /// Read Codex settings: `~/.codex/{hooks.json,config.toml}` and each ancestor's `.codex/`.
    #[serde(default = "yes")]
    pub codex: bool,
    /// Include the user layer (`~/.claude`, `~/.codex`), not just the walked ancestors.
    #[serde(default = "yes")]
    pub user_layer: bool,
    /// Events to run; empty = every supported event.
    #[serde(default)]
    pub events: Vec<String>,
    /// Substring allowlist over hook COMMANDS; empty = all. The patch-level way to turn ON just
    /// the hooks you want.
    #[serde(default)]
    pub only: Vec<String>,
    /// Substring denylist over hook commands. Applied after `only`.
    #[serde(default)]
    pub except: Vec<String>,
    /// Bough tool name → the parity name matchers expect (`bash` → `Bash`), tried alongside the
    /// raw name.
    #[serde(default = "default_aliases")]
    pub tool_aliases: BTreeMap<String, String>,
    /// Default per-hook deadline when the settings file names none.
    pub timeout_ms: u64,
    /// Cap on a hook's captured stdout/stderr, and on the `tool_response` content it is fed.
    pub max_output_bytes: usize,
}

fn yes() -> bool {
    true
}

/// The spellings both CLIs' matchers actually use for the baseline tools.
pub fn default_aliases() -> BTreeMap<String, String> {
    [
        ("bash", "Bash"),
        ("read_file", "Read"),
        ("write_file", "Write"),
        ("edit_file", "Edit"),
        ("glob", "Glob"),
        ("grep", "Grep"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// What the listeners carry. `ctx` is optional so the pure tests run without a kernel; the
/// workspace is resolved LAZILY per call, because its Provider may activate after this row.
pub struct HookState {
    pub cfg: Arc<HooksParityConfig>,
    pub ctx: Option<Context>,
    pub home: Option<PathBuf>,
}

impl HookState {
    fn workspace(&self) -> Option<PathBuf> {
        let ctx = self.ctx.as_ref()?;
        ctx.get::<Workspace>().ok().map(|w| w.path().to_path_buf())
    }
    fn names<'a>(&'a self, raw: &'a str) -> Vec<&'a str> {
        let mut names = vec![raw];
        if let Some(alias) = self.cfg.tool_aliases.get(raw) {
            names.push(alias.as_str());
        }
        names
    }
}

/// The stdin payload, the shape both CLIs feed their hooks. `tool_name` is the PARITY spelling
/// when an alias exists, so existing hook scripts match unchanged; the raw name rides alongside.
pub fn payload(
    event: &str,
    names: &[&str],
    call: &ToolCall,
    cwd: &std::path::Path,
    response: Option<(&ToolResult, usize)>,
) -> serde_json::Value {
    let mut v = serde_json::json!({
        "hook_event_name": event,
        "session_id": call.agent.as_str(),
        "wake_id": call.wake.as_str(),
        "cwd": cwd.display().to_string(),
        "tool_name": names.last().copied().unwrap_or_default(),
        "bough_tool_name": names.first().copied().unwrap_or_default(),
        "tool_use_id": call.id.as_str(),
        "tool_input": call.args,
    });
    if let Some((r, cap)) = response {
        let mut content = r.content.clone();
        if content.len() > cap {
            let mut cut = cap;
            while cut > 0 && !content.is_char_boundary(cut) {
                cut -= 1;
            }
            content.truncate(cut);
        }
        v["tool_response"] = serde_json::json!({ "ok": r.ok, "content": content });
    }
    v
}

/// The `PreToolUse` pass over one call: discover from the call's cwd, run each matching hook,
/// tighten the decision. The FIRST deny ends the pass — the guard keeps its reason anyway.
pub async fn run_pre(st: &HookState, pre: &mut PreExecute) {
    if matches!(pre.decision(), Decision::Deny { .. }) {
        return;
    }
    let cfg = &st.cfg;
    let ws = st.workspace();
    let cwd = settings::call_cwd(&pre.call.args, ws.as_deref());
    let defs = settings::discover(
        &cwd,
        st.home.as_deref(),
        cfg.claude,
        cfg.codex,
        cfg.user_layer,
    );
    let raw = pre.call.name.as_str().to_string();
    let names = st.names(&raw);
    for def in settings::filtered(
        &defs,
        "PreToolUse",
        &names,
        &cfg.events,
        &cfg.only,
        &cfg.except,
    ) {
        let body = payload("PreToolUse", &names, &pre.call, &cwd, None);
        let run = run::run_hook(
            &def.command,
            &cwd,
            &body,
            def.timeout_ms.unwrap_or(cfg.timeout_ms),
            cfg.max_output_bytes,
        )
        .await;
        warn_failure(def, &run);
        match outcome::pre_verdict(&run) {
            PreVerdict::Deny(r) => {
                pre.deny(format!("{r} (hook: {})", def.command));
                return;
            }
            PreVerdict::Ask(r) => pre.ask(format!("{r} (hook: {})", def.command)),
            PreVerdict::Nothing => {}
        }
    }
}

/// The `PostToolUse` pass: every matching hook runs; a block turns the result into a `Blocked`
/// failure, and `additionalContext` attaches without touching content or value.
pub async fn run_post(st: &HookState, post: &mut PostExecute) {
    let cfg = &st.cfg;
    let ws = st.workspace();
    let cwd = settings::call_cwd(&post.call.args, ws.as_deref());
    let defs = settings::discover(
        &cwd,
        st.home.as_deref(),
        cfg.claude,
        cfg.codex,
        cfg.user_layer,
    );
    let raw = post.call.name.as_str().to_string();
    let names = st.names(&raw);
    for def in settings::filtered(
        &defs,
        "PostToolUse",
        &names,
        &cfg.events,
        &cfg.only,
        &cfg.except,
    ) {
        let body = payload(
            "PostToolUse",
            &names,
            &post.call,
            &cwd,
            Some((post.result(), cfg.max_output_bytes)),
        );
        let run = run::run_hook(
            &def.command,
            &cwd,
            &body,
            def.timeout_ms.unwrap_or(cfg.timeout_ms),
            cfg.max_output_bytes,
        )
        .await;
        warn_failure(def, &run);
        let out = outcome::post_out(&run);
        if let Some(text) = out.context {
            post.attach(AttachedContext {
                id: PLUGIN_NAME.to_string(),
                text,
            });
        }
        if let Some(reason) = out.block {
            post.block(format!("{reason} (hook: {})", def.command));
        }
    }
}

fn warn_failure(def: &settings::HookDef, run: &run::HookRun) {
    if run.timed_out {
        tracing::warn!(command = %def.command, source = %def.source.display(), "hook timed out");
    } else if !matches!(run.status, Some(0) | Some(2)) {
        tracing::warn!(
            command = %def.command,
            source = %def.source.display(),
            status = ?run.status,
            stderr = %run.stderr,
            "hook failed; it decides nothing"
        );
    }
}

/// The row.
pub struct HooksParityPlugin;

#[async_trait::async_trait]
impl Plugin for HooksParityPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = HooksParityConfig;

    fn inject() -> Inject {
        // `tools` for ordering against the seam whose waterfalls this row rides; the workspace is
        // resolved lazily because its Provider (the tools row) pins it at its own activation.
        Inject::required(["tools"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let reject = |detail: String| Err(ConfigError::Rejected { detail });
        if cfg.timeout_ms == 0 {
            return reject("timeout_ms must be > 0".to_string());
        }
        if cfg.max_output_bytes == 0 {
            return reject("max_output_bytes must be > 0".to_string());
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        ctx.get::<Tools>()
            .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;
        let st = Arc::new(HookState {
            cfg,
            ctx: Some(ctx.clone()),
            home: std::env::var_os("HOME").map(PathBuf::from),
        });
        let s1 = Arc::clone(&st);
        ctx.on_waterfall::<ToolsPreExecute, _, _>(move |mut pre: PreExecute, next| {
            let st = Arc::clone(&s1);
            async move {
                run_pre(&st, &mut pre).await;
                next.run(pre).await
            }
        })
        .await?;
        let s2 = Arc::clone(&st);
        ctx.on_waterfall::<ToolsPostExecute, _, _>(move |mut post: PostExecute, next| {
            let st = Arc::clone(&s2);
            async move {
                run_post(&st, &mut post).await;
                next.run(post).await
            }
        })
        .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(HooksParityPlugin);
