import type { OrbStatus, Project, Row, SessionMode } from "./types";
import { Select } from "./select";

export interface ModeValue { mode: SessionMode; project?: string }

/**
 * Where a new session runs. Local is the default and needs no project;
 * only labels carrying an orb definition can host a project session,
 * because a label alone has no container to run in.
 */
export function ModePicker({ projects, value, onChange }: {
  projects: Project[]; value: ModeValue; onChange: (v: ModeValue) => void;
}) {
  const withOrb = projects.filter((p) => p.slug);
  const local = value.mode === "local";
  return (
    <div className="ctl mode-picker" role="group" aria-label="Session mode">
      <span className="ctl-label">Run</span>
      <button className={"btn" + (local ? " btn-primary" : "")} aria-pressed={local}
              title="Run on this machine: reads your home folder, writes nothing outside it"
              onClick={() => onChange({ mode: "local" })}>Local</button>
      {withOrb.length > 0
        ? <Select label="Project" value={local ? "" : value.project ?? ""} align="start" placeholder="In a project…"
                  options={withOrb.map((p) => ({ value: p.id, label: p.name }))}
                  onChange={(id) => onChange(id ? { mode: "project", project: id } : { mode: "local" })} />
        : <span className="ctl-label">No project has an orb yet</span>}
    </div>
  );
}

const TONE: Record<OrbStatus, string> = {
  "": "mode-stopped", running: "mode-running", building: "mode-busy", starting: "mode-busy",
  failed: "mode-failed", stopped: "mode-stopped",
};

/** A project session's slug and orb state; a local session shows nothing, local being the norm. */
/** `bare` drops the project name where a group heading already says it. */
export function ModeChip({ row, bare = false }: { row: Row; bare?: boolean }) {
  if (row.mode !== "project" || !row.orb) return null;
  const { project, status } = row.orb;
  return (
    <span className={"status mono mode-chip " + TONE[status]} title={`Runs in the ${project} orb`}>
      {bare ? status || "pending" : `${project} · ${status || "pending"}`}
    </span>
  );
}
