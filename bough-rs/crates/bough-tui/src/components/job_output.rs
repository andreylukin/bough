//! One background job, opened: its header and its whole retained buffer (port
//! of `src/tui/components/JobOutput.tsx`).
//!
//! WHY THIS EXISTS. A background job was a rail row and nothing else — what it
//! had PRINTED was reachable only by asking the model to call `bashOutput`,
//! which is a spinner, a token bill and a wait to see text the server already
//! had in memory. `⏎` on the rail row opens this instead, and
//! `GET /sessions/:id/jobs/:jobId/output` is non-destructive, so watching a
//! job never eats output the next round was owed.
//!
//! THE INVARIANT THIS HOLDS: **presentational only.** The buffer, the row and
//! the scroll offset are props; the fetch and the poll belong to the loop.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;

use bough_core::schema::parts::{BackgroundJob, JobStatus};

use crate::ansi::{line_from_ansi, truncate_ansi, wrap_line};
use crate::components::pad_row_ansi;
use crate::format::{accent, bold, danger, dim, warn};
use crate::store::selectors::{fmt_duration, one_line};

pub struct JobOutputProps<'a> {
    /// The id, kept separate: a job whose row failed to load still has one.
    pub id: &'a str,
    pub job: Option<&'a BackgroundJob>,
    /// The whole retained buffer, as the server holds it.
    pub output: &'a str,
    /// Lines up from the tail. 0 = pinned to the end, following live output.
    pub scroll: usize,
    pub width: usize,
    /// Rows this view may paint, header and footer included.
    pub height: usize,
    pub now: i64,
    /// Why there is no buffer on screen. Shown in place of the output.
    pub error: Option<&'a str>,
    /// `x` has armed a kill — the footer says what the next press does.
    pub armed: bool,
}

/// The status word, in the colour the rail and the transcript card already use.
fn status_text(job: &BackgroundJob, now: i64) -> String {
    let took = fmt_duration(job.exited_at.unwrap_or(now) - job.started_at);
    if job.status == JobStatus::Running {
        return format!("{} {}", warn("⋯ running"), dim(&format!("· {took}")));
    }
    // A signal leaves exitCode null; treating null as zero paints a killed
    // shell green.
    if let Some(signal) = &job.signal {
        return format!(
            "{} {}",
            warn(&format!("◼ stopped ({signal})")),
            dim(&format!("· ran {took}"))
        );
    }
    let code = job.exit_code.unwrap_or(0);
    let verdict = if code == 0 { accent("✓ done") } else { danger(&format!("✗ exit {code}")) };
    format!("{verdict} {}", dim(&format!("· ran {took}")))
}

/// The command line(s): id and pid, then the whole command wrapped to the
/// width. The command's own line breaks still collapse to `¶` — every row
/// emitted here must be exactly one row — but nothing is cut off sideways.
/// Capped at half the view so a pasted heredoc cannot squeeze the buffer out;
/// the cap is marked with an ellipsis.
pub fn job_sub_lines(
    job: Option<&BackgroundJob>,
    id: &str,
    width: usize,
    height: usize,
) -> Vec<String> {
    let Some(job) = job else { return vec![dim(id)] };
    let w = width.max(1);
    let lines = wrap_line(
        &format!("{} {}", dim(&format!("{id} · pid {} ·", job.pid)), one_line(&job.command)),
        w,
    );
    let cap = (((height as isize - 3) / 2).max(1)) as usize;
    if lines.len() <= cap {
        return lines;
    }
    let mut kept: Vec<String> = lines[..cap].to_vec();
    kept[cap - 1] = format!("{}{}", truncate_ansi(&kept[cap - 1], w.saturating_sub(2), "…"), dim(" …"));
    kept
}

/// The rows the buffer itself gets — the page step, and this view's own budget.
pub fn job_body_rows(height: usize, sub_rows: usize) -> usize {
    (height as isize - 3 - sub_rows as isize).max(1) as usize
}

/// The view's rows, ANSI-styled and each exactly `width` columns.
pub fn job_output_rows(p: &JobOutputProps) -> Vec<String> {
    let w = p.width.max(1);
    let name = one_line(if p.job.map(|j| j.name.is_empty()).unwrap_or(true) {
        p.id
    } else {
        &p.job.unwrap().name
    });
    let head = format!(
        "⚙ {}  {}",
        bold(&name),
        match p.job {
            Some(job) => status_text(job, p.now),
            None => dim("(job not found)"),
        }
    );
    let sub = job_sub_lines(p.job, p.id, w, p.height);
    let body = job_body_rows(p.height, sub.len());

    // Split on the RAW buffer rather than on wrapped rows: long lines are
    // truncated to the width instead of reflowed, so the row a scroll offset
    // addresses is the same row after a resize. A carriage return is not text
    // — it is a terminal telling the row to start over, which is how every
    // progress bar writes; what a terminal SHOWS is the last segment.
    let all: Vec<String> = match p.error {
        Some(error) => vec![danger(error)],
        None => p
            .output
            .trim_end_matches('\n')
            .split('\n')
            .map(|l| match l.rfind('\r') {
                Some(at) => l[at + 1..].to_string(),
                None => l.to_string(),
            })
            .collect(),
    };
    let lines: Vec<String> = if all.len() == 1 && all[0].is_empty() {
        vec![dim(if p.job.map(|j| j.status) == Some(JobStatus::Running) {
            "(no output yet)"
        } else {
            "(no output)"
        })]
    } else {
        all
    };
    let max = lines.len().saturating_sub(body);
    let at = p.scroll.min(max);
    // `scroll` counts up from the tail, so the window is measured back from
    // the end.
    let end = lines.len() - at;
    let rows = &lines[end.saturating_sub(body)..end];
    let pad = body.saturating_sub(rows.len());

    let behind = if at > 0 {
        format!("{at} line{} below · ", if at == 1 { "" } else { "s" })
    } else {
        String::new()
    };
    let footer = if p.armed {
        format!("{} {}", warn("x again kills it"), dim("· esc cancels"))
    } else {
        dim(&format!(
            "{behind}{} line{} · ↑↓ scroll · {}esc back",
            lines.len(),
            if lines.len() == 1 { "" } else { "s" },
            if p.job.map(|j| j.status) == Some(JobStatus::Running) { "x stop · " } else { "" },
        ))
    };

    let mut out: Vec<String> = vec![pad_row_ansi(&truncate_ansi(&head, w, "…"), w)];
    out.extend(sub.iter().map(|s| pad_row_ansi(&truncate_ansi(s, w, "…"), w)));
    out.extend((0..pad).map(|_| pad_row_ansi(" ", w)));
    out.extend(rows.iter().map(|r| {
        pad_row_ansi(&truncate_ansi(if r.is_empty() { " " } else { r }, w, "…"), w)
    }));
    out.push(pad_row_ansi(" ", w));
    out.push(pad_row_ansi(&truncate_ansi(&footer, w, "…"), w));
    out
}

pub fn render_job_output(p: &JobOutputProps, area: Rect, buf: &mut Buffer) {
    for (i, row) in job_output_rows(p).iter().take(area.height as usize).enumerate() {
        let line: Line = line_from_ansi(row);
        buf.set_line(area.x, area.y + i as u16, &line, area.width);
    }
}

// ---------------------------------------------------------------------------
// Tests — ported from src/tui/components/JobOutput.test.ts
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ansi::{strip_ansi, width};

    fn job(command: &str) -> BackgroundJob {
        BackgroundJob {
            id: "job-1".into(),
            name: "dev server".into(),
            session_id: "s1".into(),
            pid: 4242,
            command: command.into(),
            status: JobStatus::Running,
            exit_code: None,
            signal: None,
            started_at: 1_700_000_000_000,
            exited_at: None,
        }
    }

    fn props<'a>(job: Option<&'a BackgroundJob>, output: &'a str) -> JobOutputProps<'a> {
        JobOutputProps {
            id: "job-1",
            job,
            output,
            scroll: 0,
            width: 60,
            height: 12,
            now: 1_700_000_060_000,
            error: None,
            armed: false,
        }
    }

    #[test]
    fn a_short_command_stays_on_one_row() {
        let lines = job_sub_lines(Some(&job("bun test")), "job-1", 80, 20);
        assert_eq!(lines.len(), 1);
        assert!(strip_ansi(&lines[0]).contains("job-1 · pid 4242 · bun test"), "{:?}", lines[0]);
    }

    #[test]
    fn a_long_command_wraps_instead_of_being_cut_off() {
        let cmd = "bun run build --target=node --minify --outdir dist && rsync -av dist/ deploy@host:/srv/app/";
        let lines = job_sub_lines(Some(&job(cmd)), "job-1", 40, 20);
        assert!(lines.len() > 1);
        for l in &lines {
            assert!(width(l) <= 40, "row overflows: {}", strip_ansi(l));
        }
        // Nothing was lost: the rows joined back together hold the command.
        let joined: String = lines.iter().map(|l| strip_ansi(l)).collect();
        assert!(joined.replace([' ', '\n'], "").contains("deploy@host:/srv/app/"), "{joined}");
    }

    #[test]
    fn a_huge_command_is_capped_at_half_the_view_with_an_ellipsis() {
        let lines = job_sub_lines(Some(&job(&"x".repeat(4000))), "job-1", 40, 20);
        // height 20 → cap = floor((20 - 3) / 2) = 8.
        assert_eq!(lines.len(), 8);
        assert!(strip_ansi(&lines[7]).ends_with('…'));
        for l in &lines {
            assert!(width(l) <= 40, "{}", strip_ansi(l));
        }
    }

    #[test]
    fn body_budget_shrinks_by_the_rows_the_command_takes() {
        assert_eq!(job_body_rows(20, 1), 16); // one sub row — the old height - 4
        assert_eq!(job_body_rows(20, 3), 14);
        assert_eq!(job_body_rows(4, 8), 1); // never less than one row
    }

    #[test]
    fn the_view_paints_exactly_its_height_and_hangs_the_output_from_the_bottom() {
        let j = job("bun test");
        let output = (0..40).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let rows = job_output_rows(&props(Some(&j), &output));
        assert_eq!(rows.len(), 12);
        let plain: Vec<String> = rows.iter().map(|r| strip_ansi(r)).collect();
        assert!(plain[0].contains("⚙ dev server"), "{:?}", plain[0]);
        assert!(plain[0].contains("⋯ running"), "{:?}", plain[0]);
        // The tail is what it shows by default.
        assert!(plain[rows.len() - 3].trim_end().ends_with("line 39"), "{plain:?}");
        assert!(plain.last().unwrap().contains("40 lines · ↑↓ scroll · x stop · esc back"));
    }

    #[test]
    fn scroll_counts_up_from_the_tail_and_says_how_much_is_below() {
        let j = job("bun test");
        let output = (0..40).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let rows = job_output_rows(&JobOutputProps { scroll: 5, ..props(Some(&j), &output) });
        let plain: Vec<String> = rows.iter().map(|r| strip_ansi(r)).collect();
        assert!(plain[rows.len() - 3].trim_end().ends_with("line 34"), "{plain:?}");
        assert!(plain.last().unwrap().contains("5 lines below · 40 lines"), "{plain:?}");
    }

    #[test]
    fn a_progress_bar_rewrite_shows_its_last_segment_only() {
        let j = job("bun test");
        let rows = job_output_rows(&props(Some(&j), "10%\r50%\r100%\ndone"));
        let plain: Vec<String> = rows.iter().map(|r| strip_ansi(r).trim_end().to_string()).collect();
        assert!(plain.contains(&"100%".to_string()), "{plain:?}");
        assert!(!plain.iter().any(|l| l.contains("10%50%")), "{plain:?}");
        assert!(plain.contains(&"done".to_string()), "{plain:?}");
    }

    #[test]
    fn an_empty_buffer_says_which_kind_of_empty_it_is() {
        let mut j = job("bun test");
        let running = job_output_rows(&props(Some(&j), ""));
        assert!(running.iter().any(|r| strip_ansi(r).contains("(no output yet)")));
        j.status = JobStatus::Exited;
        j.exit_code = Some(0);
        j.exited_at = Some(1_700_000_030_000);
        let exited = job_output_rows(&props(Some(&j), ""));
        assert!(exited.iter().any(|r| strip_ansi(r).contains("(no output)")));
        // …and the footer drops the stop it can no longer offer.
        let footer = strip_ansi(exited.last().unwrap());
        assert!(!footer.contains("x stop"), "{footer}");
    }

    #[test]
    fn a_signal_leaves_a_null_exit_code_and_must_not_paint_the_shell_green() {
        let mut j = job("sleep 500");
        j.status = JobStatus::Exited;
        j.signal = Some("SIGTERM".into());
        j.exit_code = None;
        j.exited_at = Some(1_700_000_030_000);
        let head = strip_ansi(&job_output_rows(&props(Some(&j), "x"))[0]);
        assert!(head.contains("◼ stopped (SIGTERM) · ran 30s"), "{head}");
        assert!(!head.contains("✓ done"), "{head}");
    }

    #[test]
    fn the_armed_footer_says_what_the_next_press_does_and_how_to_back_out() {
        let j = job("bun test");
        let rows = job_output_rows(&JobOutputProps { armed: true, ..props(Some(&j), "x") });
        let footer = strip_ansi(rows.last().unwrap());
        assert!(footer.starts_with("x again kills it · esc cancels"), "{footer}");
    }

    #[test]
    fn a_job_whose_row_failed_to_load_still_has_an_id_and_an_error() {
        let rows = job_output_rows(&JobOutputProps {
            error: Some("job not found"),
            ..props(None, "")
        });
        let plain: Vec<String> = rows.iter().map(|r| strip_ansi(r)).collect();
        assert!(plain[0].contains("⚙ job-1"), "{:?}", plain[0]);
        assert!(plain[0].contains("(job not found)"), "{:?}", plain[0]);
        assert!(plain.iter().any(|l| l.contains("job not found")), "{plain:?}");
    }
}
