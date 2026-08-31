//! Invariant: every key decision is a PURE function of the held state — `on_key` returns what
//! should happen (`Action`) and never does it, so the whole key map is testable without a shell,
//! a seam, or a clock (the `tui-preview` precedent). Item identity is a stable string key, never
//! a bare index: a refresh that reorders rows cannot silently move what is expanded.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};

use crate::data::PanelData;

/// The tab table. Adding a surface is adding a row here and a `match` arm in `view` — not a new
/// pane, not a new mode (old bough's one rule worth keeping).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Config,
    Connectors,
    Model,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Config, Tab::Connectors, Tab::Model];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Config => "config",
            Tab::Connectors => "connectors",
            Tab::Model => "model",
        }
    }

    pub fn step(self, by: i32) -> Tab {
        let here = Tab::ALL.iter().position(|t| *t == self).unwrap_or(0) as i32;
        let n = Tab::ALL.len() as i32;
        Tab::ALL[((here + by).rem_euclid(n)) as usize]
    }
}

/// One selectable item of the current tab, by stable key.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    ConfigRow(usize),
    Server(usize),
    Collector(usize),
    Agent(usize),
}

/// Everything the pane holds between frames.
#[derive(Debug, Default)]
pub struct PanelState {
    pub open: bool,
    pub tab: Option<Tab>,
    pub cursor: usize,
    pub scroll: usize,
    /// Set when the CURSOR moved (arrows, a tab switch), consumed by the next render: the reveal
    /// clamp runs then and only then. Without this the render snapped `scroll` back to the
    /// cursor's line on EVERY frame, so a wheel scroll bounced straight back (found live, ^t).
    pub reveal: bool,
    /// The viewport height of the LAST frame; `handle` has no `area`.
    pub height: u16,
    /// Config tab: show `render()`'s output verbatim instead of the joined rows.
    pub raw: bool,
    /// Item keys whose detail is open.
    pub expanded: BTreeSet<String>,
    /// The last `config/reload` line, verbatim from `ConfigReload::line()`.
    pub banner: Option<String>,
    /// Rows `kernel/rows-unresolved` reported parked, rendered one line each.
    pub unresolved: Vec<String>,
    pub data: Option<PanelData>,
    /// The last refresh's failure, rendered on the pane rather than a blank surface.
    pub error: Option<String>,
    /// The last store write's failure (a Foreign file, an io error).
    pub store_error: Option<String>,
    pub refreshing: bool,
    pub refreshed_at: Option<DateTime<Utc>>,
}

impl PanelState {
    pub fn tab(&self) -> Tab {
        self.tab.unwrap_or(Tab::Config)
    }

    /// The current tab's selectable items, in render order.
    pub fn items(&self) -> Vec<Item> {
        let Some(d) = &self.data else {
            return Vec::new();
        };
        match self.tab() {
            Tab::Config => (0..d.rows.len()).map(Item::ConfigRow).collect(),
            Tab::Connectors => (0..d.servers.len())
                .map(Item::Server)
                .chain((0..d.collectors.len()).map(Item::Collector))
                .collect(),
            Tab::Model => (0..d.model.agents.len()).map(Item::Agent).collect(),
        }
    }

    /// The stable key an item expands under.
    pub fn key_of(&self, item: &Item) -> Option<String> {
        let d = self.data.as_ref()?;
        Some(match item {
            Item::ConfigRow(i) => format!("c:{}", d.rows.get(*i)?.id),
            Item::Server(i) => format!("s:{}", d.servers.get(*i)?.name),
            Item::Collector(i) => format!("k:{}", d.collectors.get(*i)?.id),
            Item::Agent(i) => format!("a:{}", d.model.agents.get(*i)?.name),
        })
    }

    pub fn selected(&self) -> Option<Item> {
        self.items().get(self.cursor).cloned()
    }

    pub fn clamp_cursor(&mut self) {
        let n = self.items().len();
        self.cursor = self.cursor.min(n.saturating_sub(1));
    }

    pub fn switch(&mut self, tab: Tab) {
        if self.tab() != tab {
            self.tab = Some(tab);
            self.cursor = 0;
            self.scroll = 0;
        }
    }

    pub fn scroll_by(&mut self, delta: i32, painted: usize) {
        let max = painted.saturating_sub(self.height.max(1) as usize) as i64;
        let to = (self.scroll as i64 + i64::from(delta)).clamp(0, max.max(0));
        self.scroll = to as usize;
    }

    /// Whether a tick should refresh: nothing in flight, and `refresh_ms` since the last.
    pub fn due(&self, now: DateTime<Utc>, refresh_ms: u64) -> bool {
        if !self.open || self.refreshing {
            return false;
        }
        match self.refreshed_at {
            None => true,
            Some(then) => (now - then).num_milliseconds() >= refresh_ms as i64,
        }
    }
}

/// What a key asks for. The caller (the Arc pane) performs it; nothing here has effects.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Redraw,
    Refresh,
    Close,
    /// Flip `id` in the ui layer; the bool is the row's EFFECTIVE disabled right now.
    Toggle {
        id: String,
        effective_disabled: bool,
    },
    /// Clear this agent's `model_override` in the ledger's agents row.
    ClearOverride {
        agent: String,
    },
    /// `fire_now` this schedule job.
    Sweep {
        job: String,
    },
    /// Re-list this server's tools on the seam.
    RefreshTools {
        server: String,
    },
    Copy(String),
    Ignored,
}

/// PURE: what one key does to the state.
pub fn on_key(key: crossterm::event::KeyEvent, st: &mut PanelState) -> Action {
    use crossterm::event::KeyCode;
    match key.code {
        KeyCode::Esc => {
            st.open = false;
            Action::Close
        }
        KeyCode::Char('[') => {
            st.switch(st.tab().step(-1));
            Action::Refresh
        }
        KeyCode::Char(']') => {
            st.switch(st.tab().step(1));
            Action::Refresh
        }
        KeyCode::Char(c @ '1'..='3') => {
            st.switch(Tab::ALL[(c as usize) - ('1' as usize)]);
            Action::Refresh
        }
        KeyCode::Up => {
            st.cursor = st.cursor.saturating_sub(1);
            st.reveal = true;
            Action::Redraw
        }
        KeyCode::Down => {
            st.cursor = (st.cursor + 1).min(st.items().len().saturating_sub(1));
            st.reveal = true;
            Action::Redraw
        }
        KeyCode::Enter | KeyCode::Char(' ') => {
            let Some(key) = st.selected().and_then(|i| st.key_of(&i)) else {
                return Action::Ignored;
            };
            if !st.expanded.remove(&key) {
                st.expanded.insert(key);
            }
            Action::Redraw
        }
        KeyCode::Char('x') => act_on_selected(st),
        KeyCode::Char('s') => match st.selected() {
            Some(Item::Collector(i)) => {
                let job = st
                    .data
                    .as_ref()
                    .and_then(|d| d.collectors.get(i))
                    .and_then(|c| c.job.as_ref())
                    .map(|j| j.name.clone());
                match job {
                    Some(job) => Action::Sweep { job },
                    None => Action::Ignored,
                }
            }
            _ => Action::Ignored,
        },
        KeyCode::Char('r') => match st.selected() {
            Some(Item::Server(i)) => match st.data.as_ref().and_then(|d| d.servers.get(i)) {
                Some(s) if s.registered => Action::RefreshTools {
                    server: s.name.clone(),
                },
                _ => Action::Ignored,
            },
            _ => Action::Refresh,
        },
        KeyCode::Char('R') => {
            if st.tab() == Tab::Config {
                st.raw = !st.raw;
                st.scroll = 0;
                Action::Redraw
            } else {
                Action::Ignored
            }
        }
        KeyCode::Char('y') => match (&st.data, st.raw) {
            (Some(d), true) => Action::Copy(d.raw_dump.clone()),
            _ => Action::Ignored,
        },
        _ => Action::Ignored,
    }
}

/// `x`: the tab's one mutation, on the selected item. Config: flip the row in the ui layer.
/// Model: clear the agent's override. Everything else has no `x` — an act with nothing behind
/// it is worse than no act.
fn act_on_selected(st: &PanelState) -> Action {
    let Some(d) = &st.data else {
        return Action::Ignored;
    };
    match st.selected() {
        Some(Item::ConfigRow(i)) => match d.rows.get(i) {
            Some(r) if r.toggleable() => Action::Toggle {
                id: r.id.clone(),
                effective_disabled: r.disabled,
            },
            _ => Action::Ignored,
        },
        Some(Item::Agent(i)) => match d.model.agents.get(i) {
            Some(a) if a.model_override.is_some() => Action::ClearOverride {
                agent: a.name.clone(),
            },
            _ => Action::Ignored,
        },
        _ => Action::Ignored,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{AgentModelRow, ConfigRow, ModelData, PanelData};
    use crossterm::event::{KeyCode, KeyEvent};

    fn row(id: &str, disabled: bool, runtime_only: bool) -> ConfigRow {
        ConfigRow {
            id: id.into(),
            depth: 0,
            plugin: "p".into(),
            disabled,
            state: "active".into(),
            error: None,
            unmet: Vec::new(),
            created_by: "bundle:x".into(),
            disabled_by: "bundle:x".into(),
            config_by: "bundle:x".into(),
            ui_pin: None,
            runtime_only,
            config_lines: Vec::new(),
        }
    }

    fn state_with(rows: Vec<ConfigRow>) -> PanelState {
        PanelState {
            open: true,
            data: Some(PanelData {
                rows,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn esc_closes_and_reports_it() {
        let mut st = state_with(vec![row("a", false, false)]);
        assert_eq!(on_key(KeyEvent::from(KeyCode::Esc), &mut st), Action::Close);
        assert!(!st.open);
    }

    #[test]
    fn tabs_cycle_both_ways_and_by_digit() {
        let mut st = state_with(Vec::new());
        assert_eq!(st.tab(), Tab::Config);
        on_key(KeyEvent::from(KeyCode::Char(']')), &mut st);
        assert_eq!(st.tab(), Tab::Connectors);
        on_key(KeyEvent::from(KeyCode::Char('[')), &mut st);
        assert_eq!(st.tab(), Tab::Config);
        on_key(KeyEvent::from(KeyCode::Char('[')), &mut st);
        assert_eq!(st.tab(), Tab::Model);
        on_key(KeyEvent::from(KeyCode::Char('1')), &mut st);
        assert_eq!(st.tab(), Tab::Config);
    }

    #[test]
    fn x_toggles_only_a_toggleable_row_and_carries_the_effective_value() {
        let mut st = state_with(vec![row("a", true, false), row("rt", false, true)]);
        assert_eq!(
            on_key(KeyEvent::from(KeyCode::Char('x')), &mut st),
            Action::Toggle {
                id: "a".into(),
                effective_disabled: true
            }
        );
        st.cursor = 1;
        // A runtime-only mount has no config row; a toggle would be a lie the recompose ignores.
        assert_eq!(
            on_key(KeyEvent::from(KeyCode::Char('x')), &mut st),
            Action::Ignored
        );
    }

    #[test]
    fn x_on_an_agent_clears_an_override_and_only_an_override() {
        let mut st = state_with(Vec::new());
        st.tab = Some(Tab::Model);
        st.data.as_mut().unwrap().model = ModelData {
            agents: vec![
                AgentModelRow {
                    name: "sol".into(),
                    model_override: None,
                    answer: "m".into(),
                    unattended: "m".into(),
                },
                AgentModelRow {
                    name: "terra".into(),
                    model_override: Some("x".into()),
                    answer: "m".into(),
                    unattended: "x".into(),
                },
            ],
            ..Default::default()
        };
        assert_eq!(
            on_key(KeyEvent::from(KeyCode::Char('x')), &mut st),
            Action::Ignored
        );
        st.cursor = 1;
        assert_eq!(
            on_key(KeyEvent::from(KeyCode::Char('x')), &mut st),
            Action::ClearOverride {
                agent: "terra".into()
            }
        );
    }

    #[test]
    fn expansion_is_keyed_by_id_not_index() {
        let mut st = state_with(vec![row("a", false, false), row("b", false, false)]);
        on_key(KeyEvent::from(KeyCode::Enter), &mut st);
        assert!(st.expanded.contains("c:a"));
        // A refresh that reorders rows moves the item, not the expansion.
        st.data.as_mut().unwrap().rows.swap(0, 1);
        assert!(st.expanded.contains("c:a"));
        on_key(KeyEvent::from(KeyCode::Enter), &mut st);
        assert!(st.expanded.contains("c:b"), "{:?}", st.expanded);
    }

    #[test]
    fn raw_mode_is_the_config_tabs_alone_and_y_copies_the_dump() {
        let mut st = state_with(Vec::new());
        st.data.as_mut().unwrap().raw_dump = "fingerprint: abc\n".into();
        on_key(KeyEvent::from(KeyCode::Char('R')), &mut st);
        assert!(st.raw);
        assert_eq!(
            on_key(KeyEvent::from(KeyCode::Char('y')), &mut st),
            Action::Copy("fingerprint: abc\n".into())
        );
        st.tab = Some(Tab::Connectors);
        st.raw = false;
        assert_eq!(
            on_key(KeyEvent::from(KeyCode::Char('R')), &mut st),
            Action::Ignored
        );
    }

    #[test]
    fn a_wheel_scroll_does_not_arm_the_reveal_and_arrows_do() {
        let mut st = PanelState {
            open: true,
            height: 5,
            ..Default::default()
        };
        st.scroll_by(3, 40);
        assert_eq!(st.scroll, 3);
        assert!(
            !st.reveal,
            "a wheel scroll must not arm the reveal clamp, or the next render snaps it back"
        );
        let _ = on_key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Down,
                crossterm::event::KeyModifiers::NONE,
            ),
            &mut st,
        );
        assert!(st.reveal, "moving the cursor is what asks for a reveal");
    }

    #[test]
    fn cursor_clamps_to_the_tabs_items() {
        let mut st = state_with(vec![row("a", false, false)]);
        on_key(KeyEvent::from(KeyCode::Down), &mut st);
        assert_eq!(st.cursor, 0);
        st.cursor = 5;
        st.clamp_cursor();
        assert_eq!(st.cursor, 0);
    }
}
