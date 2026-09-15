import type { OrbStatus, Project, Row, SessionMode } from "./types";
import { Button } from "@/components/ui/button";
import { Select } from "./select";
import { SetupFailed, orbWord } from "./status";

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
    <div className="ctl mode-picker" role="group" aria-label="Session mode">
      <span className="ctl-label">Run</span>
      <Button variant={local ? "default" : "neutral"} aria-pressed={local}
              title="Run on this machine: reads your home folder, writes nothing outside it"
              onClick={() => onChange({ mode: "local" })}>Local</Button>
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
/** `bare` drops the project name where a group heading already says it. `name` is the project's display name. */
export function ModeChip({ row, bare = false, name }: { row: Row; bare?: boolean; name?: string }) {
  if (row.mode !== "project" || !row.orb) return null;
  const { project, status } = row.orb;
  const shown = name || project;
  // A failed setup is its own indicator, set apart from the run status that follows it.
  if (status === "failed") return <><SetupFailed name={shown} /><span className="setup-sep" aria-hidden="true"> · </span></>;
  return (
    <span className={"status mode-chip " + TONE[status]} title={`Runs in the ${shown} orb`}>
      {bare ? orbWord(status) : `${shown} · ${orbWord(status)}`}
    </span>
  );
}
