import { useEffect, useRef, useState } from "react";
import type { OrbPhase, OrbState, OrbStatus, Project, Row, SessionMode } from "./types";
import { api } from "./api";
import { clampToViewport } from "./popover";
import { STATUS } from "./status";
import { Select } from "./select";
import { SetupFailed, orbWord } from "./status";
import { failedBuild } from "./orb";

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

/** A project session's slug and orb state; a local session shows nothing, local being the norm. */
/** `bare` drops the project name where a group heading already says it. `name` is the project's display name. */
/** `phases` makes the chip a disclosure of the start's timed phases (the thread header; never inside a row button). */
export function ModeChip({ row, bare = false, name, phases = false }: { row: Row; bare?: boolean; name?: string; phases?: boolean }) {
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
  // A failed setup is its own indicator, set apart from the run status that follows it.
  // Bare sits beside a turn's own status (the sidebar): it says "Orb …" and
  // a running orb stays quiet, so green only ever means a running turn.
  const chip = status === "failed"
    ? <SetupFailed name={shown} fresh={Date.now() - Date.parse(row.lastAt) < FRESH_MS} />
    : <span className={"status mode-chip " + (bare && status === "running" ? "mode-up" : TONE[status])} title={`Runs in the ${shown} orb`}>
        {bare ? `Orb ${orbWord(status).toLowerCase()}` : `${shown} · ${orbWord(status)}`}
      </span>;
  const sep = status === "failed" ? <span className="setup-sep" aria-hidden="true"> · </span> : null;
  if (!phases) return <>{chip}{sep}</>;
  return <><PhasesDisclosure session={row.id} status={status} name={shown}>{chip}</PhasesDisclosure>{sep}</>;
}

const ORDER = ["sync", "build", "worktree", "container", "resume.sh", "ready"];
const WORD: Record<string, string> = {
  sync: "Sync repos", build: "Build image", worktree: "Worktree", container: "Start container", "resume.sh": "resume.sh", ready: "Ready",
};

/** "4s", "1m 15s": a phase's time, to now while it runs. */
export function phaseDuration(p: OrbPhase, now: number): string {
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
        <p className="orb-pop-head">Orb start</p>
        {orb ? <OrbPhases orb={orb} now={now} /> : <p className="orb-phase-foot">Loading…</p>}
      </div>
    </details>
  );
}
