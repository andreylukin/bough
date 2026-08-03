/**
 * One background job, opened: its header and its whole retained buffer.
 *
 * WHY THIS EXISTS. A background job was a rail row and nothing else. The row told
 * you a shell called `dev server` had been running for four minutes; what it had
 * PRINTED was reachable only by asking the model to call `bashOutput` and reading
 * the answer back through a round of the LLM — a spinner, a token bill and a wait,
 * to see text the server already had in memory. `⏎` on the rail row opens this
 * instead. `GET /sessions/:id/jobs/:jobId/output` is deliberately non-destructive
 * (`server/jobs.ts`), so watching a job never eats output the next round was owed.
 *
 * THE INVARIANT THIS HOLDS: **presentational only.** The buffer, the row and the
 * scroll offset are props; the fetch and the poll belong to the composition root,
 * which already owns every other clock in the TUI.
 *
 * The tail is what it shows by default — `scroll` counts lines UP from the last
 * one, the same direction the transcript scrolls, because the interesting end of a
 * running build is the end that is still moving.
 */
import type { BackgroundJob } from "../../schema/parts.ts";
import { accent, bold, danger, dim, fmtDuration, oneLine, warn } from "../format.ts";
import { MessageRow, padRow } from "./Message.tsx";

export interface JobOutputProps {
  /** The id, kept separate: a job whose row failed to load still has one. */
  id: string;
  job: BackgroundJob | null;
  /** The whole retained buffer, as the server holds it. */
  output: string;
  /** Lines up from the tail. 0 = pinned to the end, following live output. */
  scroll?: number;
  width: number;
  /** Rows this view may paint, header and footer included. */
  height: number;
  now: number;
  /** Why there is no buffer on screen. Shown in place of the output. */
  error?: string | null;
  /** `x` has armed a kill for this job — the footer says what the next press does. */
  armed?: boolean;
}

/** The status word, in the colour the rail and the transcript card already use. */
function statusText(job: BackgroundJob, now: number): string {
  const took = fmtDuration((job.exitedAt ?? now) - job.startedAt);
  if (job.status === "running") return warn("⋯ running") + " " + dim("· " + took);
  // A signal leaves exitCode null; treating null as zero paints a killed shell green.
  if (job.signal) return warn("◼ stopped (" + job.signal + ")") + " " + dim("· ran " + took);
  const code = job.exitCode ?? 0;
  const verdict = code === 0 ? accent("✓ done") : danger("✗ exit " + code);
  return verdict + " " + dim("· ran " + took);
}

export function JobOutput(
  { id, job, output, scroll = 0, width, height, now, error, armed }: JobOutputProps,
) {
  const w = Math.max(1, width);
  // Header, the command line, a blank, and the footer. The body takes what is left,
  // and never less than one row: a claim about available space that is false is how
  // six rows came to be painted into three elsewhere in this tree.
  const body = jobBodyRows(height);
  // Both of these are ONE row of a fixed-height box, and a job's command is very
  // often several lines (a `for` loop, a heredoc) — see `oneLine`.
  const name = oneLine(job?.name || id);
  const head = `⚙ ${bold(name)}  ${job ? statusText(job, now) : dim("(job not found)")}`;
  const sub = job ? `${dim(`${id} · pid ${job.pid} ·`)} ${oneLine(job.command)}` : dim(id);

  // Split on the raw buffer rather than on wrapped rows: long lines are truncated to
  // the width instead of reflowed, so a column of build output stays a column and the
  // row a scroll offset addresses is the same row after a resize.
  // A carriage return is not text — it is a terminal telling the row to start over,
  // which is how every progress bar and every `npm` spinner writes. What a terminal
  // shows is the LAST segment, so that is what a row here is; keeping the whole thing
  // would paint a build's entire progress history as one unreadable line.
  const all = error
    ? [danger(error)]
    : output.replace(/\n+$/, "").split("\n").map((l) => l.slice(l.lastIndexOf("\r") + 1));
  const lines = all.length === 1 && all[0] === ""
    ? [dim(job?.status === "running" ? "(no output yet)" : "(no output)")]
    : all;
  const max = Math.max(0, lines.length - body);
  const at = Math.min(max, Math.max(0, scroll));
  // `scroll` counts up from the tail, so the window is measured back from the end.
  const end = lines.length - at;
  const rows = lines.slice(Math.max(0, end - body), end);
  const pad = Math.max(0, body - rows.length);

  const behind = at > 0 ? `${at} line${at === 1 ? "" : "s"} below · ` : "";
  const footer = armed
    ? `${warn("x again kills it")} ${dim("· esc cancels")}`
    : dim(
      `${behind}${lines.length} line${lines.length === 1 ? "" : "s"} · ↑↓ scroll · ` +
        `${job?.status === "running" ? "x stop · " : ""}esc back`,
    );

  return (
    <box flexDirection="column">
      <MessageRow line={{ text: head }} width={w} />
      <MessageRow line={{ text: sub }} width={w} />
      {Array.from({ length: pad }, (_, i) => (
        <text key={`pad${i}`} wrapMode="none" content={padRow(" ", w)} />
      ))}
      {rows.map((text, i) => (
        <MessageRow key={`row${i}`} line={{ text: text || " " }} width={w} />
      ))}
      <text wrapMode="none" content={padRow(" ", w)} />
      <MessageRow line={{ text: footer }} width={w} />
    </box>
  );
}

/** The rows the buffer itself gets — the page step, and this view's own budget. */
export function jobBodyRows(height: number): number {
  return Math.max(1, height - 4);
}
