//! The MCP tab: which servers exist, which this session may call, and which are
//! live (port of `src/tui/components/Mcp.tsx`).
//!
//! THE INVARIANT THIS HOLDS: **the three states are never conflated.** A server
//! can be *registered* (a row in the registry), *granted* to this session, and
//! *connected* — and they are independent. Registering is not granting
//! (`mcp/config.rs`); a granted server whose command is broken is granted and
//! dead; an authorized remote server is not necessarily connected. The old
//! client showed one dot and one word, so "why can't the agent call this" had no
//! answer on the screen. Here every row carries all four facts: grant, live tool
//! count, authorization, and transport.
//!
//! SECOND — **nothing here is cached.** The status object is a prop, re-fetched
//! by the host on every entry into this tab, because grants and connections
//! change between turns and a panel showing last minute's MCP state is worse
//! than one showing none. This module keeps no state of its own to make that
//! impossible.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use bough_core::mcp::config::ServerConfig;
use bough_core::mcp::keychain::is_covered_host;

use crate::api::McpStatus;
use crate::components::panel::{legend_line, paint_rows, window_around};
use crate::components::{accent, warn};
use crate::store::selectors::clip;

/// The visible slice. Chrome is the message and the legend, which is always
/// last.
pub fn mcp_window(
    count: usize,
    selected: usize,
    rows: usize,
    chrome: usize,
) -> (usize, usize, usize, bool) {
    // Two legend rows: the mark legend and the key legend. Keep the two in step
    // with what `mcp_lines` actually paints.
    let avail = rows.saturating_sub(chrome + 2);
    // Content over indicators when it is tight — a lone `1/40` row above no
    // servers at all is a position report about a list nobody can see.
    let counter = count > avail && avail >= 2;
    let height = avail.saturating_sub(usize::from(counter));
    let (start, end) = window_around(selected, count, height);
    (start, end, height, counter)
}

/// The entry carries its own credential — a static `Authorization` header, which
/// `expand_headers` resolves (from `${VAR}` or the keychain) at connect time.
///
/// Any auth-bearing header counts, not only a keychain reference: a server given
/// a literal or an env-var token is equally not waiting for anyone to press `a`.
pub fn has_static_auth(entry: Option<&ServerConfig>) -> bool {
    entry.is_some_and(|e| {
        e.headers
            .iter()
            .any(|(k, v)| k.to_lowercase() == "authorization" && !v.trim().is_empty())
    })
}

/// The dim tail of an MCP row: grant, live connection, credentials, transport.
// Two of the three arms land on "keychain" — a stored header and a covered host
// are different reasons for the same word. Kept as separate arms because that is
// the ladder in src/tui/components/Mcp.tsx:100-107, and the conditions are the
// documentation.
#[allow(clippy::if_same_then_else)]
pub fn mcp_detail(status: &McpStatus, name: &str) -> String {
    let granted = status.active.iter().any(|n| n == name);
    let conn = status.connections.iter().find(|c| c.server == name);
    let auth = status.auth.get(name);
    let entry = status.registry.servers.get(name);
    let mut parts: Vec<String> = vec![if granted {
        "granted".into()
    } else {
        "off".into()
    }];
    if let Some(c) = conn.filter(|c| c.alive) {
        parts.push(format!("{} tools", c.tool_count));
    }
    if let Some(err) = conn.and_then(|c| c.error.as_ref()) {
        parts.push(clip(err, 40));
    }
    // "needs auth" is about tokens BOUGH stored, and saying it of a server that
    // already carries a credential is the lie this row used to tell: `sync-mcp`
    // writes an `Authorization` header referencing the grant Claude Code holds,
    // and the panel still said "needs auth" — so the one server that needed
    // nothing pressed was the one the UI sent you to press `a` on, where
    // authorizing fails because the provider does not do dynamic registration.
    //
    // Three states, in order of how the connection will actually be made: a
    // token bough stored; an explicit header on the entry; the machine's Claude
    // Code credential for a host it covers (`mcp/keychain.rs`). All derived from
    // the entry alone — the panel must not spawn a keychain read to paint a row
    // — so this says what will be TRIED, not that the server has already
    // accepted it.
    if let Some(flag) = auth {
        parts.push(
            if flag.authorized {
                "authed"
            } else if has_static_auth(entry) {
                "keychain"
            } else if is_covered_host(entry.and_then(|e| e.url.as_deref()).unwrap_or("")) {
                "keychain"
            } else {
                "needs auth"
            }
            .to_string(),
        );
    }
    let transport = entry
        .and_then(|e| e.url.clone().or_else(|| e.command.clone()))
        .unwrap_or_default();
    parts.push(clip(&transport, 30));
    parts
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// What the three status glyphs mean, for the ones actually on screen.
///
/// `●`/`◐`/`○` carry the whole state of a row and were explained nowhere. The
/// gap this closes was reported exactly as it feels: authorize a server, watch
/// it stay a half circle, and have no way to learn that `◐` is about a
/// CONNECTION and not about the authorization that just succeeded.
///
/// Present-only, like the tree's: a list where everything is connected should
/// not spend columns explaining `○`.
pub fn status_legend(names: &[String], status: &McpStatus) -> Vec<String> {
    let state = |n: &String| -> &'static str {
        if status.connections.iter().any(|c| &c.server == n && c.alive) {
            "alive"
        } else if status.active.contains(n) {
            "granted"
        } else {
            "off"
        }
    };
    let seen: Vec<&str> = names.iter().map(state).collect();
    let mut out: Vec<String> = Vec::new();
    if seen.contains(&"alive") {
        out.push("● connected".into());
    }
    if seen.contains(&"granted") {
        out.push("◐ granted, not connected — c connects".into());
    }
    if seen.contains(&"off") {
        out.push("○ not granted — ⏎ grants".into());
    }
    out
}

pub struct McpTabProps<'a> {
    /// `None` while loading. Never cached by the caller.
    pub status: Option<&'a McpStatus>,
    pub selected: usize,
    pub message: Option<&'a str>,
    /// Rows this tab may paint. It had NONE — it listed every registered server
    /// and then the legend, so an install with a dozen servers overran the
    /// panel.
    pub rows: usize,
    /// Columns available, so the legend degrades instead of being cut mid-word.
    pub cols: usize,
    /// The server URL being typed, or `None` when the buffer is closed.
    /// Registration used to mean hand-editing `~/.bough/mcp.json` and restarting
    /// the server, which is why this tab's legend was one verb long.
    pub entry: Option<&'a str>,
}

impl Default for McpTabProps<'_> {
    fn default() -> Self {
        McpTabProps {
            status: None,
            selected: 0,
            message: None,
            rows: 20,
            cols: 80,
            entry: None,
        }
    }
}

/// Registered names, sorted — the list the cursor and the digits both address.
pub fn mcp_names(status: &McpStatus) -> Vec<String> {
    let mut names: Vec<String> = status.registry.servers.keys().cloned().collect();
    names.sort();
    names
}

/// The lines this tab paints, in order.
pub fn mcp_lines(p: &McpTabProps) -> Vec<Line<'static>> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let Some(status) = p.status else {
        return vec![Line::from(Span::styled("loading…", dim))];
    };
    let names = mcp_names(status);
    // Nine keys is the longest legend bough has, and at 80 columns the terminal
    // cut it after `F forg` — the keys that authorize a server and forget its
    // credentials, gone. `legend_line` drops whole items and says it did.
    let legend = match p.entry {
        Some(_) => "⏎ registers · ⌫ back · esc cancels".to_string(),
        // With no servers registered, seven of these nine keys act on a row that
        // does not exist. A legend listing inert keys is a legend the reader
        // stops trusting, and the empty state already says what to do.
        None => {
            let items: Vec<String> = if names.is_empty() {
                ["n add a server by URL", "esc back"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            } else {
                [
                    "↑↓ move",
                    "1-9 pick",
                    "⏎ grant/revoke",
                    "c test",
                    "r restart",
                    "a authorize",
                    "n add",
                    "F forget",
                    "d delete",
                    "esc back",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect()
            };
            legend_line(&items, Some(p.cols))
        }
    };
    let legend_line_row = Line::from(Span::styled(legend, dim));

    // The prompt REPLACES the list's affirmative while it is open — see
    // `confirm`, which takes ⏎ before any tab does.
    let prompt: Option<Line<'static>> = p.entry.map(|entry| {
        Line::from(vec![
            Span::styled("new server ", dim),
            Span::raw(entry.to_string()),
            Span::styled("▌", Style::default().fg(accent())),
        ])
    });

    let mut out: Vec<Line<'static>> = Vec::new();
    if names.is_empty() {
        if let Some(prompt) = prompt {
            out.push(prompt);
        }
        out.push(Line::from(Span::styled(
            if p.entry.is_none() {
                "no MCP servers configured — n adds one by URL"
            } else {
                ""
            },
            dim,
        )));
        out.push(legend_line_row);
        return out;
    }

    let chrome = usize::from(p.message.is_some()) + usize::from(prompt.is_some());
    let (start, end, height, counter) = mcp_window(names.len(), p.selected, p.rows, chrome);
    if let Some(prompt) = prompt {
        out.push(prompt);
    }
    if let Some(message) = p.message {
        // NOT clipped when it carries a URL: an authorization URL that ends in
        // "…" is a URL nobody can open, which is the whole point of it.
        let text = if message.contains("://") {
            message.to_string()
        } else {
            clip(message, 96)
        };
        out.push(Line::from(Span::styled(text, Style::default().fg(warn()))));
    }
    if height > 0 {
        for (i, name) in names[start..end.min(names.len())].iter().enumerate() {
            let idx = start + i;
            let granted = status.active.contains(name);
            let alive = status
                .connections
                .iter()
                .any(|c| &c.server == name && c.alive);
            let sel = idx == p.selected;
            let color = if alive {
                Some(accent())
            } else if granted {
                Some(warn())
            } else {
                None
            };
            let mut glyph_style = Style::default();
            if !sel {
                if let Some(c) = color {
                    glyph_style = glyph_style.fg(c);
                }
            }
            if !granted {
                glyph_style = glyph_style.add_modifier(Modifier::DIM);
            }
            out.push(Line::from(vec![
                Span::styled(
                    if i < 9 {
                        format!("{} ", i + 1)
                    } else {
                        "  ".into()
                    },
                    dim,
                ),
                Span::styled(
                    if sel { "❯ " } else { "  " },
                    if sel {
                        Style::default().fg(accent())
                    } else {
                        Style::default()
                    },
                ),
                Span::styled(
                    if alive {
                        "●"
                    } else if granted {
                        "◐"
                    } else {
                        "○"
                    },
                    glyph_style,
                ),
                Span::styled(
                    format!(" {name}"),
                    if sel { bold } else { Style::default() },
                ),
                Span::styled(format!("  {}", mcp_detail(status, name)), dim),
            ]));
        }
    }
    if counter {
        out.push(Line::from(Span::styled(
            format!("— {end}/{} —", names.len()),
            dim,
        )));
    }
    // Marks first, then keys — the glyph is what the reader is stuck on. Both
    // are counted in `mcp_window`'s reservation; keep the two in step.
    out.push(Line::from(Span::styled(
        legend_line(&status_legend(&names, status), Some(p.cols)),
        dim,
    )));
    // The legend is the tab's LAST row. This tab had none at all until the
    // message row happened to be absent, and the message row is not a legend.
    out.push(legend_line_row);
    out
}

pub fn render_mcp(p: &McpTabProps, area: Rect, buf: &mut Buffer) {
    paint_rows(&mcp_lines(p), area, buf);
}

// ---------------------------------------------------------------------------
// Tests — ported from src/tui/components/Panel.test.ts (the MCP cases)
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    use bough_core::mcp::config::Registry;
    use bough_core::mcp::manager::{ConnStatus, McpConnState};
    use bough_core::mcp::status::AuthFlag;
    use std::collections::BTreeMap;

    pub fn remote(url: &str) -> ServerConfig {
        ServerConfig {
            url: Some(url.into()),
            ..Default::default()
        }
    }

    pub fn stdio(command: &str) -> ServerConfig {
        ServerConfig {
            command: Some(command.into()),
            ..Default::default()
        }
    }

    pub fn conn(server: &str, alive: bool, tools: usize) -> ConnStatus {
        ConnStatus {
            server: server.into(),
            session_id: String::new(),
            state: if alive {
                McpConnState::Connected
            } else {
                McpConnState::Failed
            },
            alive,
            tool_count: tools,
            tools: Vec::new(),
            last_used: 0,
            error: None,
            stderr_tail: None,
        }
    }

    pub fn status(
        servers: &[(&str, ServerConfig)],
        active: &[&str],
        auth: &[(&str, bool)],
        connections: Vec<ConnStatus>,
    ) -> McpStatus {
        McpStatus {
            registry: Registry {
                servers: servers
                    .iter()
                    .map(|(n, c)| (n.to_string(), c.clone()))
                    .collect(),
            },
            auth: auth
                .iter()
                .map(|(n, ok)| (n.to_string(), AuthFlag { authorized: *ok }))
                .collect::<BTreeMap<_, _>>(),
            active: active.iter().map(|s| s.to_string()).collect(),
            connections,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    fn text(p: &McpTabProps) -> String {
        mcp_lines(p)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_mcp_tab_reports_granted_connected_and_unauthorized_distinctly() {
        let s = status(
            &[("alpha", stdio("alpha-server"))],
            &["alpha"],
            &[("alpha", false)],
            vec![],
        );
        let out = text(&McpTabProps {
            status: Some(&s),
            ..Default::default()
        });
        assert!(out.contains("alpha"), "{out}");
        assert!(out.contains("granted"), "{out}");
        assert!(out.contains("needs auth"), "{out}");
    }

    #[test]
    fn a_server_carrying_its_own_credential_is_not_told_to_authorize() {
        // `sync-mcp` writes an Authorization header referencing the grant Claude
        // Code already holds, and this row said "needs auth" anyway — sending
        // the user to authorize the one server that needed nothing, where
        // authorizing then fails because the provider does not support dynamic
        // registration.
        let mut slack = remote("https://mcp.slack.com/mcp");
        slack.headers.insert(
            "Authorization".into(),
            "Bearer ${keychain:Claude Code-credentials#mcpOAuth.s|1.accessToken}".into(),
        );
        let s = status(&[("slack", slack)], &["slack"], &[("slack", false)], vec![]);
        let out = text(&McpTabProps {
            status: Some(&s),
            cols: 200,
            ..Default::default()
        });
        assert!(out.contains("keychain"), "{out}");
        assert!(!out.contains("needs auth"), "{out}");
        // Deleting a registration is reachable and advertised. `F` next door
        // drops CREDENTIALS and keeps the entry.
        assert!(out.contains("d delete"), "{out}");
        // And proof: `c` connects now and reports, so "keychain" (which
        // credential will be TRIED) can be turned into an answer without
        // spending a turn on a tool call.
        assert!(out.contains("c test"), "{out}");
        assert!(out.contains("F forget"), "{out}");
    }

    #[test]
    fn the_mcp_tab_says_what_its_status_glyphs_mean_for_the_ones_on_screen() {
        let s = status(
            &[
                ("live", remote("https://a.example/mcp")),
                ("granted", remote("https://b.example/mcp")),
            ],
            &["live", "granted"],
            &[],
            vec![conn("live", true, 2)],
        );
        let names = vec!["live".to_string(), "granted".to_string()];
        assert_eq!(
            status_legend(&names, &s),
            vec!["● connected", "◐ granted, not connected — c connects"]
        );
        // Present-only: a list where everything is granted should not spend
        // columns on `○`.
        assert!(!status_legend(&names, &s).iter().any(|l| l.contains('○')));
        let out = text(&McpTabProps {
            status: Some(&s),
            cols: 200,
            ..Default::default()
        });
        assert!(out.contains("granted, not connected"), "{out}");
    }

    #[test]
    fn a_connected_server_reports_its_live_tool_count() {
        let s = status(
            &[("alpha", stdio("alpha-server"))],
            &["alpha"],
            &[],
            vec![conn("alpha", true, 7)],
        );
        assert!(
            mcp_detail(&s, "alpha").contains("7 tools"),
            "{}",
            mcp_detail(&s, "alpha")
        );
        // A registered-but-not-granted server says `off`, and no tool count.
        let s = status(&[("alpha", stdio("alpha-server"))], &[], &[], vec![]);
        assert!(
            mcp_detail(&s, "alpha").starts_with("off"),
            "{}",
            mcp_detail(&s, "alpha")
        );
    }

    #[test]
    fn an_empty_registry_says_what_to_press_and_names_no_inert_key() {
        let s = status(&[], &[], &[], vec![]);
        let out = text(&McpTabProps {
            status: Some(&s),
            ..Default::default()
        });
        assert!(
            out.contains("no MCP servers configured — n adds one by URL"),
            "{out}"
        );
        assert!(out.contains("n add a server by URL"), "{out}");
        // Seven of the nine verbs act on a row that does not exist.
        assert!(!out.contains("⏎ grant/revoke"), "{out}");
    }

    #[test]
    fn the_url_buffer_replaces_the_lists_legend_while_it_is_open() {
        let s = status(&[("alpha", stdio("alpha-server"))], &[], &[], vec![]);
        let out = text(&McpTabProps {
            status: Some(&s),
            entry: Some("https://mcp.example/mcp"),
            ..Default::default()
        });
        assert!(out.contains("new server https://mcp.example/mcp▌"), "{out}");
        assert!(out.contains("⏎ registers · ⌫ back · esc cancels"), "{out}");
    }

    #[test]
    fn an_authorization_url_is_never_clipped_into_something_unopenable() {
        let s = status(&[("alpha", stdio("alpha-server"))], &[], &[], vec![]);
        let url = format!("open https://auth.example/authorize?{}", "x".repeat(120));
        let out = text(&McpTabProps {
            status: Some(&s),
            message: Some(&url),
            ..Default::default()
        });
        assert!(out.contains(&url), "the URL was clipped:\n{out}");
    }

    #[test]
    fn the_window_reserves_both_legends_and_the_list_never_outgrows_it() {
        // Two legend rows, always; the counter is the third thing to go.
        let (_, _, height, counter) = mcp_window(40, 0, 12, 0);
        assert_eq!(height, 9);
        assert!(counter);
        let (_, _, height, counter) = mcp_window(2, 0, 12, 0);
        assert_eq!(height, 10);
        assert!(!counter);
        // No room at all is an honest zero, not a floor.
        let (_, _, height, _) = mcp_window(40, 0, 2, 0);
        assert_eq!(height, 0);
    }

    #[test]
    fn the_tab_never_paints_more_rows_than_its_budget() {
        let servers: Vec<(String, ServerConfig)> =
            (0..30).map(|i| (format!("s{i:02}"), stdio("x"))).collect();
        let refs: Vec<(&str, ServerConfig)> = servers
            .iter()
            .map(|(n, c)| (n.as_str(), c.clone()))
            .collect();
        let s = status(&refs, &[], &[], vec![]);
        for rows in [1usize, 2, 3, 4, 6, 8, 12, 20] {
            let painted = mcp_lines(&McpTabProps {
                status: Some(&s),
                selected: 15,
                rows,
                message: Some("something happened"),
                ..Default::default()
            });
            assert!(
                painted.len() <= rows.max(3),
                "@{rows}: painted {} rows",
                painted.len()
            );
        }
    }
}
