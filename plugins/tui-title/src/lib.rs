//! Invariant: this row OWNS the terminal tab title and nothing else. It hears `tui/focus`,
//! renders `format` over the focused lane's name, and writes ONE OSC 0 sequence out of band —
//! it never draws a cell, never takes a pane, never writes a step. The title is STICKY
//! ([`TuiHandle::set_oob_sticky`]): the resident mounts long before a client attaches, and each
//! attach is a fresh terminal whose tab the shell re-titles by replay. Unloading writes `idle`
//! and stops the replay, so disabling the row by patch never leaves a stale lane name (§0.2).

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_tui_shell::{Tui, TuiFocusEvent, TuiHandle};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tui-title";

/// The placeholder `format` must carry: where the focused lane's name goes.
pub const LANE_SLOT: &str = "{lane}";

/// The sticky key this row writes under — ONE title, however many times focus moves.
pub const STICKY_KEY: &str = "tui-title";

/// The row's config. Every deployment-varying value is here; nothing is a `DEFAULT_` constant.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TitleConfig {
    /// The tab title while a lane holds focus. `{lane}` is replaced with the lane's name.
    pub format: String,
    /// The tab title while no lane does (a cold boot, an empty roster) — and what a terminal
    /// without a title stack is left showing after the row unloads.
    pub idle: String,
}

/// PURE: a name with every control character dropped. An escape smuggled through a lane's name
/// would terminate the OSC string early and leak the rest into the terminal as input.
pub fn sanitize(name: &str) -> String {
    name.chars().filter(|c| !c.is_control()).collect()
}

/// PURE: the title a focus state renders to.
pub fn title_for(cfg: &TitleConfig, lane: Option<&str>) -> String {
    match lane {
        Some(name) => cfg.format.replace(LANE_SLOT, &sanitize(name)),
        None => cfg.idle.clone(),
    }
}

/// PURE: the OSC 0 sequence that sets the icon name and the window/tab title together.
pub fn osc_title(title: &str) -> Vec<u8> {
    format!("\x1b]0;{title}\x07").into_bytes()
}

/// Re-derive the title from the shell's own state and write it if it moved. The `tui/focus`
/// payload names only what CHANGED (a pane hop carries no agent), so the shell's
/// [`TuiHandle::agent`] is the one authority — never the event's fields. Sticky, not one-shot:
/// the dedupe here is against OUR last derivation, and the shell's replay is what covers a
/// client that attaches without moving focus.
pub fn refresh(tui: &TuiHandle, cfg: &TitleConfig) {
    let lane = tui.agent().map(|a| a.name().to_string());
    let title = title_for(cfg, lane.as_deref());
    if invariant::record(lane, title.clone()) {
        tui.set_oob_sticky(STICKY_KEY, osc_title(&title));
    }
}

/// The row.
pub struct TuiTitlePlugin;

#[async_trait::async_trait]
impl Plugin for TuiTitlePlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = TitleConfig;

    fn inject() -> Inject {
        Inject::required(["tui"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let reject = |detail: String| Err(ConfigError::Rejected { detail });
        if !cfg.format.contains(LANE_SLOT) {
            return reject(format!(
                "format {:?} never names the lane: it must contain {LANE_SLOT}, because a \
                 constant tab title is this row doing nothing at all",
                cfg.format
            ));
        }
        for (field, value) in [("format", &cfg.format), ("idle", &cfg.idle)] {
            if value.chars().any(|c| c.is_control()) {
                return reject(format!(
                    "{field} contains a control character, which would terminate the OSC title \
                     sequence early and leak the rest into the terminal as input"
                ));
            }
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let tui = ctx
            .get::<Tui>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let tui = TuiHandle(tui.0.clone());

        // Undo on unload: the parting write is `idle` — a disabled row leaving a lane's name on
        // the tab would be the title lying at rest — and the sticky entry goes with it, so no
        // later attach is re-titled by a row that is gone.
        let (t, c) = (tui.clone(), cfg.clone());
        ctx.effect(move |e| async move {
            e.defer_sync(move || {
                t.write_oob(osc_title(&title_for(&c, None)));
                t.clear_oob_sticky(STICKY_KEY);
                invariant::forget();
            });
            Ok(())
        })
        .await?;

        // The title the tab shows RIGHT NOW: a row that mounts mid-session (a live patch) must
        // not wait for the next focus change to say who has the keyboard.
        refresh(&tui, &cfg);

        let (t, c) = (tui.clone(), cfg.clone());
        ctx.on::<TuiFocusEvent, _, _>(move |_req| {
            let (t, c) = (t.clone(), c.clone());
            async move { refresh(&t, &c) }
        })
        .await?;

        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(TuiTitlePlugin);

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(format: &str, idle: &str) -> TitleConfig {
        TitleConfig {
            format: format.to_string(),
            idle: idle.to_string(),
        }
    }

    #[test]
    fn the_title_names_the_lane_and_the_idle_title_names_nobody() {
        let c = cfg("{lane} · bough", "bough");
        assert_eq!(title_for(&c, Some("sol")), "sol · bough");
        assert_eq!(title_for(&c, None), "bough");
    }

    #[test]
    fn a_control_character_in_a_lane_name_never_reaches_the_sequence() {
        let c = cfg("{lane}", "bough");
        let title = title_for(&c, Some("sol\x1b]0;pwned\x07"));
        assert_eq!(title, "sol]0;pwned");
        assert!(osc_title(&title).iter().filter(|b| **b == 0x1b).count() == 1);
    }

    #[test]
    fn a_format_that_never_names_the_lane_is_rejected() {
        let err = TuiTitlePlugin::validate(&cfg("bough", "bough"))
            .expect_err("a constant title is the row doing nothing");
        assert!(format!("{err:?}").contains(LANE_SLOT));
    }

    #[test]
    fn a_control_character_in_config_is_rejected() {
        assert!(TuiTitlePlugin::validate(&cfg("{lane}\x07", "bough")).is_err());
        assert!(TuiTitlePlugin::validate(&cfg("{lane}", "bo\x1bugh")).is_err());
        assert!(TuiTitlePlugin::validate(&cfg("{lane} · bough", "bough")).is_ok());
    }

    #[test]
    fn the_sequence_is_osc_zero_bel() {
        assert_eq!(osc_title("sol · bough"), b"\x1b]0;sol \xc2\xb7 bough\x07");
    }
}
