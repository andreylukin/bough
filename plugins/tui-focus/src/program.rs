//! Invariant: ONE program is ONE row. The `tool/call` that carries the JS source, every
//! `program/*` step written from inside it, and the `tool/result` that closes it fold into a
//! single [`Row::Program`] — the same "no step is rendered twice" rule the `tool/call` +
//! `tool/result` pair already obeys (§2.4, `invariant.rs`).
//!
//! The fold is keyed on the codemode consumer's protocol constant [`RUN_TOOL`], not on a fourth
//! `RenderIntent` (P-CM-D13): `RenderIntent` lives in `plugins/tools`, which this branch does not
//! edit, and the render decision belongs to the surface anyway. The step BODIES are read BY NAME,
//! the same way `claim/*` and `about/line` are (P3-D11), so this crate gains no dependency on
//! `tools-codemode` — which is also what keeps the two crates buildable in either order.
//!
//! TOTAL, like the rest of the projection: a `program/*` step whose body does not match its
//! declared shape, or whose program is not in this window, degrades to [`Row::Other`]. A surface
//! that panicked on an unfamiliar sub-step would take the terminal down with it.

use std::collections::BTreeMap;

use bough_plugin_ledger::{Step, StepId};
use bough_plugin_llm::ToolCallId;
use bough_plugin_tools::{RenderIntent, ToolResultBody};
use bough_plugin_tui_render::{highlight, tool_header, wrap, ToolCallView};
use bough_plugin_tui_shell::Theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::expand::Expanded;
use crate::rows::Row;

/// The ONE API tool code mode exposes (`bough-plugin-tools-codemode::RUN_TOOL`). Spelled here
/// rather than imported: the fold reads the LEDGER, and a surface must not depend on the consumer
/// that happens to be mounted. Pinned to the consumer's constant by the codemode row's own test
/// (`plugins/tools-codemode/tests/pins.rs`, `run_tool_name_is_pinned_to_the_focus_pane_fold`).
pub const RUN_TOOL: &str = "run";

/// The syntect token the source block is highlighted under.
const SOURCE_EXT: &str = "js";

/// One inner tool call made from inside a program: a `program/call` step and the `program/result`
/// that answers it, folded exactly as a top-level pair is.
#[derive(Clone, Debug, PartialEq)]
pub struct ProgramSub {
    /// 0-based, in issue order within the program.
    pub index: u32,
    /// `{program}.{index}` — deterministic, which is what lets a sub-row own a disclosure key.
    pub call: ToolCallId,
    pub name: String,
    pub intent: RenderIntent,
    pub args: serde_json::Value,
    pub result: Option<ToolResultBody>,
    pub call_step: StepId,
}

/// The one terminal error a program can end with, read from a `program/error` body. The typed
/// `JsError` enum lives in `plugins/js`; the surface needs its TAG and its message, so it reads
/// them by name rather than taking the dependency.
#[derive(Clone, Debug, PartialEq)]
pub struct ProgramError {
    /// The serde tag: `syntax`, `thrown`, `ops_exceeded`, `time_exceeded`, `cancelled`, …
    pub kind: String,
    /// Whatever the variant carried that a person can read. Empty when the variant is a bare tag.
    pub detail: String,
}

impl ProgramError {
    /// The typed error line, in the vocabulary the model was given. Never a bare "error".
    pub fn line(&self) -> String {
        if self.detail.is_empty() {
            format!("\u{2717} {}", self.kind)
        } else {
            format!("\u{2717} {} \u{b7} {}", self.kind, self.detail)
        }
    }
}

/// PURE: a `program/error` body into the row's error. `None` when the body is not one.
pub fn error_from_body(body: &serde_json::Value) -> Option<ProgramError> {
    let err = body.get("error")?;
    let kind = err.get("kind")?.as_str()?.to_string();
    // In tag order of usefulness: the thrown/syntax message, then the cap that was breached.
    let detail = err
        .get("message")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| err.get("ops").map(|v| format!("{v} ops")))
        .or_else(|| err.get("bytes").map(|v| format!("{v} bytes")))
        .or_else(|| err.get("ms").map(|v| format!("{v}ms")))
        .unwrap_or_default();
    Some(ProgramError { kind, detail })
}

/// PURE: the `tool/call` of a `run` into a fresh [`Row::Program`]. `None` when the body is not a
/// tool call at all — the caller then renders it as [`Row::Other`].
pub fn program_row(step: &Step) -> Option<(ToolCallId, Row)> {
    let call = ToolCallId::new(step.body.get("call")?.as_str()?);
    // A `run` call whose args lost the program still renders: the row is the anchor for every
    // sub-step that follows, and dropping it would strand them all.
    let source = step
        .body
        .get("args")
        .and_then(|a| a.get("program"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    Some((
        call.clone(),
        Row::Program {
            call,
            source,
            console: String::new(),
            subs: Vec::new(),
            result: None,
            error: None,
            ops: 0,
            ms: 0,
            call_step: step.id.clone(),
            parts: vec![step.id.clone()],
        },
    ))
}

/// PURE: fold one `program/*` step into the row it belongs to. `true` when it was folded; `false`
/// when the caller must render it as [`Row::Other`] (an unparseable body, or a program that has
/// paged out of this window).
///
/// `by_call` is the projection's call-id → row-index map; `by_sub` is this fold's own sub-call
/// index, so a `program/result` reaches the [`ProgramSub`] its `program/call` created.
pub fn fold_sub(
    out: &mut [Row],
    by_call: &BTreeMap<ToolCallId, usize>,
    by_sub: &mut BTreeMap<ToolCallId, (usize, usize)>,
    step: &Step,
) -> bool {
    let kind = step.kind.as_str();
    let Some(program) = step
        .body
        .get("program")
        .and_then(|v| v.as_str())
        .map(ToolCallId::new)
    else {
        return false;
    };
    let Some(at) = by_call.get(&program).copied() else {
        return false;
    };
    let Some(Row::Program {
        console,
        subs,
        error,
        ops,
        ms,
        parts,
        ..
    }) = out.get_mut(at)
    else {
        return false;
    };
    match kind {
        "program/call" => {
            let Some(sub) = sub_from_call(step) else {
                return false;
            };
            by_sub.insert(sub.call.clone(), (at, subs.len()));
            subs.push(sub);
        }
        "program/result" => {
            let Some(call) = step
                .body
                .get("call")
                .and_then(|v| v.as_str())
                .map(ToolCallId::new)
            else {
                return false;
            };
            let Ok(body) = serde_json::from_value::<ToolResultBody>(step.body.as_ref().clone())
            else {
                return false;
            };
            // A result whose call is not in this window is NOT dropped: the answer is the news,
            // exactly as it is for a top-level `tool/result`. It joins the program as a sub with
            // no arguments.
            match by_sub.get(&call).copied() {
                Some((row, idx)) if row == at => {
                    subs[idx].result = Some(body);
                }
                _ => {
                    by_sub.insert(call.clone(), (at, subs.len()));
                    subs.push(ProgramSub {
                        index: body_u32(step, "index"),
                        call,
                        name: body.name.to_string(),
                        intent: RenderIntent::Generic,
                        args: serde_json::Value::Null,
                        result: Some(body),
                        call_step: step.id.clone(),
                    });
                }
            }
        }
        "program/console" => {
            let Some(text) = step.body.get("text").and_then(|v| v.as_str()) else {
                return false;
            };
            // RAW CONCATENATION: the chunk boundary is a flush timer, not a line, and the
            // consumer's own invariant is that the chunks reassemble into the result content.
            console.push_str(text);
        }
        "program/error" => {
            let Some(e) = error_from_body(&step.body) else {
                return false;
            };
            *error = Some(e);
            *ops = body_u64(step, "ops");
            *ms = body_u64(step, "ms");
        }
        // An unknown `program/*` kind. Not this fold's business: the caller renders it as `Other`.
        _ => return false,
    }
    parts.push(step.id.clone());
    true
}

fn sub_from_call(step: &Step) -> Option<ProgramSub> {
    let call = ToolCallId::new(step.body.get("call")?.as_str()?);
    let name = step.body.get("name")?.as_str()?.to_string();
    Some(ProgramSub {
        index: body_u32(step, "index"),
        call,
        name,
        intent: step
            .body
            .get("render")
            .and_then(|v| serde_json::from_value::<RenderIntent>(v.clone()).ok())
            .unwrap_or(RenderIntent::Generic),
        args: step
            .body
            .get("args")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        result: None,
        call_step: step.id.clone(),
    })
}

fn body_u32(step: &Step, key: &str) -> u32 {
    body_u64(step, key).min(u32::MAX as u64) as u32
}

fn body_u64(step: &Step, key: &str) -> u64 {
    step.body.get(key).and_then(|v| v.as_u64()).unwrap_or(0)
}

/// One program row, as the pane wants to draw it. The mirror of [`ToolCallView`].
pub struct ProgramView<'a> {
    pub call: &'a ToolCallId,
    pub source: &'a str,
    pub console: &'a str,
    pub subs: &'a [ProgramSub],
    pub result: Option<&'a ToolResultBody>,
    pub error: Option<&'a ProgramError>,
    pub ms: u64,
    /// Which calls are drawn open — the program's own id AND its subs' ids, one set (`expand.rs`).
    pub expanded: &'a Expanded,
    pub width: u16,
    pub theme: &'a Theme,
    /// The fold applied to each SUB's body, the pane's `max_tool_lines`.
    pub max_tool_lines: usize,
}

/// PURE: the collapsed one-liner — `▸ program  4 calls · 1.2s        ✓`.
///
/// Built through [`tool_header`] rather than by hand so the disclosure marker, the ✓/✗/⋯ glyph and
/// its colour rule, and the EXACT-WIDTH guarantee are the ones every other tool row already has.
/// A program row that measured itself differently would jitter against its neighbours.
pub fn program_header(v: &ProgramView<'_>) -> Line<'static> {
    let gist = serde_json::json!({ "summary": summary(v.subs.len(), v.ms) });
    tool_header(&ToolCallView {
        name: "program",
        intent: RenderIntent::Generic,
        args: &gist,
        result: v.result,
        expanded: v.expanded.is_expanded(v.call),
        width: v.width,
        theme: v.theme,
    })
}

/// PURE: the collapsed line's gist. `1 call`, never `1 calls`; the duration is dropped rather than
/// printed as `0ms` when nothing measured it.
pub fn summary(calls: usize, ms: u64) -> String {
    let noun = if calls == 1 { "call" } else { "calls" };
    if ms == 0 {
        format!("{calls} {noun}")
    } else {
        format!("{calls} {noun} \u{b7} {}", fmt_ms(ms))
    }
}

fn fmt_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

/// PURE: the whole row — the header, and when it is open the JS source, the console output beneath
/// it, and the sub-calls as nested tool rows.
///
/// The second value is every clickable header in this row (the program's own first, then its
/// subs'), as an OFFSET from the row's first line; the pane adds its own base.
pub fn program_lines(v: &ProgramView<'_>) -> (Vec<Line<'static>>, Vec<(ToolCallId, u16)>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut headers: Vec<(ToolCallId, u16)> = Vec::new();
    headers.push((v.call.clone(), 0));
    lines.push(program_header(v));
    if !v.expanded.is_expanded(v.call) {
        return (lines, headers);
    }
    let dim = Style::default().fg(v.theme.dim);

    // The terminal outcome first when it is an error: the reason a program ended is the thing to
    // read before the code that got there.
    if let Some(e) = v.error {
        lines.push(Line::styled(
            e.line(),
            Style::default()
                .fg(v.theme.error)
                .add_modifier(Modifier::BOLD),
        ));
    }

    lines.extend(highlight(v.source, Some(SOURCE_EXT), v.theme));

    // The console output IS what the model received (D-4). It sits under the source, labelled, so
    // it never reads as more program text.
    lines.push(Line::from(vec![Span::styled("console", dim)]));
    if v.console.is_empty() {
        lines.push(Line::styled("(no output)".to_string(), dim));
    } else {
        for chunk in wrap(v.console, v.width) {
            lines.push(Line::styled(chunk, Style::default().fg(v.theme.fg)));
        }
    }

    // The sub-calls, as ordinary tool rows: same header, same ✓/✗ marks, same disclosure.
    for sub in v.subs {
        let view = ToolCallView {
            name: &sub.name,
            intent: sub.intent,
            args: &sub.args,
            result: sub.result.as_ref(),
            expanded: v.expanded.is_expanded(&sub.call),
            width: v.width,
            theme: v.theme,
        };
        headers.push((sub.call.clone(), lines.len() as u16));
        lines.push(tool_header(&view));
        if view.expanded {
            lines.extend(bough_plugin_tui_render::tool_body(&view, v.max_tool_lines));
        }
    }
    // The opened block sits on the code ground (the TUI brief, D3): source, console and the
    // inner calls are one object from the header's next line to the last, the same texture a
    // fenced code block has, so prose after it is visibly back on the transcript's ground.
    for line in lines.iter_mut().skip(1) {
        *line = line
            .clone()
            .patch_style(Style::default().bg(v.theme.code_bg));
    }
    (lines, headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_counts_and_never_prints_a_zero_duration() {
        assert_eq!(summary(0, 0), "0 calls");
        assert_eq!(summary(1, 0), "1 call");
        assert_eq!(summary(4, 1200), "4 calls \u{b7} 1.2s");
        assert_eq!(summary(2, 40), "2 calls \u{b7} 40ms");
    }

    #[test]
    fn a_typed_error_keeps_its_tag() {
        let e = error_from_body(&serde_json::json!({
            "program": "c1", "ops": 0, "ms": 5000,
            "error": { "kind": "time_exceeded", "ms": 5000 }
        }))
        .expect("a program/error body");
        assert_eq!(e.kind, "time_exceeded");
        assert!(e.line().contains("time_exceeded"), "{}", e.line());
        assert!(e.line().contains("5000ms"), "{}", e.line());
        // A body that is not a `program/error` is not one.
        assert!(error_from_body(&serde_json::json!({ "program": "c1" })).is_none());
    }
}
