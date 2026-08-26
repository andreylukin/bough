//! Invariant: `bough exec` runs ONE task through the ordinary loop and then asks the process to
//! exit. It is composition, not behaviour: it resumes-or-creates an agent, sends the task as an
//! ANDREY message (so §5's answer-wake rule applies unchanged), awaits `when_idle()`, prints, and
//! calls `Kernel::request_exit` — the launcher still owns the exit path and tears down first.

pub mod invariant;
pub mod render;

use std::sync::Arc;

use bough_kernel::{Context, Plugin, PluginError};
use bough_plugin_agents::{
    Agent, AgentKind, Agents, AgentsHandle, CreateAgent, MailClass, Message, ResumeAgent, Sender,
};
use bough_plugin_ledger::query::{Order, StepQuery};
use bough_plugin_ledger::{AgentName, Ledger, LedgerHandle, TrajId};

/// The catalog name of this row. `exec`, not `exec-headless`: the row is what the profile names.
pub const PLUGIN_NAME: &str = "exec";

/// How the result is printed.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Print {
    /// The last assistant text.
    Text,
    /// The whole wake, as JSON.
    Json,
}

/// The row's config. `bough exec "<task>"` sets `task` through one synthetic patch layer.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecConfig {
    /// Empty ⇒ the row mounts and does nothing, which is what makes the headless profile usable
    /// without a task.
    #[serde(default)]
    pub task: String,
    pub agent: String,
    pub traj: String,
    pub print: Print,
    /// `false` leaves the process running after the task, for a test that wants to inspect it.
    pub exit_when_idle: bool,
}

/// The surface row.
pub struct ExecPlugin;

#[async_trait::async_trait]
impl Plugin for ExecPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = ExecConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["agents", "ledger"])
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        // Injection is the row's contract; a missing binding is a boot failure, not a silent skip.
        let agents = ctx
            .get::<Agents>()
            .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;
        let ledger = ctx
            .get::<Ledger>()
            .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;

        // An empty task is the headless profile with no work: the row mounts, activates and does
        // nothing. That is what makes `--profile headless` usable without `bough exec`.
        if cfg.task.trim().is_empty() {
            return Ok(());
        }

        // The task runs AFTER boot quiesces, so `apply` returns immediately and the work is an
        // effect: disposing the row halts it and disposes the agent it created.
        ctx.effect_spawn(move |ectx| async move {
            let entry = ectx.ctx().entry_id().clone();
            if let Err(e) = run(ectx.ctx().clone(), agents, ledger, cfg).await {
                eprintln!("bough exec: {e}");
                if let Some(k) = ectx.ctx().kernel() {
                    k.request_exit(1);
                }
                return Err(PluginError::new(entry, e));
            }
            Ok(())
        });
        Ok(())
    }
}

bough_kernel::register_plugin!(ExecPlugin);

/// One task, end to end: resume-or-create, send it as ANDREY, wait for idle, print, ask to exit.
///
/// It is deliberately linear and has no policy of its own: every decision that matters — which
/// model answers, what the wake claims, when the wake ends — belongs to `agent-loop` and to the
/// rows on its waterfalls. This row only supplies the message and reads the ledger back.
async fn run(
    ctx: Context,
    agents: Arc<AgentsHandle>,
    ledger: Arc<LedgerHandle>,
    cfg: Arc<ExecConfig>,
) -> Result<(), anyhow::Error> {
    // A row's `apply` runs while the tree is still converging, and the loop Provider may not have
    // taken the factory slot yet. Row order carries no load semantics (§0.2), so waiting for the
    // seam to be ready is the row's job — and waiting FOREVER would turn a missing loop row into
    // a hang instead of the boot failure it is.
    wait_for_factory(&agents).await?;

    let name = AgentName::new(&cfg.agent);
    let traj = TrajId::new(&cfg.traj);
    let now = chrono::Utc::now();

    // Resume-or-create: the ledger's `agents` row is the record of "this agent has a chain".
    let existing = ledger.0.agent(&name).await?;
    let (agent, disposer) = if existing.is_some() {
        agents
            .resume(ResumeAgent {
                name: name.clone(),
                at: now,
                setup: None,
            })
            .await?
    } else {
        agents
            .create(CreateAgent {
                name: name.clone(),
                traj: traj.clone(),
                kind: AgentKind::Resident,
                scope: None,
                setup: None,
                seed: Vec::new(),
                at: now,
            })
            .await?
    };

    let receipt = agent.followup(andrey_message(&cfg.task, now)).await?;
    agent.when_idle().await;

    let out = read_back(&ledger, &agent, receipt.seq, cfg.print).await?;
    println!("{out}");

    // Teardown before exit (§0.1): the agent goes down here, and the launcher then unloads the
    // tree — the exit request only asks, it never leaves.
    disposer.dispose().await;
    if cfg.exit_when_idle {
        if let Some(k) = ctx.kernel() {
            k.request_exit(0);
        }
    }
    Ok(())
}

/// How long the exec row waits for a loop Provider to take the factory slot.
///
/// A protocol bound on a startup race, not a deployment value (§0.2): every tree that has an
/// `agent-loop` row at all fills the slot in its own `apply`, so this only ever expires when the
/// tree genuinely has no loop.
const FACTORY_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

async fn wait_for_factory(agents: &AgentsHandle) -> Result<(), anyhow::Error> {
    let deadline = std::time::Instant::now() + FACTORY_WAIT;
    while agents.factory().is_none() {
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("no agent factory after {FACTORY_WAIT:?}; mount an `agent-loop` row");
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    Ok(())
}

/// The task as one Andrey message: §5's "an Andrey message ALWAYS gets a fresh sol answer wake"
/// is the whole reason `bough exec` needs no wake vocabulary of its own.
fn andrey_message(task: &str, at: chrono::DateTime<chrono::Utc>) -> Message {
    Message {
        id: bough_plugin_agents::MessageId::new(uuid::Uuid::now_v7().to_string()),
        from: Sender::Andrey,
        class: MailClass::Wake,
        text: task.to_string(),
        subject: subject_of(task),
        cites: Vec::new(),
        refs: Default::default(),
        mail_seq: None,
        at,
    }
}

/// A one-line subject: the first line of the task, clipped. Pure, so it is testable without a tree.
pub fn subject_of(task: &str) -> String {
    const MAX: usize = 80;
    let line = task.lines().next().unwrap_or("").trim();
    if line.chars().count() <= MAX {
        line.to_string()
    } else {
        line.chars().take(MAX - 1).collect::<String>() + "\u{2026}"
    }
}

/// Read the steps this task produced and render them (§17 Phase 2, V9: what exec printed is in
/// the ledger).
async fn read_back(
    ledger: &LedgerHandle,
    agent: &Agent,
    after: bough_plugin_ledger::Seq,
    print: Print,
) -> Result<String, anyhow::Error> {
    let steps = ledger
        .0
        .steps(&StepQuery {
            trajs: vec![agent.traj().clone()],
            after: Some(bough_plugin_ledger::Seq(after.0.saturating_sub(1))),
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await?;
    Ok(match render::last_wake(&steps) {
        Some(wake) => render::render(&steps, &wake, print),
        None => String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_subject_is_the_first_line_clipped() {
        assert_eq!(
            subject_of("fix the parser\nand the tests"),
            "fix the parser"
        );
        let long = "x".repeat(200);
        assert_eq!(subject_of(&long).chars().count(), 80);
    }

    #[test]
    fn an_empty_task_has_an_empty_subject() {
        assert_eq!(subject_of("   "), "");
    }
}
