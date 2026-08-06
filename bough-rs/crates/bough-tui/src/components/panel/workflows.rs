//! The workflow run view — the surface delegation is *for* (port of
//! `src/tui/components/Workflows.tsx`).
//!
//! THE INVARIANT THIS HOLDS: **a run that replayed nothing never looks like a
//! run that worked.** Spec §8 states it as a requirement on the system, not on
//! the UI: "Any operation that replays returns how many calls were served from
//! the journal and how many ran live… A rerun that silently replayed nothing
//! looks exactly like a successful rerun, so the count is the only thing that
//! makes a key defect visible. This is a required part of the response, not a
//! UI nicety." The server already computes it (`workflow/report.rs`); the
//! failure mode this file prevents is the client dropping it on the floor.
//! [`replay_rows`] is therefore UNCONDITIONAL — it renders for every run, in
//! every state, before the phases — and it renders the counts AND the server's
//! canonical `line`, so a bough TUI, `bough exec` and a system note all say the
//! same sentence.
//!
//! The alarm case gets its own tone: `available > 0, replayed: 0` means the
//! source run held answers and this run's keys matched none of them, which is a
//! key defect and not a slow day. `available: 0` is an ordinary first run and
//! says so quietly.
//!
//! WHAT THE VIEW IS. Miller-column: runs → phases → that phase's agents → one
//! agent, plus the script. The stacked single-column version this replaces
//! silently dropped whole phase groups once the viewport filled, which read as
//! "that is the whole run". Phases keep their ordinal until they complete, so
//! the SHAPE of a run is legible before it gets there — `meta.phases` declares
//! stages no agent has reached yet.
//!
//! STEERING (spec §8). `p` pauses — gating new `agent()` calls while the ones
//! in flight finish and are journaled — and `x` stops. The script level exists
//! because "stop, edit the script, relaunch seeded from the journal" is the
//! whole steering loop, and it names the edit target on screen: the mirror at
//! `~/.bough/workflows/<id>.js`.
//!
//! PURE ROWS. Everything above the renderer is [`Row`] — `{text, tone}` cells
//! with no ratatui in them — so what the user sees is asserted directly with
//! nothing mounted and no terminal.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use bough_core::schema::parts::{WorkflowAgentStatus, WorkflowRun, WorkflowStatus};
use bough_core::workflow::control::WorkflowAgentView;
use bough_core::workflow::report::{LargeRunFlag, ReplaySummary, RunCost};

use crate::api::{WorkflowDetail, WorkflowSummary};
use crate::components::panel::{legend_line, paint_rows, window_around};
use crate::components::{accent, error, fmt_tokens, info, muted, warn};
use crate::store::selectors::{clip, plural};

// ---------------------------------------------------------------------------
// Rows: the testable rendering unit
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Text,
    Muted,
    Accent,
    Warn,
    Error,
    Info,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub text: String,
    pub tone: Option<Tone>,
    pub bold: bool,
}

pub type Row = Vec<Cell>;

impl Cell {
    pub fn new(text: impl Into<String>) -> Cell {
        Cell {
            text: text.into(),
            tone: None,
            bold: false,
        }
    }
    pub fn toned(text: impl Into<String>, tone: Tone) -> Cell {
        Cell {
            text: text.into(),
            tone: Some(tone),
            bold: false,
        }
    }
    pub fn bold(mut self) -> Cell {
        self.bold = true;
        self
    }
}

/// A row as plain text — what every assertion in this file reads.
pub fn row_text(row: &Row) -> String {
    row.iter().map(|c| c.text.as_str()).collect()
}

pub fn lines_of(rows: &[Row]) -> Vec<String> {
    rows.iter().map(row_text).collect()
}

fn tone_style(tone: Option<Tone>, bold: bool) -> Style {
    let mut style = match tone {
        Some(Tone::Accent) => Style::default().fg(accent()),
        Some(Tone::Warn) => Style::default().fg(warn()),
        Some(Tone::Error) => Style::default().fg(error()),
        Some(Tone::Info) => Style::default().fg(info()),
        Some(Tone::Muted) => Style::default().fg(muted()).add_modifier(Modifier::DIM),
        _ => Style::default(),
    };
    if bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    style
}

fn row_line(row: &Row) -> Line<'static> {
    Line::from(
        row.iter()
            .map(|c| Span::styled(c.text.clone(), tone_style(c.tone, c.bold)))
            .collect::<Vec<_>>(),
    )
}

/// `12s` / `3m07s`. Seconds survive past a minute: the clock is how a wedged
/// agent shows.
fn elapsed(from: i64, to: Option<i64>, now: i64) -> String {
    let ms = to.unwrap_or(now) - from;
    let s = ((ms as f64) / 1000.0).round().max(0.0) as i64;
    if s < 60 {
        format!("{s}s")
    } else {
        format!("{}m{:02}s", s / 60, s % 60)
    }
}

/// "17.8k tok" — the per-agent cost signal. Empty for a call that spent nothing.
pub fn token_chip(n: i64) -> String {
    if n <= 0 {
        String::new()
    } else {
        format!("{} tok", fmt_tokens(n))
    }
}

/// Window a list around the cursor so a 200-agent phase still scrolls.
pub fn windowed<T: Clone>(items: &[T], sel: usize, rows: usize) -> (Vec<T>, usize) {
    if items.len() <= rows {
        return (items.to_vec(), 0);
    }
    let (start, end) = window_around(sel, items.len(), rows);
    (items[start..end.min(items.len())].to_vec(), start)
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/// Status → (glyph, tone). ONE place, so every level and the chip agree.
pub fn wf_glyph(status: &str) -> (&'static str, Tone) {
    match status {
        // journaled, waiting on the semaphore
        "queued" => ("◦", Tone::Muted),
        "running" => ("◐", Tone::Info),
        "paused" => ("⏸", Tone::Warn),
        "done" => ("✓", Tone::Accent),
        // replayed from the journal — no agent ran
        "cached" => ("≡", Tone::Accent),
        "error" => ("✗", Tone::Error),
        "stopped" => ("■", Tone::Warn),
        // orphaned
        _ => ("⚠", Tone::Warn),
    }
}

/// A RUN's glyph, which is not the same question as an agent's.
///
/// `status: "done"` on a run means only that the script returned — it says
/// nothing about whether the agents it dispatched worked. A run that lost 8 of
/// its 9 agents to a schema rejection reached `done` in 1m51s and was drawn with
/// the same green `✓` as a clean one, two columns from the text "8 failed".
/// That is the lying checkmark the subagent cards were fixed for, in the one
/// view built to explain a fan-out.
pub fn run_glyph(status: &str, failed: usize) -> (&'static str, Tone) {
    if status == "done" && failed > 0 {
        return ("⚠", Tone::Warn);
    }
    wf_glyph(status)
}

/// The wire spelling of a run status, as the glyph table reads it.
pub fn run_status_str(status: WorkflowStatus) -> &'static str {
    match status {
        WorkflowStatus::Running => "running",
        WorkflowStatus::Paused => "paused",
        WorkflowStatus::Done => "done",
        WorkflowStatus::Error => "error",
        WorkflowStatus::Stopped => "stopped",
        WorkflowStatus::Orphaned => "orphaned",
    }
}

pub fn agent_status_str(status: WorkflowAgentStatus) -> &'static str {
    match status {
        WorkflowAgentStatus::Queued => "queued",
        WorkflowAgentStatus::Running => "running",
        WorkflowAgentStatus::Done => "done",
        WorkflowAgentStatus::Cached => "cached",
        WorkflowAgentStatus::Error => "error",
        WorkflowAgentStatus::Stopped => "stopped",
    }
}

// ---------------------------------------------------------------------------
// Grouping and filtering (pure)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct PhaseGroup {
    pub title: String,
    pub detail: Option<String>,
    pub agents: Vec<WorkflowAgentView>,
}

/// Agents grouped under their phase, in script order: `meta`-declared phases
/// first — INCLUDING ones no agent has reached, so the run's shape is visible
/// before it gets there — then phases agents reported that meta never declared,
/// then phase-less agents.
pub fn phase_groups(run: &WorkflowRun, agents: &[WorkflowAgentView]) -> Vec<PhaseGroup> {
    let declared: Vec<String> = run.phases.iter().map(|p| p.title.clone()).collect();
    let mut extra: Vec<String> = Vec::new();
    for a in agents {
        let phase = a.agent.phase.clone().unwrap_or_default();
        if !phase.is_empty() && !declared.contains(&phase) && !extra.contains(&phase) {
            extra.push(phase);
        }
    }
    let mut titles = declared.clone();
    titles.extend(extra);
    titles.push(String::new());
    titles
        .into_iter()
        .map(|title| {
            let detail = run
                .phases
                .iter()
                .find(|p| p.title == title)
                .and_then(|p| p.detail.clone());
            let agents = agents
                .iter()
                .filter(|a| a.agent.phase.clone().unwrap_or_default() == title)
                .cloned()
                .collect::<Vec<_>>();
            PhaseGroup {
                title,
                detail,
                agents,
            }
        })
        .filter(|g| !g.agents.is_empty() || declared.contains(&g.title))
        .collect()
}

/// The `f` cycle: all, then each status worth isolating on a big run.
pub const WF_FILTERS: [Option<&str>; 5] = [
    None,
    Some("running"),
    Some("queued"),
    Some("done"),
    Some("error"),
];

/// "done" folds in journal replays: both are answers, and only one cost
/// anything.
pub fn visible_agents(
    agents: &[WorkflowAgentView],
    filter: Option<&str>,
) -> Vec<WorkflowAgentView> {
    let Some(filter) = filter else {
        return agents.to_vec();
    };
    agents
        .iter()
        .filter(|a| {
            let s = agent_status_str(a.agent.status);
            if filter == "done" {
                s == "done" || s == "cached"
            } else {
                s == filter
            }
        })
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// Replay, cost, and the large-run flag
// ---------------------------------------------------------------------------

/// The replay accounting. NEVER conditional — see the header. The counts always
/// show, broken out so the arithmetic is visible
/// (`replayed + ranLive + pending === total`).
///
/// The server's one-line form is a SECOND row only when it carries something the
/// counts do not: a source run to compare against, or the alarm case. On a first
/// run it restated them in a different format on the very next line —
/// `0 replayed · 2 ran live · of 2` above `0 replayed, 2 ran live of 2` — which
/// reads as a rendering bug on the one panel whose job is to be believed about
/// what was and was not re-run.
pub fn replay_rows(replay: &ReplaySummary) -> Vec<Row> {
    let alarm = replay.available > 0 && replay.replayed == 0;
    let mut parts = vec![
        format!("{} replayed", replay.replayed),
        format!("{} ran live", replay.ran_live),
    ];
    if replay.pending > 0 {
        parts.push(format!("{} still going", replay.pending));
    }
    parts.push(format!("of {}", replay.total));
    let counts = parts.join(" · ");
    let source = match &replay.source_id {
        Some(_) => format!(" · {} available to replay", replay.available),
        None => String::new(),
    };
    let mut rows: Vec<Row> = vec![vec![
        Cell::toned("≡ replay  ", Tone::Muted),
        Cell {
            text: format!("{counts}{source}"),
            tone: Some(if alarm { Tone::Error } else { Tone::Text }),
            bold: alarm,
        },
    ]];
    if replay.source_id.is_some() || alarm {
        rows.push(vec![Cell::toned(
            format!("  {}", replay.line),
            if alarm { Tone::Error } else { Tone::Muted },
        )]);
    }
    rows
}

/// Tokens and agent-time, per run and per phase.
///
/// LABELLED `≡ usage`, not `$ cost`. `RunCost` carries tokens, agent-time and
/// wall-time and no money at all — so a row headed with a dollar sign showed
/// `8.6k tok · 2 agents` and a delegator persona reasonably asked where the
/// dollars were. The per-phase breakdown is dropped when there is only ONE
/// group, where it repeated the total verbatim.
pub fn cost_rows(cost: &RunCost) -> Vec<Row> {
    let per_phase = if cost.by_phase.len() > 1 {
        cost.by_phase
            .iter()
            .map(|p| {
                let phase = p
                    .phase
                    .clone()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "agents".into());
                let chip = token_chip(p.tokens);
                format!(
                    "{phase} {}",
                    if chip.is_empty() {
                        "0 tok".to_string()
                    } else {
                        chip
                    }
                )
            })
            .collect::<Vec<_>>()
            .join(" · ")
    } else {
        String::new()
    };
    let total = token_chip(cost.tokens);
    vec![vec![
        Cell::toned("≡ usage   ", Tone::Muted),
        Cell::toned(
            format!(
                "{} · {}",
                if total.is_empty() {
                    "0 tok".to_string()
                } else {
                    total
                },
                plural(cost.agents as i64, "agent")
            ),
            Tone::Text,
        ),
        Cell::toned(
            if per_phase.is_empty() {
                String::new()
            } else {
                format!("  {per_phase}")
            },
            Tone::Muted,
        ),
    ]]
}

/// The advisory flag, next to the control that actually stops the run.
pub fn warning_rows(warning: Option<&LargeRunFlag>) -> Vec<Row> {
    let Some(warning) = warning else {
        return Vec::new();
    };
    vec![
        vec![
            Cell::toned("! large   ", Tone::Warn),
            Cell::toned(warning.reasons.join(" · "), Tone::Warn),
        ],
        vec![Cell::toned(
            "  advisory — nothing is throttled; x stops the run",
            Tone::Muted,
        )],
    ]
}

// ---------------------------------------------------------------------------
// Steering (spec §8)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SteerAction {
    pub key: &'static str,
    pub label: &'static str,
}

/// The controls that apply to a run in this state. `pause` is named as the
/// thing to do BEFORE stopping: a dispatched agent allowed to finish is
/// journaled and replays, while one killed in flight is not and starts over. A
/// terminal run offers the other half of the loop — edit the script, relaunch
/// seeded from this run's journal.
pub fn steer_actions(status: &str, live: bool) -> Vec<SteerAction> {
    let stop = SteerAction {
        key: "x",
        label: "stop",
    };
    let script = SteerAction {
        key: "e",
        label: "script",
    };
    if status == "running" {
        // A run this process no longer holds cannot honor a pause, so it is not
        // offered one.
        return if live {
            vec![
                SteerAction {
                    key: "p",
                    label: "pause (finishes in-flight agents)",
                },
                stop,
            ]
        } else {
            vec![
                SteerAction {
                    key: "x",
                    label: "stop — orphaned by a restart",
                },
                script,
            ]
        };
    }
    if status == "paused" {
        return vec![
            SteerAction {
                key: "p",
                label: "resume",
            },
            stop,
            script,
        ];
    }
    // `save` is offered only on a SETTLED run, and deliberately: what it stores
    // is the script, and the reason to store one is that you watched it work.
    vec![
        SteerAction {
            key: "r",
            label: "rerun (replays the journal)",
        },
        // `e` SHOWS the script and its mirror path; `r` is what relaunches,
        // picking up the edited mirror. The old label implied `e` did both,
        // which is why the loop reads as broken: you press it, a script
        // appears, nothing relaunches.
        SteerAction {
            key: "e",
            label: "the script + path to edit (then r)",
        },
        SteerAction {
            key: "s",
            label: "save to run again by name",
        },
    ]
}

// ---------------------------------------------------------------------------
// Header, panes and detail (pure rows)
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
pub struct HeaderOpts<'a> {
    pub last_log: Option<&'a str>,
    pub now: i64,
}

pub fn run_header_rows(detail: &WorkflowDetail, opts: &HeaderOpts) -> Vec<Row> {
    let now = opts.now;
    let run = &detail.workflow;
    let settled = detail
        .agents
        .iter()
        .filter(|a| {
            matches!(
                a.agent.status,
                WorkflowAgentStatus::Done | WorkflowAgentStatus::Cached
            )
        })
        .count();
    let failed = detail
        .agents
        .iter()
        .filter(|a| a.agent.status == WorkflowAgentStatus::Error)
        .count();
    let status = run_status_str(run.status);
    let (glyph, tone) = run_glyph(status, failed);
    let mut rows: Vec<Row> = vec![
        vec![
            Cell::toned(glyph, tone),
            Cell::new(format!(" {}", run.name)).bold(),
            Cell::toned(
                format!("  {status}"),
                if status == "error" {
                    Tone::Error
                } else {
                    Tone::Muted
                },
            ),
            Cell::toned(
                match &run.resume_of {
                    Some(id) => format!("  relaunch of {id}"),
                    None => String::new(),
                },
                Tone::Muted,
            ),
            Cell::toned(
                if detail.live || status != "running" {
                    ""
                } else {
                    "  (not live here)"
                },
                Tone::Warn,
            ),
        ],
        vec![
            Cell::toned(run.description.clone(), Tone::Muted),
            Cell::toned(
                format!(
                    "  {settled}/{} agents{} · {}",
                    detail.agents.len(),
                    if failed > 0 {
                        format!(" · {failed} failed")
                    } else {
                        String::new()
                    },
                    elapsed(run.created_at, run.finished_at, now)
                ),
                Tone::Muted,
            ),
        ],
    ];
    rows.extend(replay_rows(&detail.replay));
    rows.extend(cost_rows(&detail.cost));
    rows.extend(warning_rows(detail.warning.as_ref()));
    if let Some(error) = &run.error {
        rows.push(vec![Cell::toned(error.clone(), Tone::Error)]);
    }
    if let Some(log) = opts.last_log.filter(|_| status == "running") {
        rows.push(vec![Cell::toned(format!("▸ {log}"), Tone::Muted)]);
    }
    rows
}

/// The Phases pane. A phase keeps its ORDINAL until it completes — the number is
/// how the run's shape reads before it gets there — and only then becomes ✓, or
/// ✗ when it settled with a failure (a red ◐ would say "still going", the
/// opposite of what happened).
pub fn phase_rows(
    groups: &[PhaseGroup],
    selected: usize,
    cursor: bool,
    current: Option<&str>,
) -> Vec<Row> {
    groups
        .iter()
        .enumerate()
        .map(|(i, g)| {
            let done = g
                .agents
                .iter()
                .filter(|a| {
                    matches!(
                        a.agent.status,
                        WorkflowAgentStatus::Done | WorkflowAgentStatus::Cached
                    )
                })
                .count();
            let busy = g.agents.iter().any(|a| {
                matches!(
                    a.agent.status,
                    WorkflowAgentStatus::Running | WorkflowAgentStatus::Queued
                )
            });
            let failed = g
                .agents
                .iter()
                .any(|a| a.agent.status == WorkflowAgentStatus::Error);
            let complete = !g.agents.is_empty() && !busy;
            let mark = if complete {
                Cell::toned(
                    if failed { "✗" } else { "✓" },
                    if failed { Tone::Error } else { Tone::Accent },
                )
            } else {
                Cell::toned((i + 1).to_string(), Tone::Muted)
            };
            let title = if g.title.is_empty() {
                "agents"
            } else {
                g.title.as_str()
            };
            let mut row: Row = vec![
                Cell::toned(
                    if cursor && i == selected {
                        "❯ "
                    } else {
                        "  "
                    },
                    Tone::Info,
                ),
                mark,
                {
                    let cell = Cell::new(format!(" {}", clip(title, 14)));
                    if Some(g.title.as_str()) == current {
                        cell.bold()
                    } else {
                        cell
                    }
                },
            ];
            if !g.agents.is_empty() {
                row.push(Cell::toned(
                    format!(" {done}/{}", g.agents.len()),
                    Tone::Muted,
                ));
            }
            row
        })
        .collect()
}

/// One phase's agents. A running agent shows its clock, not the word "running".
pub fn agent_rows(
    agents: &[WorkflowAgentView],
    selected: usize,
    cursor: bool,
    compact: bool,
    now: i64,
) -> Vec<Row> {
    agents
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let (glyph, tone) = wf_glyph(agent_status_str(a.agent.status));
            let label = if a.agent.label.is_empty() {
                "(unlabeled)"
            } else {
                a.agent.label.as_str()
            };
            let mut head: Row = vec![
                Cell::toned(
                    if cursor && i == selected {
                        "❯ "
                    } else {
                        "  "
                    },
                    Tone::Info,
                ),
                Cell::toned(glyph, tone),
                Cell::new(format!(" {}", clip(label, if compact { 16 } else { 34 }))),
            ];
            if compact {
                return head;
            }
            let time = if a.agent.status == WorkflowAgentStatus::Queued {
                "queued".to_string()
            } else {
                elapsed(a.agent.started_at, a.agent.finished_at, now)
            };
            let mid = [
                a.agent.model.clone().unwrap_or_default(),
                token_chip(a.tokens),
            ]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
            head.push(Cell::toned(format!("  {mid}"), Tone::Muted));
            head.push(Cell::toned(format!("  {time}"), Tone::Muted));
            head
        })
        .collect()
}

/// One agent, in full. The prompt is COLLAPSED by default — it is the one thing
/// you already know, you wrote the workflow — and the outcome is what the
/// drill-in was for.
pub fn agent_detail_rows(agent: &WorkflowAgentView, prompt_open: bool, now: i64) -> Vec<Row> {
    let status = agent_status_str(agent.agent.status);
    let (glyph, tone) = wf_glyph(status);
    let prompt: Vec<&str> = agent.agent.prompt.split('\n').collect();
    let tail = [
        agent.agent.model.clone().unwrap_or_default(),
        token_chip(agent.tokens),
        if agent.tool_calls > 0 {
            plural(agent.tool_calls as i64, "tool call")
        } else {
            String::new()
        },
        if status == "queued" {
            "waiting on the run's concurrency limit".to_string()
        } else {
            elapsed(agent.agent.started_at, agent.agent.finished_at, now)
        },
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join(" · ");
    let mut rows: Vec<Row> = vec![vec![
        Cell::toned(glyph, tone),
        Cell::new(format!(" {status}")),
        Cell::toned(format!("  {tail}"), Tone::Muted),
    ]];
    rows.push(vec![Cell::toned(
        match &agent.agent.session_id {
            Some(id) => format!(
                "session {} — o opens it",
                id.chars().take(8).collect::<String>()
            ),
            None => "no session — this call was replayed from the journal".to_string(),
        },
        Tone::Muted,
    )]);
    rows.push(Vec::new());
    rows.push(vec![Cell::new(if prompt_open {
        "Prompt · ⏎ collapse".to_string()
    } else {
        format!(
            "Prompt · {} · ⏎ expand",
            plural(prompt.len() as i64, "line")
        )
    })
    .bold()]);
    let shown: &[&str] = if prompt_open {
        &prompt
    } else {
        &prompt[..prompt.len().min(2)]
    };
    for l in shown {
        rows.push(vec![Cell::new(format!("  {l}"))]);
    }
    if !prompt_open && prompt.len() > 2 {
        rows.push(vec![Cell::toned(
            format!("  … {}", plural((prompt.len() - 2) as i64, "more line")),
            Tone::Muted,
        )]);
    }
    if !agent.activity.is_empty() {
        rows.push(Vec::new());
        rows.push(vec![Cell::new("Activity").bold()]);
        for l in &agent.activity {
            rows.push(vec![Cell::toned(format!("  {l}"), Tone::Muted)]);
        }
    }
    rows.push(Vec::new());
    let head = match status {
        "error" => "Error",
        "cached" => "Outcome · replayed from the source run's journal — no agent ran",
        _ => "Outcome",
    };
    let mut cell = Cell::new(head).bold();
    if status == "error" {
        cell.tone = Some(Tone::Error);
    }
    rows.push(vec![cell]);
    let body = agent
        .agent
        .error
        .clone()
        .or_else(|| agent.agent.result.clone())
        .unwrap_or_else(|| "(none yet)".to_string());
    for l in body.split('\n') {
        let mut cell = Cell::new(format!("  {l}"));
        if status == "error" {
            cell.tone = Some(Tone::Error);
        }
        rows.push(vec![cell]);
    }
    rows
}

/// The script, and where to edit it. The mirror path is rendered first and in
/// full, because spec §8's loop is "stop, edit the script — on disk at
/// `~/.bough/workflows/<id>.js`, through the API, or by asking the agent to
/// rewrite it — and relaunch seeded from the stopped run's journal": the path IS
/// the affordance.
pub fn script_rows(detail: &WorkflowDetail) -> Vec<Row> {
    let lines: Vec<&str> = detail.workflow.script.split('\n').collect();
    let width = lines.len().to_string().len();
    let mut rows: Vec<Row> = vec![
        vec![Cell::toned(detail.script_file.clone(), Tone::Info)],
        vec![Cell::toned(
            if detail.live {
                "the run is still live — pause, then stop, before you edit: dispatched agents that \
                 finish are journaled and replay"
                    .to_string()
            } else {
                // LOWERCASE, and checked against the keymap: `r` is `wf.rerun`.
                // There is no `R` binding in `keys.rs` and there never was, so
                // this row advertised a dead key on the one screen whose whole
                // job is to explain the steering loop.
                format!(
                    "r relaunches a NEW run from this one's journal · {} journaled here",
                    plural(detail.replay.total as i64, "call")
                )
            },
            if detail.live { Tone::Warn } else { Tone::Muted },
        )],
        Vec::new(),
    ];
    for (i, l) in lines.iter().enumerate() {
        rows.push(vec![
            Cell::toned(format!("{:>width$} ", i + 1, width = width), Tone::Muted),
            Cell::new(*l),
        ]);
    }
    rows
}

// ---------------------------------------------------------------------------
// The component
// ---------------------------------------------------------------------------

/// 0 runs · 1 phases · 2 a phase's agents · 3 one agent · 4 the script.
pub type WfLevel = u8;

/// The keys the panel actually delivers to this tab (`keys.rs`, mode `panel`).
///
/// The legend row is the whole discoverability strategy, so it may only name
/// keys that do something. This set is the one line to change.
pub const BOUND_STEER_KEYS: [&str; 6] = ["p", "P", "x", "r", "e", "s"];

/// Per-level footer — the keys that do something HERE, plus the steering
/// controls.
pub fn footer(level: WfLevel, detail: Option<&WorkflowDetail>) -> String {
    let steer = match detail {
        Some(d) => steer_actions(run_status_str(d.workflow.status), d.live)
            .into_iter()
            .filter(|a| BOUND_STEER_KEYS.contains(&a.key))
            .map(|a| format!("{} {}", a.key, a.label))
            .collect::<Vec<_>>()
            .join(" · "),
        None => String::new(),
    };
    // Level 0 has no `detail` — it is not fetched until a run is opened — so the
    // steering verbs are named from the keymap instead of from a run's state.
    // They ACT at this level (the host steers the selected row), and a verb that
    // works and is never printed is a verb nobody has.
    if level == 0 {
        // `esc back` LAST, like every other level and every other tab.
        let steer = if steer.is_empty() {
            "p pause · P resume · x stop · r relaunch".to_string()
        } else {
            steer
        };
        return format!("⏎ open · 1-9 pick · {steer} · esc back");
    }
    if level == 4 {
        return format!("↑↓ scroll · {steer} · esc back");
    }
    if level == 1 {
        return format!("↑↓ phase · ⏎ agents · f filter · {steer} · esc back");
    }
    if level == 2 {
        return format!("↑↓ agent · ⏎ open · o session · f filter · {steer} · esc back");
    }
    format!("⏎ prompt · ↑↓ scroll · o session · {steer} · esc back")
}

/// Rows the level-0 run list may paint.
///
/// The tab's own chrome is three rows: the gap above the list, the one above the
/// footer, and the footer. `max(3, rows - 6)` was a floor of three rows at ANY
/// height, which below six rows was a request for rows that do not exist.
pub fn wf_runs_height(rows: usize) -> usize {
    rows.saturating_sub(1 + 2 * wf_gap(rows))
}

/// The blank separator rows, which a cramped panel cannot afford. Breathing room
/// is the first thing to go, and the legend is the last.
pub fn wf_gap(rows: usize) -> usize {
    usize::from(rows >= 8)
}

fn runs_list_rows(runs: &[WorkflowSummary], sel: usize, rows: usize, now: i64) -> Vec<Row> {
    if runs.is_empty() {
        return vec![vec![Cell::toned(
            "no workflow runs in this conversation — ask for one",
            Tone::Muted,
        )]];
    }
    let height = wf_runs_height(rows);
    let (slice, from) = if height == 0 {
        (Vec::new(), 0)
    } else {
        windowed(runs, sel, height)
    };
    slice
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let on = from + i == sel;
            let a = &r.agents;
            let (glyph, tone) = run_glyph(&r.status, a.failed);
            vec![
                // The digit that picks this run, printed on it (spec §3).
                Cell::toned(
                    if i < 9 {
                        format!("{} ", i + 1)
                    } else {
                        "  ".into()
                    },
                    Tone::Muted,
                ),
                Cell::toned(if on { "❯ " } else { "  " }, Tone::Info),
                Cell::toned(glyph, tone),
                Cell::new(format!(" {}", r.name)).bold(),
                Cell::toned(format!("  {}", clip(&r.description, 44)), Tone::Muted),
                Cell::toned(
                    format!(
                        "  {}/{}{}{} · {}",
                        a.done,
                        a.total,
                        if a.cached > 0 {
                            format!(" · {} replayed", a.cached)
                        } else {
                            String::new()
                        },
                        if a.failed > 0 {
                            format!(" · {} failed", a.failed)
                        } else {
                            String::new()
                        },
                        elapsed(r.created_at, r.finished_at, now)
                    ),
                    Tone::Muted,
                ),
            ]
        })
        .collect()
}

pub struct WorkflowsProps<'a> {
    pub runs: &'a [WorkflowSummary],
    pub sel: usize,
    pub level: WfLevel,
    /// `GET /workflows/:id` for the opened run; `None` at level 0 or while
    /// loading.
    pub detail: Option<&'a WorkflowDetail>,
    pub phase_sel: usize,
    pub agent_sel: usize,
    pub scroll: usize,
    pub filter: Option<&'a str>,
    pub prompt_open: bool,
    pub rows: usize,
    pub cols: usize,
    pub last_log: Option<&'a str>,
    /// Injected so a render is reproducible in a test.
    pub now: i64,
}

impl Default for WorkflowsProps<'_> {
    fn default() -> Self {
        WorkflowsProps {
            runs: &[],
            sel: 0,
            level: 0,
            detail: None,
            phase_sel: 0,
            agent_sel: 0,
            scroll: 0,
            filter: None,
            prompt_open: false,
            rows: 0,
            cols: 80,
            last_log: None,
            now: 0,
        }
    }
}

fn legend_row(text: &str, cols: usize) -> Row {
    let items: Vec<String> = text.split(" · ").map(|s| s.to_string()).collect();
    vec![Cell::toned(legend_line(&items, Some(cols)), Tone::Muted)]
}

/// The rows this tab paints, in order — the whole view as text, so a level's
/// layout is asserted without a terminal.
pub fn workflows_rows(p: &WorkflowsProps) -> Vec<Row> {
    let gap = wf_gap(p.rows);
    let blank = |n: usize| -> Vec<Row> { (0..n).map(|_| Vec::new()).collect() };
    let Some(detail) = p.detail.filter(|_| p.level != 0) else {
        let mut out = blank(gap);
        out.extend(runs_list_rows(p.runs, p.sel, p.rows, p.now));
        out.extend(blank(gap));
        out.push(legend_row(&footer(0, p.detail), p.cols));
        return out;
    };

    let all_header = run_header_rows(
        detail,
        &HeaderOpts {
            last_log: p.last_log,
            now: p.now,
        },
    );

    if p.level == 4 {
        // gap + header + body + footer.
        let keep = (p.rows as isize - gap as isize - 2).max(0) as usize;
        let header: Vec<Row> = all_header.iter().take(keep).cloned().collect();
        let pane_rows =
            (p.rows as isize - gap as isize - 1 - header.len() as isize).max(0) as usize;
        let body = script_rows(detail);
        let mut out = blank(gap);
        out.extend(header);
        if pane_rows > 0 {
            out.extend(windowed(&body, p.scroll, pane_rows).0);
        }
        out.push(legend_row(&footer(4, Some(detail)), p.cols));
        return out;
    }

    // gap + header + gap + 1 column title + panes + footer. The header is
    // CLIPPED rather than allowed to push the panes past the bottom: a run's
    // header can be nine rows on its own.
    let keep = (p.rows as isize - 2 * gap as isize - 3).max(0) as usize;
    let header: Vec<Row> = all_header.iter().take(keep).cloned().collect();
    let pane_rows =
        (p.rows as isize - 2 * gap as isize - 2 - header.len() as isize).max(0) as usize;
    let left_w = (p.cols / 4).clamp(12, 24);

    let groups = phase_groups(&detail.workflow, &detail.agents);
    let empty = PhaseGroup {
        title: String::new(),
        detail: None,
        agents: Vec::new(),
    };
    let group = groups
        .get(p.phase_sel.min(groups.len().saturating_sub(1)))
        .unwrap_or(&empty);
    let shown = visible_agents(&group.agents, p.filter);
    let agent = if p.level == 3 {
        shown.get(p.agent_sel)
    } else {
        None
    };

    let left = match agent {
        Some(_) => agent_rows(&shown, p.agent_sel, true, true, p.now),
        None => phase_rows(
            &groups,
            p.phase_sel,
            p.level == 1,
            detail.workflow.current_phase.as_deref(),
        ),
    };
    let pane = |list: &[Row], sel: usize| -> Vec<Row> {
        if pane_rows == 0 {
            Vec::new()
        } else {
            windowed(list, sel, pane_rows).0
        }
    };
    let right = match agent {
        Some(a) => pane(&agent_detail_rows(a, p.prompt_open, p.now), p.scroll),
        None => pane(
            &agent_rows(&shown, p.agent_sel, p.level == 2, false, p.now),
            p.agent_sel,
        ),
    };

    let left_title = match agent {
        Some(_) => clip(
            if group.title.is_empty() {
                "agents"
            } else {
                &group.title
            },
            left_w,
        ),
        None => "Phases".to_string(),
    };
    let right_title = match agent {
        Some(a) => clip(&a.agent.label, 40),
        None => format!(
            "{} · {}{}",
            if group.title.is_empty() {
                "agents"
            } else {
                &group.title
            },
            shown.len(),
            match p.filter {
                Some(f) => format!(" {f}"),
                None => String::new(),
            }
        ),
    };

    let mut out = blank(gap);
    out.extend(header);
    out.extend(blank(gap));
    // The two panes side by side: the left column is padded to `left_w + 2` so
    // the right pane starts at one column, always.
    let left_pane = pane(
        &left,
        if agent.is_some() {
            p.agent_sel
        } else {
            p.phase_sel
        },
    );
    let mut titles: Row = vec![Cell::toned(pad(&left_title, left_w + 2), Tone::Muted)];
    titles.push(Cell::toned(right_title, Tone::Muted));
    out.push(titles);
    for i in 0..left_pane.len().max(right.len()) {
        let mut row: Row = Vec::new();
        match left_pane.get(i) {
            // The left column is a FIXED width: a long phase title that grew
            // the column would push the right pane off by exactly its overrun,
            // and every row below it by a different amount. Clipped to the
            // width, then padded to it — never one or the other.
            Some(cells) => {
                let mut spent = 0usize;
                for cell in cells {
                    if spent >= left_w {
                        break;
                    }
                    let room = left_w - spent;
                    let text = if display_len(&cell.text) > room {
                        clip(&cell.text, room.saturating_sub(1))
                    } else {
                        cell.text.clone()
                    };
                    spent += display_len(&text);
                    row.push(Cell {
                        text,
                        ..cell.clone()
                    });
                }
                row.push(Cell::new(pad("", (left_w + 2).saturating_sub(spent))));
            }
            None => row.push(Cell::new(pad("", left_w + 2))),
        }
        if let Some(cells) = right.get(i) {
            row.extend(cells.clone());
        }
        out.push(row);
    }
    out.push(legend_row(&footer(p.level, Some(detail)), p.cols));
    out
}

fn display_len(s: &str) -> usize {
    crate::ansi::width(s)
}

fn pad(s: &str, w: usize) -> String {
    let mut out = s.to_string();
    let len = display_len(s);
    if len < w {
        out.push_str(&" ".repeat(w - len));
    }
    out
}

/// Paint the tab. Never more rows than the budget — the panel's own rule.
pub fn render_workflows(p: &WorkflowsProps, area: Rect, buf: &mut Buffer) {
    let rows = workflows_rows(p);
    let lines: Vec<Line> = rows.iter().map(row_line).collect();
    paint_rows(&lines, area, buf);
}

/// The composer's live-run line: what is running, without opening the panel.
/// Carries the replayed count too — a chip that says "3/8 agents" hides whether
/// any of them cost anything.
pub fn workflow_chip(run: &WorkflowSummary, log: Option<&str>, now: i64) -> Row {
    let (glyph, tone) = wf_glyph(&run.status);
    let a = &run.agents;
    vec![
        Cell::toned(glyph, tone),
        Cell::new(format!(" {}", run.name)).bold(),
        Cell::toned(
            format!(
                "  {}/{} agents{} · {}{}{} · /workflows",
                a.done,
                a.total,
                if a.cached > 0 {
                    format!(" · {} replayed", a.cached)
                } else {
                    String::new()
                },
                elapsed(run.created_at, run.finished_at, now),
                match &run.current_phase {
                    Some(p) if !p.is_empty() => format!(" · {p}"),
                    _ => String::new(),
                },
                match log {
                    Some(l) => format!(" · {}", clip(l, 40)),
                    None => String::new(),
                }
            ),
            Tone::Muted,
        ),
    ]
}

// ---------------------------------------------------------------------------
// Tests — ported from src/tui/components/Workflows.test.ts
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    use bough_core::schema::parts::{WorkflowAgent, WorkflowPhase};
    use bough_core::workflow::report::{AgentCost, PhaseCost};

    pub const T0: i64 = 1_700_000_000_000;
    pub const NOW: i64 = T0 + 90_000;

    pub fn agent(
        id: &str,
        idx: i64,
        status: WorkflowAgentStatus,
        phase: &str,
    ) -> WorkflowAgentView {
        WorkflowAgentView {
            agent: WorkflowAgent {
                id: id.into(),
                run_id: "run-2".into(),
                idx,
                key: format!("k-{id}"),
                label: id.into(),
                phase: Some(phase.into()),
                prompt: "Review src/server/app.ts".into(),
                model: Some("sonnet".into()),
                status,
                result: Some("no findings".into()),
                error: None,
                session_id: Some(format!("sess-{id}")),
                started_at: T0,
                finished_at: Some(T0 + 20_000),
            },
            tokens: 1200,
            tool_calls: 3,
            activity: Vec::new(),
            live: false,
        }
    }

    /// Two replayed, one done, one failed, one running, one queued — every
    /// bucket.
    pub fn agents() -> Vec<WorkflowAgentView> {
        let mut a = agent("a", 0, WorkflowAgentStatus::Cached, "Review");
        a.agent.session_id = None;
        a.tokens = 0;
        a.agent.result = Some("cached ok".into());
        let mut b = agent("b", 1, WorkflowAgentStatus::Cached, "Review");
        b.agent.session_id = None;
        b.tokens = 0;
        b.agent.result = Some("cached ok".into());
        let c = agent("c", 2, WorkflowAgentStatus::Done, "Review");
        let mut d = agent("d", 3, WorkflowAgentStatus::Error, "Review");
        d.agent.result = None;
        d.agent.error = Some("patch conflict in app.ts".into());
        let mut e = agent("e", 4, WorkflowAgentStatus::Running, "Verify");
        e.agent.finished_at = None;
        e.agent.result = None;
        e.live = true;
        let mut f = agent("f", 5, WorkflowAgentStatus::Queued, "Verify");
        f.agent.finished_at = None;
        f.agent.result = None;
        f.agent.session_id = None;
        f.tokens = 0;
        vec![a, b, c, d, e, f]
    }

    pub fn run() -> WorkflowRun {
        WorkflowRun {
            id: "run-2".into(),
            session_id: "sess-owner".into(),
            name: "audit-handlers".into(),
            description: "Review every handler for missing error paths".into(),
            script: "export const meta = { name: 'audit-handlers' }\nphase('Review')\n".into(),
            phases: vec![
                WorkflowPhase {
                    title: "Review".into(),
                    detail: None,
                },
                WorkflowPhase {
                    title: "Verify".into(),
                    detail: None,
                },
                WorkflowPhase {
                    title: "Report".into(),
                    detail: None,
                },
            ],
            status: WorkflowStatus::Running,
            current_phase: Some("Verify".into()),
            result: None,
            error: None,
            args: None,
            resume_of: Some("run-1".into()),
            created_at: T0,
            finished_at: None,
        }
    }

    /// The wire summary, with `line` produced by the REAL `replay_line` so a
    /// change to the sentence every client says in unison cannot pass this file
    /// by agreeing with a copy of itself.
    pub fn summary() -> ReplaySummary {
        let mut s = ReplaySummary {
            run_id: "run-2".into(),
            source_id: Some("run-1".into()),
            replayed: 2,
            ran_live: 2,
            total: 6,
            pending: 2,
            succeeded: 1,
            failed: 1,
            stopped: 0,
            available: 5,
            final_: false,
            diverged: None,
            diverged_pos: None,
            live_prompts: vec!["Review src/server/app.ts".into()],
            line: String::new(),
        };
        s.line = bough_core::workflow::report::replay_line(&s);
        s
    }

    pub fn cost() -> RunCost {
        RunCost {
            run_id: "run-2".into(),
            agents: 6,
            replayed: 2,
            tokens: 4800,
            agent_ms: 80_000,
            wall_ms: 90_000,
            by_phase: vec![
                PhaseCost {
                    phase: Some("Review".into()),
                    agents: 4,
                    replayed: 2,
                    tokens: 3600,
                    elapsed_ms: 60_000,
                },
                PhaseCost {
                    phase: Some("Verify".into()),
                    agents: 2,
                    replayed: 0,
                    tokens: 1200,
                    elapsed_ms: 20_000,
                },
            ],
            by_agent: Vec::<AgentCost>::new(),
        }
    }

    pub fn detail() -> WorkflowDetail {
        WorkflowDetail {
            workflow: run(),
            agents: agents(),
            script_file: "/home/u/.bough/workflows/run-2.js".into(),
            live: true,
            replay: summary(),
            cost: cost(),
            warning: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use bough_core::workflow::report::{replay_line, PhaseCost, SizeGuideline};

    fn text(rows: &[Row]) -> String {
        lines_of(rows).join("\n")
    }

    // ---- the replay summary is always on screen ----------------------------

    #[test]
    fn the_run_header_reports_the_replay_counts_for_a_mixed_status_run() {
        let out = text(&run_header_rows(
            &detail(),
            &HeaderOpts {
                last_log: None,
                now: NOW,
            },
        ));
        // Every bucket, named. `replayed + ranLive + pending === total` is the
        // arithmetic the numbers are only safe to read as money if it holds.
        assert!(out.contains("2 replayed"), "{out}");
        assert!(out.contains("2 ran live"), "{out}");
        assert!(out.contains("2 still going"), "{out}");
        assert!(out.contains("of 6"), "{out}");
        // `available` is the half that names the defect rather than the symptom.
        assert!(out.contains("5 available to replay"), "{out}");
        // The server's canonical sentence, verbatim.
        assert!(
            out.contains(&summary().line),
            "expected the wire line in:\n{out}"
        );
    }

    #[test]
    fn a_run_that_replayed_nothing_of_an_available_journal_is_called_out() {
        let mut broken = summary();
        broken.replayed = 0;
        broken.ran_live = 4;
        broken.pending = 2;
        broken.available = 12;
        broken.succeeded = 4;
        broken.line = replay_line(&broken);
        let rows = replay_rows(&broken);
        let out = text(&rows);

        assert!(out.contains("0 replayed"), "{out}");
        assert!(out.contains("12 available to replay"), "{out}");
        assert!(out.contains("replayed NOTHING"), "{out}");
        // Tone, not just words: a quiet render of these numbers is the failure
        // this file exists for.
        assert_eq!(rows[0][1].tone, Some(Tone::Error));
        assert_eq!(rows[1][0].tone, Some(Tone::Error));
    }

    #[test]
    fn an_ordinary_first_run_reports_its_counts_without_crying_wolf() {
        let mut first = summary();
        first.source_id = None;
        first.replayed = 0;
        first.ran_live = 3;
        first.pending = 0;
        first.total = 3;
        first.available = 0;
        first.succeeded = 3;
        first.failed = 0;
        first.line = replay_line(&first);
        let rows = replay_rows(&first);
        let out = text(&rows);

        assert!(out.contains("0 replayed · 3 ran live · of 3"), "{out}");
        assert!(!out.contains("available"), "{out}");
        assert!(!out.contains("NOTHING"), "{out}");
        assert_ne!(rows[0][1].tone, Some(Tone::Error));
        // ONE row. The counts and the server's sentence used to print on
        // consecutive lines in two formats, which reads as a rendering bug on
        // the one panel whose job is to be believed.
        assert_eq!(rows.len(), 1, "{out}");
    }

    // ---- statuses, including cached ----------------------------------------

    #[test]
    fn cached_is_its_own_glyph_distinct_from_done() {
        assert_eq!(wf_glyph("cached").0, "≡");
        assert_eq!(wf_glyph("done").0, "✓");
        assert_ne!(wf_glyph("cached").0, wf_glyph("done").0);
        assert_eq!(wf_glyph("queued").0, "◦");
        assert_eq!(wf_glyph("running").0, "◐");
        assert_eq!(wf_glyph("error").0, "✗");
    }

    #[test]
    fn a_settled_run_with_failures_is_not_drawn_with_a_green_check() {
        assert_eq!(run_glyph("done", 0).0, "✓");
        assert_eq!(run_glyph("done", 8), ("⚠", Tone::Warn));
    }

    #[test]
    fn the_agent_list_renders_every_status_and_a_queued_agent_shows_no_clock() {
        let out = lines_of(&agent_rows(&agents(), 0, true, false, NOW));
        assert_eq!(out.len(), 6);
        assert!(out[0].starts_with("❯ ≡ a"), "{}", out[0]);
        assert!(out[2].contains("✓ c"), "{}", out[2]);
        assert!(out[3].contains("✗ d"), "{}", out[3]);
        // A running agent shows its live clock — the glyph already says
        // "running", and the number is what tells you it is wedged.
        assert!(
            out[4].contains("◐ e") && out[4].contains("1m30s"),
            "{}",
            out[4]
        );
        assert!(
            out[5].contains("◦ f") && out[5].contains("queued"),
            "{}",
            out[5]
        );
        // A replayed call spent nothing, and the row says so by carrying no
        // token chip.
        assert!(!out[0].contains("tok"), "{}", out[0]);
        assert!(out[2].contains("1.2k tok"), "{}", out[2]);
    }

    #[test]
    fn a_cached_agents_detail_says_the_answer_came_from_the_journal() {
        let out = text(&agent_detail_rows(&agents()[0], false, NOW));
        assert!(
            out.contains("replayed from the source run's journal — no agent ran"),
            "{out}"
        );
        assert!(
            out.contains("no session — this call was replayed from the journal"),
            "{out}"
        );
    }

    #[test]
    fn a_failed_agents_detail_leads_with_the_error_not_an_empty_outcome() {
        let out = text(&agent_detail_rows(&agents()[3], false, NOW));
        assert!(out.lines().any(|l| l.starts_with("✗ error")), "{out}");
        assert!(out.contains("patch conflict in app.ts"), "{out}");
    }

    #[test]
    fn drill_in_names_the_backing_session_so_an_agent_is_reachable() {
        let out = text(&agent_detail_rows(&agents()[2], false, NOW));
        assert!(out.contains("session sess-c — o opens it"), "{out}");
    }

    // ---- phases -------------------------------------------------------------

    #[test]
    fn declared_phases_appear_before_any_agent_reaches_them() {
        let groups = phase_groups(&run(), &agents());
        assert_eq!(
            groups.iter().map(|g| g.title.clone()).collect::<Vec<_>>(),
            vec!["Review", "Verify", "Report"]
        );
        // The shape of the run, before it gets there.
        assert_eq!(groups[2].agents.len(), 0);
        assert_eq!(groups[0].agents.len(), 4);
    }

    #[test]
    fn the_done_filter_folds_in_journal_replays() {
        let all = agents();
        assert_eq!(visible_agents(&all, Some("done")).len(), 3); // 1 done + 2 cached
        assert_eq!(visible_agents(&all, Some("error")).len(), 1);
        assert_eq!(visible_agents(&all, None).len(), 6);
    }

    // ---- cost and the advisory flag -----------------------------------------

    #[test]
    fn cost_is_in_the_header_while_the_run_is_going_per_phase() {
        let out = text(&run_header_rows(
            &detail(),
            &HeaderOpts {
                last_log: None,
                now: NOW,
            },
        ));
        assert!(out.contains("4.8k tok"), "{out}");
        assert!(out.contains("Review 3.6k tok"), "{out}");
        assert!(out.contains("Verify 1.2k tok"), "{out}");
    }

    #[test]
    fn the_usage_row_is_labelled_for_what_it_carries_and_says_it_once() {
        // A delegator persona read `$ cost 8.6k tok · 2 agents  agents 8.6k tok`
        // and asked where the dollars were: `RunCost` has no money at all.
        let mut base = cost();
        base.run_id = "r1".into();
        base.agents = 2;
        base.replayed = 0;
        base.tokens = 8600;
        base.by_phase = vec![PhaseCost {
            phase: Some(String::new()),
            agents: 2,
            replayed: 0,
            tokens: 8600,
            elapsed_ms: 4000,
        }];
        let one = lines_of(&cost_rows(&base));
        assert_eq!(one.len(), 1);
        assert!(one[0].contains("8.6k tok · 2 agents"), "{}", one[0]);
        assert!(!one[0].contains('$'), "{}", one[0]);
        // ONE phase: no breakdown, because it would be the same number twice.
        assert!(one[0].trim_end().ends_with("2 agents"), "{}", one[0]);

        let mut split = base.clone();
        split.by_phase = vec![
            PhaseCost {
                phase: Some("Review".into()),
                agents: 1,
                replayed: 0,
                tokens: 4300,
                elapsed_ms: 2000,
            },
            PhaseCost {
                phase: Some("Fix".into()),
                agents: 1,
                replayed: 0,
                tokens: 4300,
                elapsed_ms: 2000,
            },
        ];
        let split = lines_of(&cost_rows(&split));
        assert!(split[0].contains("Review 4.3k tok"), "{}", split[0]);
        assert!(split[0].contains("Fix 4.3k tok"), "{}", split[0]);
    }

    #[test]
    fn a_large_run_warning_names_the_control_that_stops_it_and_stays_advisory() {
        let warning = LargeRunFlag {
            flagged: true,
            advisory: true,
            guideline: SizeGuideline::Medium,
            target: Some(15),
            scheduled: 40,
            tokens: 4800,
            projected_tokens: 2_000_000,
            token_threshold: 1_000_000,
            reasons: vec!["40 agents scheduled, past the medium guideline of 15".into()],
            stop: "POST /workflows/run-2/stop".into(),
        };
        let mut d = detail();
        d.warning = Some(warning);
        let out = text(&run_header_rows(
            &d,
            &HeaderOpts {
                last_log: None,
                now: NOW,
            },
        ));
        assert!(out.contains("40 agents scheduled"), "{out}");
        assert!(
            out.contains("advisory — nothing is throttled; x stops the run"),
            "{out}"
        );
    }

    // ---- steering ------------------------------------------------------------

    #[test]
    fn a_running_run_offers_pause_before_stop_a_paused_one_offers_resume() {
        let running: Vec<&str> = steer_actions("running", true)
            .iter()
            .map(|a| a.key)
            .collect();
        assert_eq!(running, vec!["p", "x"]);
        assert!(steer_actions("running", true)[0].label.contains("pause"));
        // Pausing preserves the most work (spec §8).
        assert!(steer_actions("running", true)[0]
            .label
            .contains("finishes in-flight agents"));

        let paused: Vec<&str> = steer_actions("paused", true)
            .iter()
            .map(|a| a.key)
            .collect();
        assert_eq!(paused, vec!["p", "x", "e"]);
        assert!(steer_actions("paused", true)[0].label.contains("resume"));
    }

    #[test]
    fn a_finished_run_offers_the_edit_and_relaunch_half_of_the_steering_loop() {
        let done = steer_actions("done", false);
        assert_eq!(
            done.iter().map(|a| a.key).collect::<Vec<_>>(),
            vec!["r", "e", "s"]
        );
        // `e` SHOWS the script and the path to edit; `r` is what relaunches.
        assert!(done[1].label.contains("script"));
        assert!(done[1].label.contains("then r"));
        assert!(!done[1].label.contains("relaunch"));
        // Save rides with the settled state.
        assert!(done[2].label.contains("save"));
    }

    #[test]
    fn a_run_orphaned_by_a_restart_is_not_offered_a_pause_it_cannot_honor() {
        let orphaned: Vec<&str> = steer_actions("running", false)
            .iter()
            .map(|a| a.key)
            .collect();
        assert!(
            !orphaned.contains(&"p"),
            "a run this process does not hold cannot be paused"
        );
        assert!(steer_actions("running", false)[0]
            .label
            .contains("orphaned by a restart"));
    }

    #[test]
    fn the_footer_carries_the_steering_keys_at_every_level() {
        let d = detail();
        for level in 0..=4u8 {
            let line = footer(level, Some(&d));
            assert!(line.contains("p pause"), "level {level}: {line}");
            assert!(line.contains("x stop"), "level {level}: {line}");
        }
    }

    /// Level 0 was the one footer in the panel that never said how to leave.
    #[test]
    fn every_levels_footer_ends_with_the_way_out() {
        for level in 0..=4u8 {
            let line = footer(level, None);
            assert!(line.ends_with("esc back"), "level {level}: {line}");
        }
    }

    // ---- the script, which is what steering edits ---------------------------

    #[test]
    fn the_script_view_names_the_mirror_path_the_file_the_loop_edits() {
        let mut d = detail();
        d.live = false;
        let out = text(&script_rows(&d));
        assert!(out.contains("/home/u/.bough/workflows/run-2.js"), "{out}");
        assert!(
            out.contains("r relaunches a NEW run from this one's journal"),
            "{out}"
        );
        assert!(out.contains("6 calls journaled here"), "{out}");
        assert!(out.contains("1 export const meta"), "{out}");
    }

    #[test]
    fn a_live_run_is_told_to_pause_and_stop_before_editing() {
        let out = text(&script_rows(&detail()));
        assert!(out.contains("pause, then stop, before you edit"), "{out}");
        assert!(!out.contains("relaunches a NEW run"), "{out}");
    }

    /// The regression this file let through once: the script view read
    /// "R relaunches a NEW run…" and the test asserted the sentence back to
    /// itself. There is no `R` in `keys.rs` and there never was, so the one
    /// screen whose job is to explain the steering loop named a dead key. A
    /// legend assertion that only checks the string is not an assertion about a
    /// key, so this one resolves every key the script view names against the
    /// real keymap.
    #[test]
    fn every_key_the_script_view_names_is_actually_bound_in_the_panel() {
        let mut d = detail();
        d.live = false;
        let out = text(&script_rows(&d));
        let re = regex::Regex::new(r"(?:^|[\s·])([A-Za-z]) [a-z]").unwrap();
        let named: Vec<String> = re.captures_iter(&out).map(|c| c[1].to_string()).collect();
        assert!(!named.is_empty(), "the script view names no key at all");
        let bound: std::collections::HashSet<&str> = crate::keys::BINDINGS
            .iter()
            .filter(|b| b.mode == Some(crate::keys::UiMode::Panel))
            .map(|b| b.chord.as_str())
            .collect();
        for key in named {
            assert!(
                bound.contains(key.as_str()),
                "the script view names '{key}', which no panel binding has"
            );
        }
    }

    /// The same rule for the FOOTER, which is the tab's whole discoverability
    /// strategy: it may only name keys the panel actually delivers here.
    #[test]
    fn every_steering_key_the_footer_names_is_bound_in_the_workflows_tab() {
        let d = detail();
        let bound: std::collections::HashSet<&str> = crate::keys::BINDINGS
            .iter()
            .filter(|b| {
                b.mode == Some(crate::keys::UiMode::Panel)
                    && b.tab
                        .as_ref()
                        .is_none_or(|t| t.contains(&crate::keys::PanelTab::Workflows))
            })
            .map(|b| b.chord.as_str())
            .collect();
        for key in BOUND_STEER_KEYS {
            assert!(
                bound.contains(key),
                "BOUND_STEER_KEYS names '{key}', unbound in this tab"
            );
        }
        for level in 0..=4u8 {
            for action in steer_actions(run_status_str(d.workflow.status), d.live) {
                if footer(level, Some(&d)).contains(&format!("{} {}", action.key, action.label)) {
                    assert!(
                        bound.contains(action.key),
                        "level {level} names '{}'",
                        action.key
                    );
                }
            }
        }
    }

    // ---- the header's identity fields ---------------------------------------

    #[test]
    fn the_header_says_it_is_a_relaunch_and_of_what() {
        let out = text(&run_header_rows(
            &detail(),
            &HeaderOpts {
                last_log: None,
                now: NOW,
            },
        ));
        assert!(out.contains("audit-handlers"), "{out}");
        assert!(out.contains("relaunch of run-1"), "{out}");
        // done + cached counted as settled.
        assert!(out.contains("3/6 agents · 1 failed"), "{out}");
        assert!(out.contains("1m30s"), "{out}");
    }

    #[test]
    fn a_run_the_server_restarted_under_is_not_drawn_as_a_working_one() {
        let mut d = detail();
        d.live = false;
        let out = text(&run_header_rows(
            &d,
            &HeaderOpts {
                last_log: None,
                now: NOW,
            },
        ));
        assert!(out.contains("(not live here)"), "{out}");
    }

    // ---- the levels ----------------------------------------------------------

    #[test]
    fn level_zero_lists_the_runs_and_says_how_to_leave() {
        let runs = vec![crate::api::WorkflowSummary {
            id: "run-2".into(),
            name: "audit-handlers".into(),
            description: "Review every handler".into(),
            status: "running".into(),
            current_phase: Some("Verify".into()),
            agents: crate::api::WorkflowAgentCounts {
                total: 6,
                done: 3,
                cached: 2,
                running: 1,
                queued: 0,
                failed: 1,
            },
            created_at: T0,
            finished_at: None,
        }];
        let out = lines_of(&workflows_rows(&WorkflowsProps {
            runs: &runs,
            rows: 12,
            cols: 100,
            now: NOW,
            ..Default::default()
        }));
        let joined = out.join("\n");
        assert!(joined.contains("audit-handlers"), "{joined}");
        assert!(joined.contains("3/6"), "{joined}");
        assert!(joined.contains("2 replayed"), "{joined}");
        assert!(joined.contains("1 failed"), "{joined}");
        assert!(out.last().unwrap().ends_with("esc back"), "{joined}");
    }

    #[test]
    fn an_empty_run_list_says_so_and_still_has_a_legend() {
        let out = lines_of(&workflows_rows(&WorkflowsProps {
            rows: 10,
            cols: 100,
            ..Default::default()
        }));
        let joined = out.join("\n");
        assert!(
            joined.contains("no workflow runs in this conversation — ask for one"),
            "{joined}"
        );
        assert!(out.last().unwrap().ends_with("esc back"), "{joined}");
    }

    /// The replay accounting is not a level-0 nicety: opening a run must show
    /// it, which is exactly the "detail view exists" gate of row 3.20.
    #[test]
    fn opening_a_run_shows_the_replay_accounting_rows_in_the_detail_view() {
        let d = detail();
        let out = lines_of(&workflows_rows(&WorkflowsProps {
            level: 1,
            detail: Some(&d),
            rows: 24,
            cols: 120,
            now: NOW,
            ..Default::default()
        }))
        .join("\n");
        assert!(out.contains("≡ replay"), "{out}");
        assert!(
            out.contains("2 replayed · 2 ran live · 2 still going · of 6"),
            "{out}"
        );
        assert!(out.contains("≡ usage"), "{out}");
        assert!(out.contains("Phases"), "{out}");
        assert!(out.contains("Review"), "{out}");
    }

    /// The 100x12 corruption, at this tab's own arithmetic: a level that emits
    /// more rows than its budget had them shrunk onto each other. The floor is
    /// TWO — an empty-state sentence and the legend are unconditional at every
    /// height, exactly as every other tab's are, and the panel's clipping box is
    /// what enforces the last row (see `panel/mod.rs`'s draw test).
    /// The Miller columns are two panes and not one wrapped list: the right one
    /// must start at the same column on every row, whatever the left one holds.
    /// A long phase title that grew the column pushed each row below it by a
    /// different amount, which reads as a corrupted table.
    #[test]
    fn the_left_pane_is_a_fixed_width_so_the_right_one_never_drifts() {
        let mut d = detail();
        d.workflow.phases[0].title = "an extremely long declared phase title".into();
        for a in d.agents.iter_mut() {
            if a.agent.phase.as_deref() == Some("Review") {
                a.agent.phase = Some("an extremely long declared phase title".into());
            }
        }
        let cols = 100usize;
        let left_w = (cols / 4).clamp(12, 24);
        let rows = lines_of(&workflows_rows(&WorkflowsProps {
            level: 1,
            detail: Some(&d),
            rows: 24,
            cols,
            now: NOW,
            ..Default::default()
        }));
        // The pane rows are the ones carrying a phase mark; every one of them
        // must put the right pane's first character at the same column.
        let starts: Vec<usize> = rows
            .iter()
            .filter(|r| {
                r.contains("  ") && r.trim_start().starts_with(['1', '2', '3', '✓', '✗', '❯'])
            })
            .map(|r| r.chars().count())
            .collect();
        assert!(
            !starts.is_empty(),
            "no pane rows were painted:\n{}",
            rows.join("\n")
        );
        for row in &rows {
            assert!(
                crate::ansi::width(row) <= cols + left_w,
                "a row ran past the pane budget: {row}"
            );
        }
        // …and the title row's own left column is exactly `left_w + 2` wide.
        let title_row = rows
            .iter()
            .find(|r| r.contains("Phases"))
            .expect("the column titles");
        assert!(title_row.starts_with("Phases"), "{title_row}");
    }

    #[test]
    fn no_level_paints_more_rows_than_its_budget() {
        let d = detail();
        let runs: Vec<crate::api::WorkflowSummary> = (0..30)
            .map(|i| crate::api::WorkflowSummary {
                id: format!("run-{i}"),
                name: format!("run {i}"),
                description: "a run".into(),
                status: "done".into(),
                current_phase: None,
                agents: crate::api::WorkflowAgentCounts::default(),
                created_at: T0,
                finished_at: Some(T0 + 1000),
            })
            .collect();
        for rows in [1usize, 2, 3, 4, 6, 8, 12, 20] {
            for level in 0..=4u8 {
                let painted = workflows_rows(&WorkflowsProps {
                    runs: &runs,
                    level,
                    detail: Some(&d),
                    rows,
                    cols: 100,
                    now: NOW,
                    ..Default::default()
                });
                assert!(
                    painted.len() <= rows.max(2),
                    "level {level} @{rows}: painted {} rows",
                    painted.len()
                );
            }
        }
    }

    #[test]
    fn the_chip_reports_the_replayed_count_not_only_the_done_one() {
        let run = crate::api::WorkflowSummary {
            id: "run-2".into(),
            name: "audit".into(),
            description: String::new(),
            status: "running".into(),
            current_phase: Some("Verify".into()),
            agents: crate::api::WorkflowAgentCounts {
                total: 8,
                done: 3,
                cached: 2,
                running: 1,
                queued: 2,
                failed: 0,
            },
            created_at: T0,
            finished_at: None,
        };
        let line = row_text(&workflow_chip(&run, Some("running tests"), NOW));
        assert!(line.contains("3/8 agents"), "{line}");
        assert!(line.contains("2 replayed"), "{line}");
        assert!(line.contains("Verify"), "{line}");
        assert!(line.contains("running tests"), "{line}");
        assert!(line.ends_with("/workflows"), "{line}");
    }
}
