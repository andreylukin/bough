import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Project, Row } from "./types";
import { StatusMark, shownStatus } from "./status";
import { plainTitle, untitled } from "./render";
import { Back } from "./app";
import { Select } from "./select";
import { askConfirm, askText } from "./dialog";

const clock = (iso: string) =>
  new Date(iso).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });

function Conversation({ row, projects, onOpen, onAssign, picked, onPick, locked }: {
  row: Row; projects: Project[];
  onOpen: (id: string) => void; onAssign: (id: string, project: string) => void;
  picked: boolean; onPick: (id: string) => void;
  /** A bulk move is running: the selection it snapshotted cannot change under it. */
  locked: boolean;
}) {
  const title = plainTitle(row.title) || untitled(row.id);
  return (
    <div className={"proj-row" + (picked ? " proj-picked" : "")}>
      <label className="proj-check">
        <input type="checkbox" checked={picked} disabled={locked} onChange={() => onPick(row.id)} />
        <span className="visually-hidden">Select {title}</span>
      </label>
      {/* Title and when in one target, so a phone row is two short lines, not three. */}
      <button className="proj-open" onClick={() => onOpen(row.id)}>
        <span className="proj-title">{title}</span>
        <span className="num proj-when">{clock(row.modified)}</span>
      </button>
      <StatusMark status={shownStatus(row)} />
      {/* With no project to move to, a one-option menu is a dead end. */}
      {projects.length > 0 && (
        <div className="proj-move">
          <Select label="Move to project" value={row.project ?? ""} align="end"
                  onChange={(p) => onAssign(row.id, p)}
                  options={[{ value: "", label: "Unassigned" }, ...projects.map((p) => ({ value: p.id, label: p.name }))]} />
        </div>
      )}
    </div>
  );
}


interface RepoGroup { repo: string; count: number; sessions: string[] }

/**
 * Every session here started in the home directory, so it records no
 * repo — but the paths it touched name one. Ticking a repo selects its
 * unassigned sessions in the page's one selection; the bar below files
 * them, so there is no second list to keep in step.
 */
function ByRepo({ unassigned, selected, onPickMany }: {
  unassigned: Set<string>; selected: Set<string>; onPickMany: (ids: string[], on: boolean) => void;
}) {
  const [groups, setGroups] = useState<RepoGroup[] | null>(null);
  const [err, setErr] = useState("");

  const load = useCallback(() => {
    setErr("");
    fetch("/api/projects/by-repo")
      .then((r) => { if (!r.ok) throw new Error(`HTTP ${r.status}`); return r.json(); })
      .then((d) => setGroups(d.groups ?? []))
      .catch((e: unknown) => setErr(e instanceof Error ? e.message : String(e)));
  }, []);
  useEffect(load, [load]);

  // A failure or a slow read is a row that says so, not a missing feature.
  if (err || groups === null) {
    return (
      <p className="rp-state" role="status">
        {err ? <>Repo grouping unavailable · {err} <button className="link" onClick={load}>Retry</button></> : "Reading repos…"}
      </p>
    );
  }
  if (groups.length === 0) return null;
  // Unique unassigned sessions: one conversation can touch more than one
  // repo, and one already filed is not coverage of the unassigned ones.
  const total = new Set(groups.flatMap((g) => g.sessions).filter((id) => unassigned.has(id))).size;

  return (
    <details className="proj rp-group">
      <summary className="proj-head">
        <h2>Select unassigned by repo</h2>
        <span className="num proj-count">
          {total} of {unassigned.size} have an inferred repo, across {groups.length} {groups.length === 1 ? "repo" : "repos"}
        </span>
      </summary>
      {groups.map((g) => {
        const ids = g.sessions.filter((id) => unassigned.has(id));
        const on = ids.length > 0 && ids.every((id) => selected.has(id));
        return (
          <div key={g.repo} className="hk2-row">
            <label className="hk2-line rp-pick">
              <input type="checkbox" checked={on} disabled={ids.length === 0} onChange={() => onPickMany(ids, !on)} />
              <span className="mono hk2-name">{g.repo}</span>
              <span className="hk2-facts">
                {ids.length} {ids.length === 1 ? "conversation" : "conversations"}
              </span>
            </label>
          </div>
        );
      })}
    </details>
  );
}

/**
 * A first guess at what a set of repos is called: what they share,
 * minus the organisation prefix every repo at a company carries. Two
 * "uni-fmds-*" repos suggest "fmds"; unrelated ones suggest nothing,
 * because a wrong name is worse than an empty box.
 */
export function suggestName(repos: string[]): string {
  if (repos.length === 0) return "";
  const parts = repos.map((r) => r.split("-").filter(Boolean));
  const shared: string[] = [];
  for (let i = 0; i < parts[0].length; i++) {
    const seg = parts[0][i];
    if (parts.every((p) => p[i] === seg)) shared.push(seg);
    else break;
  }
  // A single leading segment shared by everything is the org, not the
  // subject: "uni" alone says nothing about what the work is.
  const useful = shared.length > 1 ? shared.slice(1) : shared;
  if (repos.length === 1) return repos[0];
  return useful.length > 0 ? useful.join("-") : "";
}

export function ProjectsView({ projects, rows, onOpen, onBack, onAssign, onAssignMany, onCreate, onRename, onDelete }: {
  projects: Project[]; rows: Row[];
  onOpen: (id: string) => void;
  onBack?: () => void;
  onAssign: (id: string, project: string) => void;
  /** Resolves to the ids that did not move. */
  onAssignMany: (ids: string[], project: string) => Promise<string[]>;
  onCreate: (name: string) => Promise<{ id: string }>;
  onRename: (id: string, name: string) => Promise<void>;
  onDelete: (id: string) => void;
}) {
  const [filter, setFilter] = useState("");
  // The page's one selection, whichever list or repo it was ticked from.
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [target, setTarget] = useState("");

  const needle = filter.trim().toLowerCase();
  const shown = useMemo(() => needle
    ? rows.filter((r) => (plainTitle(r.title) || "").toLowerCase().includes(needle) || (r.repo ?? "").toLowerCase().includes(needle))
    : rows, [rows, needle]);

  const byProject = useMemo(() => {
    const m = new Map<string, Row[]>();
    for (const r of shown) {
      const k = r.project ?? "";
      if (!m.has(k)) m.set(k, []);
      m.get(k)!.push(r);
    }
    return m;
  }, [shown]);

  const unassigned = byProject.get("") ?? [];
  const unassignedAll = useMemo(() => new Set(rows.filter((r) => !r.project).map((r) => r.id)), [rows]);

  // While a move runs its selection is locked: a second move cannot overlap it.
  const [moving, setMoving] = useState(false);
  const pickMany = (ids: string[], on: boolean) => moving || setSelected((prev) => {
    const next = new Set(prev);
    for (const id of ids) on ? next.add(id) : next.delete(id);
    return next;
  });
  const pick = (id: string) => pickMany([id], !selected.has(id));
  const ids = [...selected];

  // What moved leaves the selection; what did not stays selected, with a
  // Retry. A retry inside one "New project…" reuses the project it made.
  const [failed, setFailed] = useState<{ project: string; n: number } | null>(null);
  const made = useRef<string | null>(null);
  const assign = async (project: string) => {
    const batch = [...selected];
    setFailed(null); setMoving(true);
    let left: string[];
    try { left = await onAssignMany(batch, project); } catch { left = batch; }
    setMoving(false);
    setSelected(new Set(left));
    if (left.length) { setFailed({ project, n: left.length }); return false; }
    return true;
  };

  const createProject = async () => {
    made.current = null;
    await askText("New project", { placeholder: "What is this work?", action: "Create",
      onSubmit: async (name) => {
        const id = made.current ?? (await onCreate(name)).id;
        made.current = id;
        // With a selection, the new project is where it goes.
        if (ids.length && !(await assign(id))) throw new Error("the project exists, but some conversations did not move");
      } });
    // Done or cancelled, the next creation is a new project.
    made.current = null;
  };

  const list = (rs: Row[]) => rs.map((r) => (
    <Conversation key={r.id} row={r} projects={projects} onOpen={onOpen} onAssign={onAssign}
                  picked={selected.has(r.id)} onPick={pick} locked={moving} />
  ));

  return (
    <div className="thread">
      <header className="thread-head page-head proj-page-head">
        <Back onBack={onBack} />
        <div className="head-main">
          <h1>Projects</h1>
          <span className="head-repo">
            {projects.length} {projects.length === 1 ? "project" : "projects"}
          </span>
        </div>
        <div className="head-side">
          <button className="btn btn-primary" onClick={() => { void createProject(); }}>New project</button>
        </div>
      </header>

      <div className="scroll proj-body">
        <div className="proj-filter">
          <input className="field" type="search" value={filter} placeholder="Filter conversations by title or repo"
                 aria-label="Filter conversations" onChange={(e) => setFilter(e.target.value)} />
        </div>
        <ByRepo unassigned={unassignedAll} selected={selected} onPickMany={pickMany} />

        {projects.map((p) => {
          const rs = byProject.get(p.id) ?? [];
          return (
            <section key={p.id} className="proj">
              <div className="proj-head">
                <h2>{p.name}</h2>
                <span className="num proj-count">
                  {rs.length} {rs.length === 1 ? "conversation" : "conversations"}
                </span>
                <button className="link" onClick={() => {
                  void askText("Rename project", { initial: p.name, action: "Rename", onSubmit: (name) => onRename(p.id, name) });
                }}>Rename</button>
                <button className="link" onClick={async () => {
                  const ok = await askConfirm(`Delete “${p.name}”?`,
                    `Its ${rs.length} conversation${rs.length === 1 ? "" : "s"} stay, unassigned.`,
                    { action: "Delete project", danger: true });
                  if (ok) onDelete(p.id);
                }}>Delete</button>
              </div>
              {rs.length === 0
                ? <p className="proj-none">{needle ? "Nothing here matches the filter." : "Nothing here yet. Move a conversation in from below."}</p>
                : list(rs)}
            </section>
          );
        })}

        <section className="proj">
          <div className="proj-head">
            <h2>Unassigned</h2>
            <span className="num proj-count">
              {unassigned.length} {unassigned.length === 1 ? "conversation" : "conversations"}
            </span>
            {unassigned.length > 0 && (
              <button className="link" onClick={() => pickMany(unassigned.map((r) => r.id), !unassigned.every((r) => selected.has(r.id)))}>
                {unassigned.every((r) => selected.has(r.id)) ? "Select none" : "Select all"}
              </button>
            )}
          </div>
          {unassigned.length === 0
            ? <p className="proj-none">{needle ? "Nothing unassigned matches the filter." : "Every conversation is in a project."}</p>
            : list(unassigned)}
        </section>
      </div>

      {ids.length > 0 && (
        <div className="rp-bar" role="region" aria-label="Selected conversations" aria-busy={moving || undefined}>
          <span className="num rp-count">{moving ? `Moving ${ids.length}…` : `${ids.length} selected`}</span>
          {projects.length > 0 && (
            <>
              <Select label="Project" value={target} align="start"
                      options={[{ value: "", label: "Choose a project" }, ...projects.map((p) => ({ value: p.id, label: p.name }))]}
                      onChange={setTarget} />
              <button className="btn btn-primary" disabled={!target || moving}
                      onClick={() => { void assign(target); }}>Assign</button>
            </>
          )}
          {failed && (
            <span className="err rp-err" role="alert">
              {failed.n} not moved <button className="link" disabled={moving} onClick={() => { void assign(failed.project); }}>Retry</button>
            </span>
          )}
          <button className="btn" disabled={moving} onClick={() => { void createProject(); }}>New project…</button>
          <button className="btn rp-clear" disabled={moving} onClick={() => { setSelected(new Set()); setFailed(null); }}>Clear</button>
        </div>
      )}
    </div>
  );
}
