//! Invariant: neither system pass invents behaviour. `schedule-catch-up` makes exactly the call
//! `catch-up-on-wake` makes (`Agent::request_wake(WakeKind::Catchup, WakeCause::CatchUp)`), so
//! "one catch-up per active agent" has ONE implementation; and `schedule-reconsolidate` resolves
//! its command BY NAME through `ctx.commands` and never reaches into Phase 4's code.
//!
//! P6-D2: a command that does not exist yet makes the JOB return [`JobOutcome::Pending`]. The ROW
//! stays ACTIVE — §0.2 makes an enabled row that never activates a boot failure, so a row that
//! stayed PENDING would break the boot instead of waiting politely.

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_agents::WakeKind;
use bough_plugin_agents::{AgentKind, Agents, AgentsHandle, WakeCause};
use bough_plugin_commands::{CommandCx, CommandName, Commands, CommandsHandle, Invocation};
use bough_plugin_ledger::AgentName;
use bough_plugin_schedule::{Cadence, Job, JobFire, JobName, JobOutcome, JobSpec, Schedule};

/// The catch-up row's catalog name.
pub const CATCH_UP_NAME: &str = "schedule-catch-up";
/// The reconsolidation row's catalog name.
pub const RECONSOLIDATE_NAME: &str = "schedule-reconsolidate";

/// The job name the catch-up row registers. Unique in the tree (§9).
pub const CATCH_UP_JOB: &str = "system:catch-up";
/// The job name the reconsolidation row registers.
pub const RECONSOLIDATE_JOB: &str = "system:reconsolidate";

/// PURE: the config spelling of an agent kind, so `kinds: ["resident"]` means something checkable.
pub fn kind_str(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Resident => "resident",
        AgentKind::Worker => "worker",
        AgentKind::Fork => "fork",
    }
}

/// The catch-up pass's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatchUpConfig {
    pub cadence: Cadence,
    pub catch_up: bool,
    /// Which agent kinds get a catch-up wake. `["resident"]`.
    pub kinds: Vec<String>,
}

/// The reconsolidation pass's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReconsolidateConfig {
    pub cadence: Cadence,
    pub catch_up: bool,
    /// The command to invoke, BY NAME, through `ctx.commands`. Absent command ⇒ the job returns
    /// [`JobOutcome::Pending`], the row stays ACTIVE, and the next cadence tries again (P6-D2).
    pub command: String,
    /// Whose lane the command runs in, when it needs one.
    pub agent: Option<String>,
}

/// The catch-up job: one `request_wake` per agent of a configured kind that is not disposed.
pub struct CatchUpJob {
    pub agents: AgentsHandle,
    pub kinds: Vec<String>,
}

#[async_trait::async_trait]
impl Job for CatchUpJob {
    /// EXACTLY the call `catch-up-on-wake` makes, so "one catch-up per active agent" has one
    /// implementation. A disposed agent is terminal (§2) and is never asked.
    async fn run(&self, _fire: JobFire) -> JobOutcome {
        let mut eligible = 0usize;
        let mut asked = 0usize;
        let mut woke = 0usize;
        for agent in self.agents.list() {
            if agent.is_disposed() || !self.kinds.iter().any(|k| k == kind_str(agent.kind())) {
                continue;
            }
            eligible += 1;
            asked += 1;
            if let bough_plugin_agents::WakeRequest::Started(_) = agent
                .request_wake(WakeKind::Catchup, WakeCause::CatchUp)
                .await
            {
                woke += 1;
            }
        }
        crate::invariant::record(crate::invariant::Sweep { eligible, asked });
        JobOutcome::Ran {
            detail: format!("asked {asked} agent(s); {woke} had something queued"),
        }
    }
}

/// The reconsolidation job: resolve `command` through `ctx.commands` and dispatch it.
pub struct ReconsolidateJob {
    /// The row's own context: a dispatch needs one, and the row's is the one whose scope the
    /// command runs under.
    pub ctx: Context,
    pub commands: Option<CommandsHandle>,
    pub agents: AgentsHandle,
    pub command: String,
    pub agent: Option<String>,
}

#[async_trait::async_trait]
impl Job for ReconsolidateJob {
    /// P6-D2: a command that does not exist yet is `Pending`, never `Failed`. The job says so and
    /// is tried again next cadence; the ROW stays active.
    async fn run(&self, fire: JobFire) -> JobOutcome {
        let Some(commands) = &self.commands else {
            return JobOutcome::Pending {
                reason: format!(
                    "`{}` cannot run: this tree has no commands seam yet",
                    self.command
                ),
            };
        };
        let name = CommandName::new(&self.command);
        let agent = self
            .agent
            .as_ref()
            .and_then(|a| self.agents.by_name(&AgentName::new(a)));
        let scope = agent.as_ref().map(|a| a.name().clone());
        if commands.resolve(&name, scope.as_ref()).is_none() {
            return JobOutcome::Pending {
                reason: format!("no command named `{}` in this tree yet", self.command),
            };
        }
        // A named agent that is not here is NOT a silent skip: the pass says what it wanted.
        if let (Some(want), None) = (self.agent.as_ref(), agent.as_ref()) {
            return JobOutcome::Pending {
                reason: format!("no live agent named `{want}` to run `{}` in", self.command),
            };
        }
        let inv = Invocation {
            name: name.clone(),
            raw: format!("{}{}", commands.prefix(), self.command),
            args: Vec::new(),
        };
        let cx = CommandCx {
            ctx: self.ctx.clone(),
            agent,
            at: fire.at,
        };
        match commands.dispatch(inv, cx).await {
            Ok(out) => JobOutcome::Ran {
                detail: out.text.lines().next().unwrap_or("").to_string(),
            },
            Err(e) => JobOutcome::Failed {
                error: e.to_string(),
            },
        }
    }
}

/// The catch-up row.
pub struct CatchUpPlugin;

#[async_trait::async_trait]
impl Plugin for CatchUpPlugin {
    const NAME: &'static str = CATCH_UP_NAME;
    type Config = CatchUpConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["schedule", "agents"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        cfg.cadence.check().map_err(|e| ConfigError::Rejected {
            detail: e.to_string(),
        })?;
        if cfg.kinds.is_empty() {
            return Err(ConfigError::Rejected {
                detail: "`kinds` is empty: this pass would wake nobody, forever".to_string(),
            });
        }
        let known = ["resident", "worker", "fork"];
        if let Some(bad) = cfg.kinds.iter().find(|k| !known.contains(&k.as_str())) {
            return Err(ConfigError::Rejected {
                detail: format!("`{bad}` is not an agent kind ({})", known.join(", ")),
            });
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let schedule = ctx
            .get::<Schedule>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let agents = ctx
            .get::<Agents>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        schedule
            .0
            .register(
                &ctx,
                JobSpec {
                    name: JobName::new(CATCH_UP_JOB),
                    cadence: cfg.cadence.clone(),
                    catch_up: cfg.catch_up,
                    job: Arc::new(CatchUpJob {
                        agents: (*agents).clone(),
                        kinds: cfg.kinds.clone(),
                    }),
                },
            )
            .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

/// The reconsolidation row.
pub struct ReconsolidatePlugin;

#[async_trait::async_trait]
impl Plugin for ReconsolidatePlugin {
    const NAME: &'static str = RECONSOLIDATE_NAME;
    type Config = ReconsolidateConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["schedule", "agents"])
            .union(&bough_kernel::Inject::optional(["commands"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        cfg.cadence.check().map_err(|e| ConfigError::Rejected {
            detail: e.to_string(),
        })?;
        if cfg.command.trim().is_empty() {
            return Err(ConfigError::Rejected {
                detail: "`command` is empty: this pass would have nothing to invoke".to_string(),
            });
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let schedule = ctx
            .get::<Schedule>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let agents = ctx
            .get::<Agents>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        // OPTIONAL, and absent is the ordinary case until Phase 4's `/reconsolidate` exists.
        let commands = ctx
            .try_get::<Commands>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        schedule
            .0
            .register(
                &ctx,
                JobSpec {
                    name: JobName::new(RECONSOLIDATE_JOB),
                    cadence: cfg.cadence.clone(),
                    catch_up: cfg.catch_up,
                    job: Arc::new(ReconsolidateJob {
                        ctx: ctx.clone(),
                        commands: commands.map(|c| (*c).clone()),
                        agents: (*agents).clone(),
                        command: cfg.command.clone(),
                        agent: cfg.agent.clone(),
                    }),
                },
            )
            .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(CatchUpPlugin);
bough_kernel::register_plugin!(ReconsolidatePlugin);
