import { useEffect, useRef, useState } from "react";
import type { OrbPhase, OrbState, OrbStatus, Project, Row, SessionMode } from "./types";
import { api } from "./api";
import { clampToViewport } from "./popover";
import { STATUS } from "./status";
import { Select } from "./select";
import { SetupFailed, orbWord } from "./status";
import { failedBuild, orbUp } from "./orb";

export interface ModeValue { mode: SessionMode; project?: string }

/**
 * Where a new session runs. Local is the default and needs no project;
 * only labels carrying an orb definition can host a project session,
 * because a label alone has no container to run in.
 */
export function ModePicker({ projects, value, onChange }: {
  projects: Project[]; value: ModeValue; onChange: (v: ModeValue) => void;
}) {
  const withOrb = projects.filter((p) => p.slug).sort((a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: "base" }));
  const local = value.mode === "local";
  return (
    <div className="mode-picker seg" role="group" aria-label="Where a new session runs">
      <button type="button" className="seg-item" aria-pressed={local}
              title="Runs on this machine. Can edit files only inside a git checkout; read-only elsewhere."
              onClick={() => onChange({ mode: "local" })}>Local</button>
      {withOrb.length > 0
        ? <Select label="Project" value={local ? "" : value.project ?? ""} align="start" placeholder="In a project…"
                  options={withOrb.map((p) => ({ value: p.id, label: p.name, detail: failedBuild(p) ? "Build failed" : undefined }))}
                  onChange={(id) => onChange(id ? { mode: "project", project: id } : { mode: "local" })} />
        : <button type="button" className="seg-item" disabled title="No project has an orb yet">Project</button>}
      {!local && failedBuild(withOrb.find((p) => p.id === value.project)) && (
        <span className="mode-warn" role="note">Last image build failed</span>
      )}
    </div>
  );
}

/* A failure stops being an alarm once you have had a chance to see it:
   after this it stays on the row in words, in the resting grey. */
const FRESH_MS = 6 * 60 * 60 * 1000;

const TONE: Record<OrbStatus, string> = {
  "": "mode-stopped", running: "mode-running", building: "mode-busy", starting: "mode-busy",
  failed: "mode-failed", stopped: "mode-stopped",
};

/**
 * The step named as it reads while it is running. The popover's WORD list
 * names the steps as an inventory ("Sync repos"); a chip says what is
 * happening right now, so it speaks in the present participle.
 */
const BUSY_WORD: Record<string, string> = {
  sync: "Syncing repos", build: "Building image", worktree: "Making worktrees",
  container: "Starting container", "resume.sh": "Running resume.sh", ready: "Ready",
};

/**
 * A 1s tick while `on`. One interval per busy chip, stopped the moment the
 * orb settles: with many rows building that is N intervals, bounded by the
 * number of orbs starting at once, and zero once they are up.
 */
function useTick(on: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!on) return;
    setNow(Date.now());
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [on]);
  return now;
}

/**
 * "2.03s", "12s", "4m 46s". A warm start is over in a couple of seconds, so
 * under ten it keeps its hundredths — that is the number worth comparing;
 * past that the fraction is noise.
 */
export function startupWord(ms: number): string {
  const s = Math.max(0, ms) / 1000;
  if (s < 10) return `${s.toFixed(2)}s`;
  const r = Math.round(s);
  return r < 60 ? `${r}s` : `${Math.floor(r / 60)}m ${r % 60}s`;
}

/** The start's total from its phases: to ready, or to now while it is still running. */
export function startupMs(phases: OrbPhase[] | undefined, now: number): number | undefined {
  if (!phases?.length) return undefined;
  const from = Date.parse(phases[0].startedAt);
  if (!Number.isFinite(from)) return undefined;
  const ready = phases.find((p) => p.name === "ready");
  return Math.max(0, (ready ? Date.parse(ready.startedAt) : now) - from);
}

/** How long the start took. Resting it is a plain fact; while it climbs it borrows the busy amber. */
export function OrbStartup({ ms, live }: { ms: number; live?: boolean }) {
  return <span className={"orb-startup mono" + (live ? " is-live" : "")}
               title="Time from the first step to ready">{startupWord(ms)}</span>;
}

/** A project session's slug and orb state; a local session shows nothing, local being the norm. */
/** `bare` drops the project name where a group heading already says it. `name` is the project's display name. */
/** `phases` makes the chip a disclosure of the start's timed phases (the thread header; never inside a row button). */
export function ModeChip({ row, bare = false, name, phases = false }: { row: Row; bare?: boolean; name?: string; phases?: boolean }) {
  // The tick is read before any of the early returns below: a bare row whose
  // orb settles stops rendering the chip, and a hook called after that gate
  // would change count mid-render. `busy` is false for every case that bails.
  const busy = row.orb?.status === "building" || row.orb?.status === "starting";
  const now = useTick(busy);
  if (row.mode !== "project" || !row.orb) return null;
  const { project, status } = row.orb;
  // A stopped orb is the resting state: most rows in a project are it,
  // and the group heading above them already says which project. Saying
  // "Orb stopped" on each one repeats the heading, says nothing the row
  // did not already say, and takes the width from the title — which is
  // the one thing on the row that differs. Bare rows keep the word only
  // while the orb is doing something.
  if (bare && (status === "" || status === "stopped")) return null;
  const shown = name || project;
  // A 286s build with the word "Building" on it looks identical at second 4
  // and at minute 4. The step it is on and how long it has been there is the
  // whole of the news, and the row already carries the chip — so the chip
  // says it, and no second fetch is needed: the list poll brought the phase.
  const step = busy && row.orb.phaseAt
    ? `${BUSY_WORD[row.orb.phase ?? ""] ?? orbWord(status)} ${phaseDuration({ startedAt: row.orb.phaseAt }, now)}`
    : "";
  // A failed setup is its own indicator, set apart from the run status that follows it.
  // Bare sits beside a turn's own status (the sidebar): it says "Orb …" and
  // a running orb stays quiet, so green only ever means a running turn.
  const chip = status === "failed"
    ? <SetupFailed name={shown} fresh={Date.now() - Date.parse(row.lastAt) < FRESH_MS} />
    : <span className={"status mode-chip " + (bare && status === "running" ? "mode-up" : TONE[status])} title={`Runs in the ${shown} orb`}>
        {step ? (bare ? step : `${shown} · ${step}`)
              : (bare ? `Orb ${orbWord(status).toLowerCase()}` : `${shown} · ${orbWord(status)}`)}
      </span>;
  const sep = status === "failed" ? <span className="setup-sep" aria-hidden="true"> · </span> : null;
  if (!phases) return <>{chip}{sep}</>;
  return <><PhasesDisclosure session={row.id} status={status} name={shown}>{chip}</PhasesDisclosure>{sep}</>;
}

/**
 * How many of these sessions have a container up right now. Counted per
 * session, because an orb is per session: two sessions in one project are two
 * orbs. `up` outranks status, since a failed setup can leave one up.
 */
export function orbsUp(rows: Row[]): number {
  return rows.filter((r) => r.mode === "project" && orbUp(r.orb)).length;
}

/**
 * "2 orbs up", or nothing. A count of RUNNING orbs is news; a count of stopped
 * ones is not, so zero renders nothing at all — never "0 orbs up". `quiet`
 * drops the accent: in the sidebar green means a running *turn* and nothing else.
 */
export function OrbUp({ n, quiet = false }: { n: number; quiet?: boolean }) {
  if (n < 1) return null;
  return <span className={"orb-up" + (quiet ? " is-quiet" : "")}>{n} orb{n === 1 ? "" : "s"} up</span>;
}

/** The same phrase for an aria-label or a title. */
export const orbsUpLabel = (n: number) => `${n} orb${n === 1 ? "" : "s"} up`;

const ORDER = ["sync", "build", "worktree", "container", "resume.sh", "ready"];
const WORD: Record<string, string> = {
  sync: "Sync repos", build: "Build image", worktree: "Worktree", container: "Start container", "resume.sh": "resume.sh", ready: "Ready",
};

/** "4s", "1m 15s": a phase's time, to now while it runs. */
export function phaseDuration(p: { startedAt: string; endedAt?: string }, now: number): string {
  const end = p.endedAt ? Date.parse(p.endedAt) : now;
  const s = Math.max(0, Math.round((end - Date.parse(p.startedAt)) / 1000));
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${s % 60}s`;
}

/**
 * The start's steps in order: finished ones with their time, the running
 * one with a live timer, the failed one with its error, and the steps not
 * reached (after a failure too) as pending. A restart lists only the steps it ran.
 */
export function OrbPhases({ orb, now }: { orb: OrbState; now: number }) {
  const ran = orb.phases ?? [];
  const first = ran.length ? ORDER.indexOf(ran[0].name) : 0;
  const pending = ORDER.slice(Math.max(first, 0)).filter((n) => !ran.some((p) => p.name === n));
  const mark = (s: keyof typeof STATUS) => (
    <svg className="state-mark" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5"
         strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" style={{ color: STATUS[s].tone }}>{STATUS[s].glyph}</svg>
  );
  return (
    <div className="orb-phases-body">
      <ol className="orb-phase-list">
        {ran.map((p) => {
          const state = p.error ? "error" : p.endedAt ? "done" : "running";
          return (
            <li key={p.name + p.startedAt} className={"orb-phase is-" + state}>
              {mark(state)}
              <span className="orb-phase-name">{WORD[p.name] ?? p.name}</span>
              <span className="orb-phase-time">{p.name === "ready" ? "" : phaseDuration(p, now)}</span>
              {p.error && <span className="orb-phase-err">{p.error}</span>}
            </li>
          );
        })}
        {pending.map((n) => (
          <li key={n} className="orb-phase is-pending">{mark("queued")}<span className="orb-phase-name">{WORD[n] ?? n}</span><span className="orb-phase-time" /></li>
        ))}
      </ol>
      {orb.container && <p className="orb-phase-foot mono" title={orb.image}>{orb.container}</p>}
      <OrbAddress orb={orb} />
    </div>
  );
}

/**
 * Where a running orb's servers are reachable: the container IP (never
 * localhost) and each opted-in 127.0.0.1 forward; a skipped forward says why.
 * Nothing while stopped: the address goes with the VM.
 */
export function OrbAddress({ orb }: { orb: OrbState }) {
  const ports = orb.ports ?? [];
  if (orb.status !== "running" || (!orb.ip && !ports.length)) return null;
  return (
    <p className="orb-addr">
      {orb.ip && <span><span className="orb-addr-k">IP</span> <span className="mono">{orb.ip}</span></span>}
      {ports.map((p) => p.error
        ? <span key={p.host} className="orb-addr-skip" >{`Not forwarded: ${p.error}`}</span>
        : <a key={p.host} className="mono" href={`http://127.0.0.1:${p.host}`} target="_blank" rel="noreferrer">{`127.0.0.1:${p.host}${p.host === p.guest ? "" : ` → ${p.guest}`}`}</a>)}
    </p>
  );
}

/** The chip as a summary; opening it fetches the orb and polls it while it starts. */
function PhasesDisclosure({ session, status, name, children }: { session: string; status: OrbStatus; name: string; children: React.ReactNode }) {
  const ref = useRef<HTMLDetailsElement>(null);
  const [open, setOpen] = useState(false);
  const [orb, setOrb] = useState<OrbState | null>(null);
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!open) return;
    let stop = false, timer: ReturnType<typeof setTimeout> | undefined;
    const tick = async () => {
      try {
        const o = await api.sessionOrb(session);
        if (stop) return;
        setOrb(o); setNow(Date.now());
        if (o && (o.status === "building" || o.status === "starting")) timer = setTimeout(tick, 1000);
      } catch { /* the chip keeps its last state */ }
    };
    void tick();
    return () => { stop = true; clearTimeout(timer); };
  }, [open, session, status]);
  useEffect(() => {
    if (!open) return;
    clampToViewport(ref.current?.querySelector<HTMLElement>(":scope>.orb-pop") ?? null);
    const key = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || !ref.current) return;
      e.preventDefault(); e.stopPropagation();
      ref.current.open = false; ref.current.querySelector("summary")?.focus();
    };
    const away = (e: MouseEvent) => { if (ref.current && !ref.current.contains(e.target as Node)) ref.current.open = false; };
    window.addEventListener("keydown", key, true);
    document.addEventListener("mousedown", away);
    return () => { window.removeEventListener("keydown", key, true); document.removeEventListener("mousedown", away); };
  }, [open]);
  return (
    <details ref={ref} className="orb-phases" onToggle={(e) => setOpen(e.currentTarget.open)}>
      <summary aria-label={`${name} orb: start phases`}>{children}</summary>
      <div className="orb-pop" role="group" aria-label={`${name} orb start`}>
        <p className="orb-pop-head">
          <span>Orb start</span>
          {/* The total belongs here, where someone opening the popover went
              looking for it — not on the crowded header beside the chip. */}
          {orb && startupMs(orb.phases, now) !== undefined &&
            <OrbStartup ms={startupMs(orb.phases, now)!} live={orb.status === "building" || orb.status === "starting"} />}
        </p>
        {orb ? <OrbPhases orb={orb} now={now} /> : <p className="orb-phase-foot">Loading…</p>}
      </div>
    </details>
  );
}
