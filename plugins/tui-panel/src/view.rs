//! Invariant: rendering is a PURE function of `(state, width, theme, focused)` — no clock, no
//! seam, no I/O — and every degradation is stated on screen (a clipped value says `…`, a failed
//! refresh renders in the error role, a parked dependent is a line, never a silence). Three
//! marks never conflate three facts (old bough's mcp lesson): configured, registered, and ready
//! are separate columns of a server row.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use bough_plugin_tui_shell::theme::Theme;

use crate::data::{ConfigRow, ServerRow};
use crate::state::{PanelState, Tab};

/// One clickable region, in LINE space (x=0..width unless narrower); the pane translates to
/// screen rects. Ids: `panel:tab:<title>`, `panel:item:<key>`.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub id: String,
    pub line: usize,
    pub x: u16,
    pub width: u16,
}

/// What one frame paints.
#[derive(Default)]
pub struct ViewOut {
    pub lines: Vec<Line<'static>>,
    pub hits: Vec<Hit>,
    /// Each selectable item's FIRST line, in item order — the reveal and the cursor mark key
    /// off this, so a hidden or reordered item cannot desynchronise them.
    pub item_lines: Vec<usize>,
}

pub fn lines(st: &PanelState, width: u16, theme: &Theme, focused: bool) -> ViewOut {
    let mut out = ViewOut::default();
    header(st, width, theme, &mut out);
    notices(st, theme, &mut out);
    let Some(_) = &st.data else {
        out.lines.push(dim(
            match &st.error {
                Some(e) => format!("panel failed to read the tree: {e}"),
                None => "reading the tree…".to_string(),
            },
            theme,
        ));
        return out;
    };
    if st.tab() == Tab::Config && st.raw {
        raw_tab(st, theme, &mut out);
        return out;
    }
    match st.tab() {
        Tab::Config => config_tab(st, width, theme, focused, &mut out),
        Tab::Connectors => connectors_tab(st, width, theme, focused, &mut out),
        Tab::Model => model_tab(st, width, theme, focused, &mut out),
    }
    out
}

// ---- chrome ---------------------------------------------------------------------------------

fn header(st: &PanelState, width: u16, theme: &Theme, out: &mut ViewOut) {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut col: u16 = 0;
    for (i, tab) in Tab::ALL.iter().enumerate() {
        let sep = if i == 0 { " " } else { " │ " };
        spans.push(Span::styled(
            sep.to_string(),
            Style::default().fg(theme.dim),
        ));
        col += sep.chars().count() as u16;
        let title = tab.title();
        let style = if *tab == st.tab() {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };
        out.hits.push(Hit {
            id: format!("panel:tab:{title}"),
            line: 0,
            x: col,
            width: title.chars().count() as u16,
        });
        spans.push(Span::styled(title.to_string(), style));
        col += title.chars().count() as u16;
    }
    let right = match &st.data {
        Some(d) if !d.fingerprint.is_empty() => {
            let fp = &d.fingerprint[..8.min(d.fingerprint.len())];
            let warn = match d.warnings.len() {
                0 => String::new(),
                1 => " · 1 warning".to_string(),
                n => format!(" · {n} warnings"),
            };
            format!("tree {fp}{warn} ")
        }
        _ => String::new(),
    };
    let used: u16 = col;
    let pad = (width as usize)
        .saturating_sub(used as usize)
        .saturating_sub(right.chars().count());
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(right, Style::default().fg(theme.dim)));
    out.lines.push(Line::from(spans));
}

fn notices(st: &PanelState, theme: &Theme, out: &mut ViewOut) {
    if let Some(line) = &st.banner {
        let style = if line.starts_with("config rejected") {
            Style::default().fg(theme.error)
        } else {
            Style::default().fg(theme.hint)
        };
        out.lines.push(Line::styled(format!(" {line}"), style));
    }
    if let Some(e) = &st.store_error {
        out.lines.push(Line::styled(
            format!(" {e}"),
            Style::default().fg(theme.error),
        ));
    }
    if let Some(e) = &st.error {
        out.lines.push(Line::styled(
            format!(" refresh failed: {e}"),
            Style::default().fg(theme.error),
        ));
    }
    for row in &st.unresolved {
        out.lines.push(Line::styled(
            format!(" {row}"),
            Style::default().fg(theme.warn),
        ));
    }
}

fn dim(text: String, theme: &Theme) -> Line<'static> {
    Line::styled(text, Style::default().fg(theme.dim))
}

fn clip(text: &str, cols: usize) -> String {
    if text.chars().count() <= cols {
        return text.to_string();
    }
    let mut s: String = text.chars().take(cols.saturating_sub(1)).collect();
    s.push('…');
    s
}

/// The selected item's first line wears the selection ground while the pane has the keyboard —
/// a mark drawn only where the keys actually are (the row-marker rule from the TUI brief).
fn mark_selected(st: &PanelState, focused: bool, theme: &Theme, out: &mut ViewOut) {
    if !focused {
        return;
    }
    let Some(line) = out.item_lines.get(st.cursor).copied() else {
        return;
    };
    if let Some(l) = out.lines.get_mut(line) {
        *l = l.clone().style(Style::default().bg(theme.sel_bg));
    }
}

fn push_item(out: &mut ViewOut, key: &str, first_line: usize, width: u16) {
    out.item_lines.push(first_line);
    out.hits.push(Hit {
        id: format!("panel:item:{key}"),
        line: first_line,
        x: 0,
        width,
    });
}

// ---- config ---------------------------------------------------------------------------------

fn state_style(row: &ConfigRow, theme: &Theme) -> Style {
    match row.state.as_str() {
        "failed" => Style::default().fg(theme.error),
        "pending" | "loading" | "unloading" => Style::default().fg(theme.warn),
        "active" => Style::default().fg(theme.added),
        _ => Style::default().fg(theme.dim),
    }
}

fn config_tab(st: &PanelState, width: u16, theme: &Theme, focused: bool, out: &mut ViewOut) {
    let d = st.data.as_ref().expect("caller checked");
    for row in &d.rows {
        let key = format!("c:{}", row.id);
        let open = st.expanded.contains(&key);
        let marker = if open { "▾" } else { "▸" };
        let indent = "  ".repeat(row.depth);
        let mut spans = vec![
            Span::styled(
                format!(" {indent}{marker} "),
                Style::default().fg(theme.dim),
            ),
            Span::styled(
                row.id.clone(),
                if row.disabled {
                    Style::default().fg(theme.dim)
                } else {
                    Style::default().fg(theme.fg)
                },
            ),
            Span::raw("  "),
            Span::styled(row.state.clone(), state_style(row, theme)),
        ];
        if row.disabled {
            spans.push(Span::styled(" · off", Style::default().fg(theme.dim)));
        }
        if let Some(pin) = row.ui_pin {
            spans.push(Span::styled(
                format!(" · panel pinned {}", if pin { "off" } else { "on" }),
                Style::default().fg(theme.hint),
            ));
        }
        if row.runtime_only {
            spans.push(Span::styled(
                " · runtime mount, no config row",
                Style::default().fg(theme.dim),
            ));
        }
        push_item(out, &key, out.lines.len(), width);
        out.lines.push(Line::from(spans));
        if let Some(e) = &row.error {
            out.lines.push(Line::styled(
                clip(&format!("   {indent}error: {e}"), width as usize),
                Style::default().fg(theme.error),
            ));
        }
        if !row.unmet.is_empty() {
            out.lines.push(Line::styled(
                clip(
                    &format!("   {indent}waiting on: {}", row.unmet.join(", ")),
                    width as usize,
                ),
                Style::default().fg(theme.warn),
            ));
        }
        if open {
            out.lines.push(dim(
                clip(
                    &format!(
                        "   {indent}plugin {} · created by {} · config by {} · disabled by {}",
                        row.plugin, row.created_by, row.config_by, row.disabled_by
                    ),
                    width as usize,
                ),
                theme,
            ));
            if row.config_lines.is_empty() {
                out.lines
                    .push(dim(format!("   {indent}(no config)"), theme));
            }
            for l in &row.config_lines {
                out.lines.push(Line::styled(
                    clip(&format!("   {indent}{l}"), width as usize),
                    Style::default().fg(theme.code),
                ));
            }
        }
    }
    if !d.warnings.is_empty() {
        out.lines.push(Line::from(""));
        for w in &d.warnings {
            out.lines.push(Line::styled(
                clip(&format!(" ⚠ {w}"), width as usize),
                Style::default().fg(theme.warn),
            ));
        }
    }
    mark_selected(st, focused, theme, out);
}

fn raw_tab(st: &PanelState, theme: &Theme, out: &mut ViewOut) {
    let d = st.data.as_ref().expect("caller checked");
    out.lines.push(dim(
        " raw · the dump renderer's output, verbatim · y copies · R back".to_string(),
        theme,
    ));
    for l in d.raw_dump.lines() {
        out.lines
            .push(Line::styled(l.to_string(), Style::default().fg(theme.code)));
    }
}

// ---- connectors -----------------------------------------------------------------------------

/// Configured / registered / ready, as one glyph each row leads with. `●` connected and ready,
/// `◐` registered but not ready (a resident that died and is restarting), `○` never connected,
/// `·` disabled.
fn server_glyph(s: &ServerRow) -> &'static str {
    if s.disabled {
        "·"
    } else if s.ready == Some(true) {
        "●"
    } else if s.registered {
        "◐"
    } else {
        "○"
    }
}

fn connectors_tab(st: &PanelState, width: u16, theme: &Theme, focused: bool, out: &mut ViewOut) {
    let d = st.data.as_ref().expect("caller checked");
    out.lines.push(Line::styled(
        " mcp servers",
        Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
    ));
    if d.servers.is_empty() {
        out.lines.push(dim("   none configured".to_string(), theme));
    }
    for s in &d.servers {
        let key = format!("s:{}", s.name);
        let open = st.expanded.contains(&key);
        let mut spans = vec![
            Span::styled(
                format!("  {} ", server_glyph(s)),
                match (
                    s.disabled,
                    s.ready,
                    s.error.is_some() || s.state == "failed",
                ) {
                    (true, _, _) => Style::default().fg(theme.dim),
                    (_, Some(true), _) => Style::default().fg(theme.added),
                    (_, _, true) => Style::default().fg(theme.error),
                    _ => Style::default().fg(theme.warn),
                },
            ),
            Span::styled(
                s.name.clone(),
                if s.disabled {
                    Style::default().fg(theme.dim)
                } else {
                    Style::default().fg(theme.fg)
                },
            ),
        ];
        let mut tail = format!("  {}", s.detail);
        if let Some(n) = s.tools {
            tail.push_str(&format!(" · {n} tool{}", if n == 1 { "" } else { "s" }));
        }
        if s.disabled {
            tail.push_str(" · off");
        } else if !s.registered {
            tail.push_str(" · never connected");
        } else if s.ready == Some(false) {
            tail.push_str(" · not ready");
        }
        spans.push(Span::styled(
            clip(&tail, (width as usize).saturating_sub(4 + s.name.len())),
            Style::default().fg(theme.dim),
        ));
        push_item(out, &key, out.lines.len(), width);
        out.lines.push(Line::from(spans));
        if let Some(e) = &s.error {
            out.lines.push(Line::styled(
                clip(&format!("     {e}"), width as usize),
                Style::default().fg(theme.error),
            ));
        }
        if open {
            out.lines.push(dim(
                clip(
                    &format!(
                        "     row {} · state {} · r re-lists tools",
                        s.owner_id, s.state
                    ),
                    width as usize,
                ),
                theme,
            ));
        }
    }
    out.lines.push(Line::from(""));
    out.lines.push(Line::styled(
        " collectors",
        Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
    ));
    if d.collectors.is_empty() {
        out.lines
            .push(dim("   none in the tree".to_string(), theme));
    }
    for c in &d.collectors {
        let key = format!("k:{}", c.id);
        let open = st.expanded.contains(&key);
        let mut text = format!("  {}  {}", c.id, c.cadence);
        if let Some(j) = &c.job {
            if let Some(next) = j.next {
                text.push_str(&format!(" · next {}", next.format("%H:%M:%S")));
            }
            if let Some(last) = &j.last {
                text.push_str(&format!(" · {last}"));
            }
        } else if !c.disabled {
            text.push_str(" · no job registered");
        }
        if c.disabled {
            text.push_str(" · off");
        }
        push_item(out, &key, out.lines.len(), width);
        out.lines.push(Line::styled(
            clip(&text, width as usize),
            if c.disabled {
                Style::default().fg(theme.dim)
            } else {
                Style::default().fg(theme.fg)
            },
        ));
        if open {
            out.lines.push(dim(
                clip(
                    &format!(
                        "     sweeps {} · plugin {} · s sweeps now",
                        c.scope, c.plugin
                    ),
                    width as usize,
                ),
                theme,
            ));
        }
    }
    mark_selected(st, focused, theme, out);
}

// ---- model ----------------------------------------------------------------------------------

fn model_tab(st: &PanelState, width: u16, theme: &Theme, focused: bool, out: &mut ViewOut) {
    let d = st.data.as_ref().expect("caller checked");
    let m = &d.model;
    match (&m.interactive, &m.unattended) {
        (Some(interactive), Some(unattended)) => {
            out.lines.push(Line::from(vec![
                Span::styled(" policy  ", Style::default().fg(theme.dim)),
                Span::styled("interactive ", Style::default().fg(theme.dim)),
                Span::styled(interactive.clone(), Style::default().fg(theme.fg)),
                Span::styled("  ·  unattended ", Style::default().fg(theme.dim)),
                Span::styled(unattended.clone(), Style::default().fg(theme.fg)),
            ]));
            out.lines.push(dim(
                "         interactive answers Andrey and is never overridable; unattended may be overridden per lane"
                    .to_string(),
                theme,
            ));
        }
        _ => out.lines.push(Line::styled(
            " no model.policy row is mounted",
            Style::default().fg(theme.warn),
        )),
    }
    out.lines.push(Line::from(""));
    out.lines.push(Line::styled(
        " agents",
        Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
    ));
    if m.agents.is_empty() {
        out.lines
            .push(dim("   no agent rows in the ledger".to_string(), theme));
    }
    for a in &m.agents {
        let key = format!("a:{}", a.name);
        let mut text = format!(
            "  {}  answers → {} · unattended → {}",
            a.name, a.answer, a.unattended
        );
        if let Some(over) = &a.model_override {
            text.push_str(&format!(" · override {over} (x clears)"));
        }
        push_item(out, &key, out.lines.len(), width);
        out.lines.push(Line::styled(
            clip(&text, width as usize),
            Style::default().fg(theme.fg),
        ));
    }
    out.lines.push(Line::from(""));
    if !m.adapters.is_empty() {
        let list = m
            .adapters
            .iter()
            .map(|a| format!("{} ({})", a.name, a.claim))
            .collect::<Vec<_>>()
            .join(" · ");
        out.lines.push(dim(
            clip(&format!(" adapters  {list}"), width as usize),
            theme,
        ));
    }
    if !m.env_keys.is_empty() {
        let list = m
            .env_keys
            .iter()
            .map(|(name, set)| format!("{name} {}", if *set { "set" } else { "MISSING" }))
            .collect::<Vec<_>>()
            .join(" · ");
        out.lines.push(dim(
            clip(&format!(" keys      {list}"), width as usize),
            theme,
        ));
        out.lines.push(dim(
            "           a key is read at call time; set means the variable exists, not that it works"
                .to_string(),
            theme,
        ));
    }
    match &m.last_model {
        Some(model) => out.lines.push(dim(
            clip(&format!(" last request ran {model}"), width as usize),
            theme,
        )),
        None => out.lines.push(dim(
            " no request has run in this process".to_string(),
            theme,
        )),
    }
    mark_selected(st, focused, theme, out);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{AgentModelRow, ConfigRow, ModelData, PanelData, ServerRow};

    fn theme() -> Theme {
        bough_plugin_tui_shell::theme::Theme::of(bough_plugin_tui_shell::theme::ThemeName::Dark)
    }

    fn text(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn base_state() -> PanelState {
        PanelState {
            open: true,
            data: Some(PanelData {
                fingerprint: "abcdef0123456789".into(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn the_header_names_every_tab_and_the_fingerprint() {
        let st = base_state();
        let out = lines(&st, 100, &theme(), false);
        let head = &text(&out.lines)[0];
        for t in ["config", "connectors", "model", "tree abcdef01"] {
            assert!(head.contains(t), "{head}");
        }
        assert_eq!(
            out.hits
                .iter()
                .filter(|h| h.id.starts_with("panel:tab:"))
                .count(),
            3
        );
    }

    #[test]
    fn a_config_row_states_who_wrote_it_when_opened() {
        let mut st = base_state();
        st.data.as_mut().unwrap().rows = vec![ConfigRow {
            id: "old-feed".into(),
            depth: 0,
            plugin: "old-feed-adapter".into(),
            disabled: true,
            state: "inactive".into(),
            error: None,
            unmet: Vec::new(),
            created_by: "bundle:bough-tui-app".into(),
            disabled_by: "ui".into(),
            config_by: "bundle:bough-tui-app".into(),
            ui_pin: Some(true),
            runtime_only: false,
            config_lines: vec!["poll_ms: 30000".into()],
        }];
        st.expanded.insert("c:old-feed".into());
        let body = text(&lines(&st, 120, &theme(), false).lines).join("\n");
        assert!(body.contains("old-feed"), "{body}");
        assert!(body.contains("disabled by ui"), "{body}");
        assert!(body.contains("panel pinned off"), "{body}");
        assert!(body.contains("poll_ms: 30000"), "{body}");
    }

    #[test]
    fn a_parked_row_shows_what_it_waits_on() {
        let mut st = base_state();
        st.data.as_mut().unwrap().rows = vec![ConfigRow {
            id: "hello".into(),
            depth: 0,
            plugin: "hello".into(),
            disabled: false,
            state: "pending".into(),
            error: None,
            unmet: vec!["greeting".into()],
            created_by: "bundle:x".into(),
            disabled_by: "bundle:x".into(),
            config_by: "bundle:x".into(),
            ui_pin: None,
            runtime_only: false,
            config_lines: Vec::new(),
        }];
        let body = text(&lines(&st, 120, &theme(), false).lines).join("\n");
        assert!(body.contains("waiting on: greeting"), "{body}");
    }

    #[test]
    fn a_server_never_conflates_registered_and_ready() {
        let mut st = base_state();
        st.tab = Some(Tab::Connectors);
        let s = |name: &str, registered: bool, ready: Option<bool>| ServerRow {
            name: name.into(),
            owner_id: "mcp.rmcp".into(),
            detail: "http: https://x".into(),
            disabled: false,
            registered,
            ready,
            tools: Some(3),
            state: "active".into(),
            error: None,
        };
        st.data.as_mut().unwrap().servers = vec![
            s("up", true, Some(true)),
            s("wobbly", true, Some(false)),
            s("never", false, None),
        ];
        let body = text(&lines(&st, 120, &theme(), false).lines);
        let row = |name: &str| body.iter().find(|l| l.contains(name)).unwrap().clone();
        assert!(row("up").contains('●'), "{}", row("up"));
        assert!(row("wobbly").contains('◐') && row("wobbly").contains("not ready"));
        assert!(row("never").contains('○') && row("never").contains("never connected"));
    }

    #[test]
    fn the_model_tab_says_the_override_rule_out_loud() {
        let mut st = base_state();
        st.tab = Some(Tab::Model);
        st.data.as_mut().unwrap().model = ModelData {
            interactive: Some("sol-m".into()),
            unattended: Some("terra-m".into()),
            agents: vec![AgentModelRow {
                name: "terra".into(),
                model_override: Some("special".into()),
                answer: "sol-m".into(),
                unattended: "special".into(),
            }],
            env_keys: vec![
                ("ANTHROPIC_API_KEY".into(), true),
                ("OPENAI_API_KEY".into(), false),
            ],
            ..Default::default()
        };
        let body = text(&lines(&st, 140, &theme(), false).lines).join("\n");
        assert!(body.contains("override special (x clears)"), "{body}");
        assert!(body.contains("never overridable"), "{body}");
        assert!(body.contains("OPENAI_API_KEY MISSING"), "{body}");
        assert!(body.contains("set means the variable exists"), "{body}");
    }

    #[test]
    fn raw_mode_is_the_renderers_output_verbatim() {
        let mut st = base_state();
        st.raw = true;
        st.data.as_mut().unwrap().raw_dump = "fingerprint: abc\nrows: []\n".into();
        let body = text(&lines(&st, 120, &theme(), false).lines);
        assert_eq!(body[2], "fingerprint: abc");
        assert_eq!(body[3], "rows: []");
    }

    #[test]
    fn a_rejected_reload_renders_in_the_error_role_by_its_own_words() {
        let mut st = base_state();
        st.banner = Some("config rejected, last good tree still running: bad yaml".into());
        let out = lines(&st, 120, &theme(), false);
        let l = &out.lines[1];
        assert_eq!(l.style.fg, Some(theme().error));
        assert!(text(std::slice::from_ref(l))[0].contains("config rejected"));
    }
}
