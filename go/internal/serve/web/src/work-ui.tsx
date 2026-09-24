import { Fragment, createContext, useCallback, useContext, useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { api } from "./api";
import { elapsed } from "./loading";
import { anchorPlace } from "./popover";
import { Markdown, duration, execNote, lineCount, plainTitle } from "./render";
import type { Line, Row } from "./types";
import { jobSummaryLine, jobTitle, jobsFromLines, parseLegacyJob, useReviewed, workCounts, workElapsedMs, workSummaryText, type WorkCounts, type WorkKind, type WorkLife, type Worker } from "./work";

/*
 * The Work surfaces: one state word for every worker, the Stop button,
 * job rows, the header's Work button and its dialog. They all read the
 * one work index a thread builds (work.ts), through WorkContext.
 */

export const LIFE_WORD: Record<WorkLife, string> = {
  running: "Running", queued: "Queued", failed: "Failed", finished: "Finished", stopped: "Stopped", unknown: "Outcome unknown",
};
const KIND_WORD: Record<WorkKind, string> = { job: "Job", subagent: "Subagent", agent: "Background agent" };

/** "Failed · exit 1": the exit only when it was recorded. */
export function stateText(w: Pick<Worker, "life" | "exit">): string {
  return w.life === "failed" && w.exit !== undefined ? `Failed · exit ${w.exit}` : LIFE_WORD[w.life];
}

/** A worker's state as a glyph: the colour lives here, the word stays neutral. Decorative; the word is what is read. */
export function WorkGlyph({ life, size = 14 }: { life: WorkLife; size?: number }) {
  const svg = { width: size, height: size, viewBox: "0 0 24 24", fill: "none", "aria-hidden": true as const, "data-life": life };
  switch (life) {
    case "running":
      return <svg {...svg} className="work-glyph spin-mark" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round"><circle cx="12" cy="12" r="8.5" strokeDasharray="40 14" /></svg>;
    case "queued":
      return <svg {...svg} className="work-glyph" stroke="currentColor" strokeWidth="3"><circle cx="12" cy="12" r="8" /></svg>;
    case "failed":
      return <svg {...svg} className="work-glyph"><circle cx="12" cy="12" r="10" fill="currentColor" /><path d="M12 7v6.5M12 17h.01" stroke="#fff" strokeWidth="2.6" strokeLinecap="round" /></svg>;
    case "finished":
      return <svg {...svg} className="work-glyph" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round"><path d="M4 12.5l5 5L20 6.5" /></svg>;
    case "stopped":
      return <svg {...svg} className="work-glyph"><rect x="5" y="5" width="14" height="14" rx="2" fill="currentColor" /></svg>;
    default:
      return <svg {...svg} className="work-glyph" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round"><path d="M8.5 9a3.5 3.5 0 1 1 5 3.2c-1 .5-1.5 1.2-1.5 2.3M12 18.5h.01" /></svg>;
  }
}

/** A glyph and a word; colour only on the glyph, and on the word "Failed". */
export function WorkState({ w }: { w: Pick<Worker, "life" | "exit"> }) {
  return (
    <span className="work-state" data-life={w.life}>
      <WorkGlyph life={w.life} size={12} /><span className="work-word">{LIFE_WORD[w.life]}</span>{w.life === "failed" && w.exit !== undefined && <span className="work-exit"> · exit {w.exit}</span>}
    </span>
  );
}

/** 205000 → "3 minutes 25 seconds", for a label read aloud. */
export function spokenDuration(ms: number): string {
  const s = Math.round(ms / 1000);
  const part = (n: number, u: string) => (n ? [`${n} ${u}${n === 1 ? "" : "s"}`] : []);
  const out = [...part(Math.floor(s / 3600), "hour"), ...part(Math.floor(s / 60) % 60, "minute"), ...part(s % 60, "second")];
  return out.join(" ") || "under a second";
}

/* ---------------- execution notes ---------------- */

const NOTE_RE = /\n*\[(?:\d+ further code block\(s\) dropped|the \d+ code block\(s\) after this one)[^\]]*\]/;
const STOP_RE = /\n*\[stop block dropped[^\]]*\]/;

/** Text without the loop's "blocks not run" or "stop dropped" note, and the note on its own. */
export function splitExecNote(text: string): { text: string; note: { notRun: number; reason: string } | null } {
  const note = execNote(text);
  if (note) return { text: text.replace(NOTE_RE, "").trimEnd(), note };
  // notRun 0 marks the loop's stop-dropped note: a system annotation, not prose.
  if (STOP_RE.test(text)) return { text: text.replace(STOP_RE, "").trimEnd(), note: { notRun: 0, reason: "stop" } };
  return { text, note: null };
}

/**
 * "2 later code blocks skipped: the block before them failed": a notice,
 * never a paragraph of the reply. The loop runs every block but stops at the
 * first failure, so the reason always shows: "not run" alone read as if
 * blocks were being dropped for no reason.
 */
export function ExecNote({ note }: { note: { notRun: number; reason: string } }) {
  const n = note.notRun;
  if (n === 0) {
    return <p className="exec-note exec-note-quiet">Early stop ignored: the agent tried to finish before its code ran, so the turn kept going</p>;
  }
  const why = /failed/.test(note.reason) ? "the block before them failed" : note.reason;
  return (
    <p className="exec-note">
      {n} later code {n === 1 ? "block" : "blocks"} skipped{why ? `: ${why}` : ""}
    </p>
  );
}

/* ---------------- context ---------------- */

type StopEntry = { state: "stopping" | "timeout" | "error"; fromWork?: boolean };

export interface WorkCtx {
  session: string;
  live: boolean;
  workers: Worker[];
  byKey: Map<string, Worker>;
  /** Job workers by id, and the seq of the first transcript line for each: the row renders there once. */
  jobs: Map<string, Worker>;
  jobFirst: Map<string, number>;
  review: ReturnType<typeof useReviewed>;
  stops: Record<string, StopEntry>;
  requestStop: (w: Worker, fromWork?: boolean) => void;
  /** Opens the Work dialog: a turn footer's "calls still running" leads there. */
  openWork?: () => void;
}

export const WorkContext = createContext<WorkCtx | null>(null);
export const useWork = () => useContext(WorkContext);

/** A job line's id: typed entries and the loop's outcome notices; a "matched while running" notice is not one. */
export function jobIdOf(l: Line): number | undefined {
  const d = l.data ?? {};
  if (typeof d.event === "string" && typeof d.id === "number") return d.id;
  const p = parseLegacyJob(String(d.text ?? l.text ?? ""));
  return p && p.id !== undefined && p.life !== "running" ? p.id : undefined;
}

/** "[agent title · id word] reply": a child's report to its parent, the only completion payload the parent holds. */
export function agentReports(lines: Line[]): Map<string, string> {
  const out = new Map<string, string>();
  for (const l of lines) {
    const m = /^\[agent .* · (\S+) \w+\] ([\s\S]*)$/.exec(l.text ?? "");
    const reply = m?.[2].replace(/^Background agent failed: .*\n?/, "").trim();
    if (m && reply) out.set(m[1], reply);
  }
  return out;
}

/**
 * Stops asked for, per worker key. A request that was accepted says
 * "Stopping…" until the worker's own record ends it (a natural outcome
 * wins), or 15s pass without one.
 */
export function useStopStore(byKey: Map<string, Worker>, review: ReturnType<typeof useReviewed>, onStopped?: () => void) {
  const [stops, setStops] = useState<Record<string, StopEntry>>({});
  const timers = useRef(new Map<string, ReturnType<typeof setTimeout>>());
  const requestStop = useCallback((w: Worker, fromWork?: boolean) => {
    setStops((m) => ({ ...m, [w.key]: { state: "stopping", fromWork } }));
    clearTimeout(timers.current.get(w.key));
    timers.current.set(w.key, setTimeout(() => setStops((m) => (m[w.key]?.state === "stopping" ? { ...m, [w.key]: { ...m[w.key], state: "timeout" } } : m)), 15_000));
    const req = w.kind === "job" ? api.killJob(w.session, Number(w.id)) : api.stopAgent(w.id);
    // A stopped queued agent leaves no process and no event behind: only
    // a fresh read of the agents shows it gone.
    req.then(() => onStopped?.(), () => {});
    req.catch(() => {
      clearTimeout(timers.current.get(w.key));
      setStops((m) => ({ ...m, [w.key]: { state: "error", fromWork } }));
    });
  }, [onStopped]);
  // The worker's own record ended it: the stop is over, whatever the outcome.
  useEffect(() => {
    const done = Object.entries(stops).filter(([k, s]) => s.state !== "error" && byKey.get(k) && byKey.get(k)!.life !== "running" && byKey.get(k)!.life !== "queued");
    if (!done.length) return;
    for (const [k, s] of done) {
      clearTimeout(timers.current.get(k));
      if (s.fromWork) review.markReviewed(byKey.get(k)!);
    }
    setStops((m) => { const next = { ...m }; for (const [k] of done) delete next[k]; return next; });
  }, [byKey, stops, review]);
  useEffect(() => () => timers.current.forEach(clearTimeout), []);
  return { stops, requestStop };
}

/** Stop for a running worker whose own session is live. One click, no confirm. */
export function StopWorkButton({ w, fromWork }: { w: Worker; fromWork?: boolean }) {
  const ctx = useWork();
  const [local, setLocal] = useState<StopEntry | undefined>();
  const entry = ctx ? ctx.stops[w.key] : local;
  const name = w.kind === "job" ? `Job ${w.id}` : "agent";
  if (!entry && !w.canStop) return null;
  const stop = (e: React.MouseEvent) => {
    e.stopPropagation();
    if (ctx) ctx.requestStop(w, fromWork);
    else {
      setLocal({ state: "stopping" });
      (w.kind === "job" ? api.killJob(w.session, Number(w.id)) : api.stopAgent(w.id)).catch(() => setLocal({ state: "error" }));
    }
  };
  if (entry?.state === "error") {
    const why = w.kind === "job" ? `Couldn't stop Job ${w.id}.` : "Couldn't stop this background agent.";
    return (
      <button type="button" className="stop-work" data-state="error" aria-label={`${why} Try stop again`} title={why} onClick={stop}>
        Retry
      </button>
    );
  }
  if (entry?.state === "timeout") {
    const full = "Stop requested · status unavailable";
    return <button type="button" className="stop-work" data-state="timeout" disabled aria-label={full} title={full}>No reply</button>;
  }
  if (entry) {
    return (
      <button type="button" className="stop-work" data-state="stopping" disabled aria-label="Stopping…">
        <svg width={10} height={10} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round" className="spin-mark" aria-hidden="true"><circle cx="12" cy="12" r="8.5" strokeDasharray="40 14" /></svg>Stopping
      </button>
    );
  }
  return (
    <button type="button" className="stop-work" data-state="idle" aria-label={w.kind === "job" ? `Stop ${name}` : "Stop agent"} onClick={stop}>
      <span className="stop-sq" aria-hidden="true" />Stop
    </button>
  );
}

/** A labelled copy, for the command a job ran. */
export function CopyCommand({ text, label = "Copy command" }: { text: string; label?: string }) {
  const [done, setDone] = useState(false);
  useEffect(() => {
    if (!done) return;
    const t = setTimeout(() => setDone(false), 1400);
    return () => clearTimeout(t);
  }, [done]);
  return (
    <button type="button" className="btn" onClick={() => navigator.clipboard?.writeText(text).then(() => setDone(true), () => {})}>
      {done ? "Copied" : label}
    </button>
  );
}

/* ---------------- job rows ---------------- */

/** What a job's output section says when there is no output to show. */
function outputWords(w: Worker): string | null {
  if (w.outputState === "recorded") return null;
  if (w.life === "running") return "No output yet.";
  if (w.outputState === "empty") return "Exited with no output.";
  return "Output not recorded.";
}

/** The line under a job that did not simply finish: its exit note, else the last real line it printed, with the untouched line to hover. */
function jobCause(w: Worker): { text: string; full?: string } {
  if (w.exitNote) return { text: w.exitNote };
  if (w.life !== "failed") return { text: "" };
  return jobSummaryLine(w.output ?? "") ?? { text: "" };
}

/** Identity, command, outcome: one status-first row per job, opening onto the command and its output. */
export function JobRow({ w }: { w: Worker }) {
  const ctx = useWork();
  const failed = w.life === "failed";
  const cause = jobCause(w);
  const cmd = w.cmd ?? "";
  const empty = outputWords(w);
  const lines = w.output ? w.output.split("\n").length : 0;
  const now = useTick(w.life === "running" && w.ms === undefined && Boolean(w.startedAt));
  const took = w.ms !== undefined ? duration(w.ms) : workElapsedMs(w, now) !== undefined ? elapsed(workElapsedMs(w, now)!) : "";
  const aria = `Job ${w.id}, ${stateText(w)}${w.ms !== undefined ? `, ${spokenDuration(w.ms)}` : ""}${cmd ? `: ${cmd.slice(0, 80)}` : ""}`;
  return (
    // Stop is the details' sibling: inside a closed <details> it would be hidden with the body.
    <div className="job-wrap">
    <details className={"block thin job" + (failed ? " job-failed" : "")} data-work-key={w.key} data-open-key={w.key}>
      <summary className="job-summary" aria-label={aria}
               onClick={(e) => {
                 // Opening a failure's output is reading it.
                 const opening = !(e.currentTarget.parentElement as HTMLDetailsElement).open;
                 if (opening && w.life !== "running" && (w.output || w.error)) ctx?.review.markReviewed(w);
               }}>
        <span className="num sub-tag">Job {w.id}</span>
        <span className="job-cmd" title={cmd || undefined}>{cmd && jobTitle(cmd, w.id) !== `Job ${w.id}` ? jobTitle(cmd, w.id) : "Command not recorded"}</span>
        <WorkState w={w} />
        <span className="job-meta" data-ticking={w.ms === undefined && took ? "" : undefined}>{took}</span>
        <span className="job-action" data-stop={w.canStop || ctx?.stops[w.key] ? "" : undefined} aria-hidden="true" />
        {cause.text && <span className="sub-line2 job-cause" title={cause.full}>{cause.text}</span>}
      </summary>
      <div className="block-body job-body">
        <div className="job-sec">
          <span className="block-label">Command</span>
          {cmd ? <pre className="mono job-cmd-full">{cmd}</pre> : <p className="meta-line">Command not recorded.</p>}
          {cmd && <div className="fail-actions"><CopyCommand text={cmd} /></div>}
        </div>
        <div className="job-sec">
          <span className="block-label">{w.outputState === "recorded" ? `Output · ${lineCount(lines)}` : "Output"}</span>
          {empty ? <p className="meta-line">{empty}</p> : <pre className="mono job-out">{w.output}</pre>}
        </div>
      </div>
    </details>
    {/* Beside the summary, not in it: a button inside a <summary> toggles the row too. */}
    <div className="job-stop"><StopWorkButton w={w} /></div>
    </div>
  );
}

/** Once a second while on: the clock a running row's elapsed time reads. */
function useTick(on: boolean): number {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    if (!on) return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [on]);
  return now;
}

/** "JOBS  2 running · 3 failed": the head of a run of consecutive job rows; only the failures' dot is coloured. */
export function JobGroupHead({ workers }: { workers: Worker[] }) {
  const c = workCounts(workers);
  const parts = ([["running", c.running], ["queued", c.queued], ["failed", c.failed], ["unknown", c.unknown], ["stopped", c.stopped]] as const).filter(([, n]) => n);
  return (
    <div className="job-group-head">
      <span className="job-group-label">Jobs</span>
      <span className="job-group-counts">
        {parts.length
          ? parts.map(([word, n], i) => <Fragment key={word}>{i > 0 && <span className="work-sep"> · </span>}{word === "failed" && <span className="work-dot" aria-hidden="true" />}{n} {word}</Fragment>)
          : `${c.finished} finished`}
      </span>
    </div>
  );
}

/** The job lines of one stretch: a head when two or more rows show, then each row once, where the job first appears. */
export function JobLines({ lines, render }: { lines: Line[]; render: (l: Line) => React.ReactNode }) {
  const ctx = useWork();
  const shown = lines.map((l) => {
    const id = jobIdOf(l);
    if (id === undefined) return null;
    if (ctx && ctx.jobFirst.get(String(id)) !== l.seq) return null;
    return ctx?.jobs.get(String(id)) ?? jobsFromLines([l], "", false)[0] ?? null;
  }).filter((w): w is Worker => Boolean(w));
  return (
    <>
      {shown.length >= 2 && <JobGroupHead workers={shown} />}
      {lines.map((l) => <Fragment key={l.seq}>{render(l)}</Fragment>)}
    </>
  );
}

/* ---------------- children ---------------- */

export type ChildState = "idle" | "loading" | "ok" | "error";

/**
 * A session's direct background agents from /children, read again when
 * its counts or transcript move. Only a session that says it started
 * agents asks; the others have none to load.
 */
export function useChildren(row: Row, rows: Row[], tick: number): { children: Row[] | null; state: ChildState; retry: () => void; refresh: () => void } {
  const has = Boolean(row.agents?.total) || rows.some((r) => r.spawnedBy === row.id);
  const [children, setChildren] = useState<Row[] | null>(null);
  const [state, setState] = useState<ChildState>(has ? "loading" : "idle");
  const [nonce, setNonce] = useState(0);
  // A child that goes from starting to running changes no count: its list
  // row (status, and whether it has recorded anything) is what says it
  // did, or the dialog kept "Starting" for an agent long under way.
  const kidRows = rows.filter((r) => r.spawnedBy === row.id).map((r) => `${r.id}/${r.status}/${r.live}/${r.entries > 0}`).join(",");
  const sig = `${row.agents?.running ?? 0}:${row.agents?.queued ?? 0}:${row.agents?.total ?? 0}:${tick}:${kidRows}`;
  useEffect(() => {
    // No agents left (Stop and archive drops a queued one outright): the
    // last list must go too, or Work kept counting an agent that is gone.
    if (!has) { setChildren(null); setState("idle"); return; }
    let on = true;
    const t = setTimeout(() => {
      api.children(row.id)
        .then((c) => { if (on) { setChildren(c); setState("ok"); } })
        // A failed refresh keeps the last list; only a first failure is "unavailable".
        .catch(() => { if (on) setState((s) => (s === "ok" ? s : "error")); });
    }, 250);
    return () => { on = false; clearTimeout(t); };
  }, [row.id, has, sig, nonce]);
  const retry = useCallback(() => { setState("loading"); setNonce((n) => n + 1); }, []);
  // Read again, keeping the list on screen meanwhile.
  const refresh = useCallback(() => setNonce((n) => n + 1), []);
  return { children, state, retry, refresh };
}

/* ---------------- live region ---------------- */

/** Transitions after the first load, batched over 750ms into one polite line. Never timers. */
export function useWorkAnnouncer(workers: Worker[], ready: boolean): string {
  const prev = useRef<Map<string, WorkLife> | null>(null);
  const pending = useRef<string[]>([]);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const [msg, setMsg] = useState("");
  useEffect(() => {
    if (!ready) return;
    const was = prev.current;
    prev.current = new Map(workers.map((w) => [w.key, w.life]));
    if (!was) return;
    for (const w of workers) {
      const before = was.get(w.key);
      if (before === w.life) continue;
      if (w.life === "running" && before !== "queued") continue;
      if (before === undefined && w.life === "queued") continue;
      pending.current.push(`${w.kind === "agent" ? plainTitle(w.label) : w.label} ${w.life === "running" ? "started" : LIFE_WORD[w.life].toLowerCase()}`);
    }
    if (!pending.current.length || timer.current) return;
    timer.current = setTimeout(() => {
      timer.current = undefined;
      setMsg(pending.current.join(" · "));
      pending.current = [];
    }, 750);
  }, [workers, ready]);
  useEffect(() => () => clearTimeout(timer.current), []);
  return msg;
}

/* ---------------- Work button + dialog ---------------- */

export function WorkButton({ counts, loading, unavailable, paused, narrow, expanded, onClick, btnRef }: {
  counts: WorkCounts; loading?: boolean; unavailable?: boolean; paused?: boolean; narrow?: boolean; expanded: boolean;
  onClick: () => void; btnRef?: React.Ref<HTMLButtonElement>;
}) {
  const t = workSummaryText(counts, { loading, unavailable, paused, narrow });
  // One line: a phone's failures show as the dot alone; the label keeps the words.
  const glyph = counts.running ? <WorkGlyph life="running" size={12} />
    : counts.failed ? <span className="work-dot" aria-hidden="true" /> : null;
  return (
    <button type="button" ref={btnRef} className="work-summary" aria-haspopup="dialog" aria-expanded={expanded} aria-label={t.aria} onClick={onClick}>
      {glyph && <span className="work-summary-glyph">{glyph}</span>}
      <span className="work-summary-1">{t.primary.split(" · ").map((part, i) => <Fragment key={i}>{i > 0 && <span className="work-sep"> · </span>}{part}</Fragment>)}</span>
    </button>
  );
}

type Group = "review" | "running" | "queued" | "history";
const GROUP_WORD: Record<Group, string> = { review: "Needs review", running: "Running", queued: "Queued", history: "Finished" };
/** Finished rows shown before "Show all". */
const FINISHED_SHOWN = 10;
type Filter = "all" | WorkKind;
const FILTER_WORD: Record<Filter, string> = { all: "All", subagent: "Subagents", job: "Jobs", agent: "Agents" };

const when = (w: Worker) => Date.parse(w.endedAt ?? w.startedAt ?? "") || w.seq;

/**
 * Work, as a modal dialog: a popover over the thread on a wide pane, a
 * bottom sheet on a phone. Rows keep their group while pointed at,
 * focused or open, and regroup once they are let go.
 */
export function WorkDialog({ workers, sheet, anchor, childState, onRetryChildren, paused, parent, onClose, onView, onOpenAgent }: {
  workers: Worker[]; sheet: boolean; /** The Work button: the popover opens under it, inside its pane. */ anchor?: React.RefObject<HTMLElement | null>; childState: ChildState; onRetryChildren: () => void; paused?: boolean;
  /** The session whose Work this is: an agent's details are read on its behalf. */
  parent: string;
  /** refocus: give focus back to the Work button. */
  onClose: (refocus: boolean) => void;
  onView: (w: Worker) => void; onOpenAgent: (w: Worker) => void;
}) {
  const ctx = useWork();
  const ref = useRef<HTMLDivElement>(null);
  const head = useRef<HTMLHeadingElement>(null);
  const titleId = useId();
  const [filter, setFilter] = useState<Filter>("all");
  const [openKey, setOpenKey] = useState<string | null>(null);
  const [pins, setPins] = useState<Record<string, Group>>({});
  const [allFinished, setAllFinished] = useState(false);
  // Finished folds while anything is live; opened or closed by hand, it stays that way.
  const [finishedOpen, setFinishedOpen] = useState<boolean | null>(null);
  // Running rows tick their elapsed time.
  const [now, setNow] = useState(Date.now());
  const ticking = workers.some((w) => w.life === "running" && w.ms === undefined && w.startedAt);
  useEffect(() => {
    if (!ticking) return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [ticking]);

  // Focus lands on the heading; Escape closes; the page behind is inert.
  useLayoutEffect(() => { head.current?.focus(); }, []);
  // Fixed against the button's rect, kept inside the pane and the window;
  // the body scrolls inside the height left, so Close is never cut off.
  const [place, setPlace] = useState<React.CSSProperties>();
  useLayoutEffect(() => {
    if (sheet) return;
    const at = () => {
      const el = ref.current, b = anchor?.current;
      if (!el || !b) return;
      const t = b.getBoundingClientRect(), pane = b.closest(".thread")?.getBoundingClientRect();
      const p = anchorPlace(t, el.offsetWidth, el.scrollHeight,
        { left: Math.max(0, pane?.left ?? 0), right: Math.min(innerWidth, pane?.right ?? innerWidth), top: 0, bottom: innerHeight }, "end");
      setPlace({ position: "fixed", top: p.top, left: p.left, insetInlineEnd: "auto", maxHeight: Math.min(560, p.maxHeight) });
    };
    at();
    addEventListener("resize", at);
    return () => removeEventListener("resize", at);
  }, [sheet, anchor, workers.length]);
  // A layout effect: the page is un-inerted in the same commit that closes
  // the dialog, so focus can move to the transcript right after.
  useLayoutEffect(() => {
    const key = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault(); e.stopPropagation();
      onClose(true);
    };
    window.addEventListener("keydown", key, true);
    const marked: HTMLElement[] = [];
    for (let el: HTMLElement | null = ref.current; el?.parentElement && el !== document.body; el = el.parentElement) {
      for (const sib of el.parentElement.children) {
        if (sib !== el && sib instanceof HTMLElement && !sib.inert) { sib.inert = true; marked.push(sib); }
      }
    }
    return () => { window.removeEventListener("keydown", key, true); marked.forEach((m) => { m.inert = false; }); };
  }, [onClose]);
  const trap = (e: React.KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== "Tab") return;
    const all = [...e.currentTarget.querySelectorAll<HTMLElement>("button:not([disabled]), a[href], [tabindex='0']")].filter((x) => x.offsetParent);
    if (!all.length) return;
    const first = all[0], last = all[all.length - 1], here = document.activeElement;
    if (e.shiftKey && (here === first || here === head.current)) { e.preventDefault(); last.focus(); }
    else if (!e.shiftKey && here === last) { e.preventDefault(); first.focus(); }
  };

  const review = ctx?.review;
  const groupOf = (w: Worker): Group => {
    if (w.life === "running") return "running";
    if (w.life === "queued") return "queued";
    if (review?.isReviewed(w)) return "history";
    // Old history is not a to-do list: a long session opened days later
    // put all 58 of its failed jobs under Needs review. Only terminal work
    // from the last day asks for review; older records sit in History.
    const ended = w.endedAt ? Date.parse(w.endedAt) : NaN;
    const recent = Number.isFinite(ended) && Date.now() - ended < 86_400_000;
    const needs = (recent && (w.life === "failed" || w.life === "stopped" || w.life === "unknown" || (w.kind === "agent" && Boolean(w.result)))) || Boolean(review?.isNew(w));
    return needs ? "review" : "history";
  };
  const pin = (w: Worker) => setPins((p) => (p[w.key] ? p : { ...p, [w.key]: groupOf(w) }));
  const unpin = (w: Worker, el: HTMLElement) => {
    if (openKey === w.key || el.matches(":hover") || el.contains(document.activeElement)) return;
    setPins((p) => { if (!p[w.key]) return p; const { [w.key]: _, ...rest } = p; return rest; });
  };

  const kinds = (["subagent", "job", "agent"] as const).filter((k) => workers.some((w) => w.kind === k));
  const shown = workers.filter((w) => filter === "all" || w.kind === filter);
  const groups = new Map<Group, Worker[]>([["review", []], ["running", []], ["queued", []], ["history", []]]);
  for (const w of shown) groups.get(pins[w.key] ?? groupOf(w))!.push(w);
  for (const [g, list] of groups) list.sort((a, b) => (g === "history" || g === "review" ? when(b) - when(a) : when(a) - when(b)));
  const agents = workers.filter((w) => w.kind === "agent");

  return (
    <div ref={ref} className={sheet ? "work-sheet" : "work-popover"} role="dialog" aria-modal="true" aria-labelledby={titleId}
         style={!sheet ? place ?? (anchor ? { visibility: "hidden" } : undefined) : undefined} onKeyDown={trap}>
      {sheet && <div className="work-grabber" aria-hidden="true" />}
      <header>
        <h2 id={titleId} ref={head} tabIndex={-1} className="work-title">Work <span className="work-count">{workers.length}</span></h2>
        {kinds.length >= 2 && (
          <div className="work-filters" role="group" aria-label="Show">
            {(["all", ...kinds] as Filter[]).map((f) => (
              <button key={f} type="button" aria-pressed={filter === f} onClick={() => setFilter(f)}>{FILTER_WORD[f]}</button>
            ))}
          </div>
        )}
        <button type="button" className="btn btn-ghost work-close" aria-label="Close" onClick={() => onClose(true)}>
          <svg width={14} height={14} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" aria-hidden="true"><path d="M6 6l12 12M18 6L6 18" /></svg>
        </button>
      </header>
      {paused && <p className="work-paused" role="status"><span className="work-dot work-dot-amber" aria-hidden="true" />Updates paused · Reconnecting…</p>}
      <div className="work-body" data-stops={shown.some((w) => (w.live || w.life === "queued") && (w.canStop || ctx?.stops[w.key])) ? "" : undefined}>
        {(filter === "all" || filter === "agent") && (childState === "loading" && !agents.length
          ? <p className="work-note work-loading" role="status"><WorkGlyph life="running" size={12} /><span>Loading background agents…</span></p>
          : childState === "error"
          ? <div className="work-note work-error" role="alert"><span>Couldn’t load agents</span><button type="button" className="btn btn-sm" onClick={onRetryChildren}>Retry</button></div>
          : filter === "agent" && childState === "ok" && !agents.length && <p className="work-note work-empty">No background agents in this session.</p>)}
        {!shown.length && childState !== "loading" && filter === "all" && <p className="work-note work-empty">No work recorded in this session.</p>}
        {[...groups].filter(([, list]) => list.length).map(([g, list]) => {
          const failedN = list.filter((w) => w.life === "failed").length;
          const fold = g === "history";
          const open = !fold || (finishedOpen ?? !(groups.get("review")!.length || groups.get("running")!.length || groups.get("queued")!.length));
          const label = <>
            {fold && <svg className="work-chev" width={12} height={12} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M9 6l6 6-6 6" /></svg>}
            <span className="work-group-name">{GROUP_WORD[g]}</span><span className="work-group-n">{list.length}</span>
            {/* The header chip counts "failed": the review head says how many of its rows those are, in the same word. */}
            {g === "review" && failedN > 0 && failedN < list.length && <span className="work-group-sub"><span className="work-dot" aria-hidden="true" />{failedN} failed</span>}
          </>;
          return (
          <section key={g} aria-labelledby={`${titleId}-${g}`}>
            <h3 className="work-group-head" id={`${titleId}-${g}`}>
              {fold ? <button type="button" className="work-group-toggle" aria-expanded={open} onClick={() => setFinishedOpen(!open)}>{label}</button> : label}
            </h3>
            {open && (fold && !allFinished ? list.slice(0, FINISHED_SHOWN) : list).map((w) => (
              <WorkRow key={w.key} w={w} now={now} parent={parent} inReview={g === "review"} inHistory={fold} open={openKey === w.key}
                       onToggle={(open) => {
                         setOpenKey(open ? w.key : null);
                         if (open) pin(w);
                         // Opening recorded result or error content is reading it; a placeholder is not.
                         if (open && (w.result || w.error || (w.kind === "job" && w.outputState === "recorded" && w.life !== "running"))) review?.markReviewed(w);
                       }}
                       onPin={() => pin(w)} onUnpin={(el) => unpin(w, el)}
                       onView={() => onView(w)} onOpenAgent={() => onOpenAgent(w)} />
            ))}
            {open && fold && !allFinished && list.length > FINISHED_SHOWN && (
              <button type="button" className="work-more" onClick={() => setAllFinished(true)}>Show {list.length - FINISHED_SHOWN} more</button>
            )}
          </section>
          );
        })}
      </div>
    </div>
  );
}

function WorkRow({ w, now, parent, inReview, inHistory, open, onToggle, onPin, onUnpin, onView, onOpenAgent }: {
  w: Worker; now: number; parent: string; inReview: boolean; inHistory: boolean; open: boolean; onToggle: (open: boolean) => void;
  onPin: () => void; onUnpin: (el: HTMLElement) => void; onView: () => void; onOpenAgent: () => void;
}) {
  const ctx = useWork();
  const previewId = useId();
  const fresh = ctx?.review.isNew(w);
  const task = w.kind === "job" ? (w.task ? jobTitle(w.task, w.id) : "") : firstLine(plainTitle(w.task));
  const jc = jobCause(w);
  const cause = w.life === "failed" ? firstLine(w.error ?? "") || jc.text : w.exitNote ?? "";
  const ms = workElapsedMs(w, now);
  // Never blank: an empty cell would collapse the column between rows.
  const took = w.ms !== undefined ? duration(w.ms) : ms !== undefined ? elapsed(ms) : "—";
  // A job's label already says "Job N"; the kind word only names the others.
  const kind = w.kind === "job" ? "" : KIND_WORD[w.kind];
  const stop = (e: React.SyntheticEvent) => e.stopPropagation();
  return (
    <div className="work-row" data-life={w.life} data-open={open ? "" : undefined} onPointerEnter={onPin} onPointerLeave={(e) => onUnpin(e.currentTarget)}
         onFocus={onPin} onBlur={(e) => { const el = e.currentTarget; requestAnimationFrame(() => onUnpin(el)); }}>
      <button type="button" className="work-row-head" aria-expanded={open} aria-controls={previewId} title={w.task || undefined}
              onClick={(e) => { stop(e); onToggle(!open); }}>
        <span className="work-row-glyph"><WorkGlyph life={w.life} /></span>
        <span className="work-row-task">
          {w.kind === "job"
            ? <><span className="work-row-job">{w.label} </span><span className="work-row-cmd">{task && task !== w.label ? task : "command not recorded"}</span></>
            : <span className="work-row-text">{w.kind === "agent" ? plainTitle(w.label) : task && task !== w.label ? `${w.label} · ${task}` : w.kind === "subagent" ? `${w.label} · task not recorded` : w.label}</span>}
          <svg className="work-chev" width={12} height={12} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M9 6l6 6-6 6" /></svg>
        </span>
      </button>
      <div className="work-row-meta">
        {[
          // In Finished the check says "Finished"; the meta starts at the kind.
          !(inHistory && w.life === "finished") && <span key="s" className="work-state" data-life={w.life}><span className="work-word">{w.starting ? "Starting" : LIFE_WORD[w.life]}</span>{w.life === "failed" && w.exit !== undefined && <span className="work-exit"> · exit {w.exit}</span>}</span>,
          kind && <span key="k">{kind}</span>,
          cause && <span key="c" className="work-cause">{cause}</span>,
          fresh && <span key="n" className="work-new"><span className="work-dot work-dot-accent" aria-hidden="true" />New</span>,
        ].filter(Boolean).map((node, i) => <Fragment key={i}>{i > 0 && <span className="work-sep"> · </span>}{node}</Fragment>)}
      </div>
      <span className="work-row-time" data-ticking={w.ms === undefined && ms !== undefined ? "" : undefined}>{took}</span>
      <div className="work-row-stop">{(w.live || w.life === "queued") && <StopWorkButton w={w} fromWork />}</div>
      {open && (
        <div className="work-row-preview" id={previewId}>
          <Preview w={w} parent={parent} />
          <div className="work-row-actions">
            {w.kind === "agent"
              ? <button type="button" className="btn btn-sm" onClick={(e) => { stop(e); onOpenAgent(); }}>Open agent session</button>
              : <button type="button" className="btn btn-sm" onClick={(e) => { stop(e); onView(); }}>View in transcript</button>}
            {inReview && <button type="button" className="btn btn-sm" onClick={(e) => { stop(e); ctx?.review.markReviewed(w); }}>Mark reviewed</button>}
          </div>
        </div>
      )}
    </div>
  );
}

function firstLine(t: string): string {
  return t.split("\n").find((x) => x.trim())?.trim() ?? "";
}

/** A row's preview: the result, the error, or the command and its output. */
function Preview({ w, parent }: { w: Worker; parent: string }) {
  if (w.kind === "job") {
    const empty = outputWords(w);
    return <>
      <span className="block-label">Command</span>
      <pre>{w.cmd || "Command not recorded."}</pre>
      {w.exitNote && <p className="meta-line">{w.exitNote}</p>}
      <span className="block-label">Output</span>
      {empty ? <p className="meta-line">{empty}</p> : <pre>{w.output}</pre>}
    </>;
  }
  if (w.kind === "agent") return <AgentPreview w={w} parent={parent} />;
  return <>
    {w.life === "failed" && <div className="work-fail">
      <span className="work-fail-head">Failure</span>
      {w.error ? <pre>{w.error}</pre> : <p className="meta-line">Failure details not recorded.</p>}
    </div>}
    {w.life === "unknown" && <p className="meta-line">The recorded history does not establish an outcome.</p>}
    {(w.life === "finished" || w.result) && <>
      <span className="block-label">Result</span>
      {w.result ? <Markdown text={w.result} /> : <p className="meta-line">Result not recorded</p>}
    </>}
    {w.life === "running" && <p className="meta-line">{w.recordedSteps ? `${w.recordedSteps} recorded ${w.recordedSteps === 1 ? "step" : "steps"} so far.` : "Waiting for the first recorded step."}</p>}
  </>;
}

/** A background agent's last reply, read from /agent when the preview opens. */
function AgentPreview({ w, parent }: { w: Worker; parent: string }) {
  const [state, setState] = useState<{ reply?: string; err?: boolean }>({});
  const [nonce, setNonce] = useState(0);
  useEffect(() => {
    let on = true;
    setState({});
    api.agent(w.id, parent).then((a) => { if (on) setState({ reply: a.reply ?? "" }); }, () => { if (on) setState({ err: true }); });
    return () => { on = false; };
  }, [w.id, parent, nonce]);
  const text = w.result || state.reply;
  return <>
    {w.task && w.task !== w.label && <p className="meta-line">{firstLine(w.task)}</p>}
    {w.life === "failed" && w.error && <div className="work-fail"><span className="work-fail-head">Failure</span><pre>{w.error}</pre></div>}
    {text ? <><span className="block-label">Result</span><Markdown text={text} /></>
      : state.err ? <p className="meta-line" role="alert">Couldn’t load agent details. <button type="button" className="link" onClick={() => setNonce((n) => n + 1)}>Try again</button></p>
      : state.reply === undefined ? <p className="meta-line" role="status">Loading agent details…</p>
      : <p className="meta-line">Result not recorded.</p>}
  </>;
}
