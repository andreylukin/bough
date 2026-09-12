import { useMemo } from "react";
import type { Project, Row } from "./types";
import { StatusMark } from "./status";
import { plainTitle } from "./render";

const clock = (iso: string) =>
  new Date(iso).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });

function Conversation({ row, projects, onOpen, onAssign }: {
  row: Row; projects: Project[];
  onOpen: (id: string) => void; onAssign: (id: string, project: string) => void;
}) {
  return (
    <div className="proj-row">
      <button className="proj-open" onClick={() => onOpen(row.id)}>
        {plainTitle(row.title) || "Untitled session"}
      </button>
      <StatusMark status={row.status} />
      <span className="num proj-when">{clock(row.modified)}</span>
      <label className="proj-move">
        <span className="visually-hidden">Move to project</span>
        <select value={row.project ?? ""} onChange={(e) => onAssign(row.id, e.target.value)}>
          <option value="">Unassigned</option>
          {projects.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
        </select>
      </label>
    </div>
  );
}

export function ProjectsView({ projects, rows, onOpen, onAssign, onCreate, onRename, onDelete }: {
  projects: Project[]; rows: Row[];
  onOpen: (id: string) => void;
  onAssign: (id: string, project: string) => void;
  onCreate: (name: string) => void;
  onRename: (id: string, name: string) => void;
  onDelete: (id: string) => void;
}) {
  const byProject = useMemo(() => {
    const m = new Map<string, Row[]>();
    for (const r of rows) {
      const k = r.project ?? "";
      if (!m.has(k)) m.set(k, []);
      m.get(k)!.push(r);
    }
    return m;
  }, [rows]);

  const unassigned = byProject.get("") ?? [];

  return (
    <div className="thread">
      <header className="thread-head">
        <div className="head-main">
          <h1>Projects</h1>
          <span className="head-repo">
            {projects.length} {projects.length === 1 ? "project" : "projects"}
          </span>
        </div>
        <div className="head-side">
          <button className="btn btn-primary" onClick={() => {
            const name = prompt("Name this project");
            if (name?.trim()) onCreate(name.trim());
          }}>New project</button>
        </div>
      </header>

      <div className="scroll proj-body">
        {projects.length === 0 && (
          <div className="proj-empty">
            <p className="proj-empty-title">No projects yet</p>
            <p>A project groups conversations you think of together — one service, one incident, one migration. A conversation can sit in one, or none.</p>
          </div>
        )}

        {projects.map((p) => {
          const list = byProject.get(p.id) ?? [];
          return (
            <section key={p.id} className="proj">
              <div className="proj-head">
                <h2>{p.name}</h2>
                <span className="num proj-count">
                  {list.length} {list.length === 1 ? "conversation" : "conversations"}
                </span>
                <button className="link" onClick={() => {
                  const name = prompt("Rename project", p.name);
                  if (name?.trim()) onRename(p.id, name.trim());
                }}>Rename</button>
                <button className="link" onClick={() => {
                  if (confirm(`Delete “${p.name}”? Its ${list.length} conversation${list.length === 1 ? "" : "s"} stay, unassigned.`)) {
                    onDelete(p.id);
                  }
                }}>Delete</button>
              </div>
              {list.length === 0
                ? <p className="proj-none">Nothing here yet. Move a conversation in from below.</p>
                : list.map((r) => (
                    <Conversation key={r.id} row={r} projects={projects} onOpen={onOpen} onAssign={onAssign} />
                  ))}
            </section>
          );
        })}

        <section className="proj">
          <div className="proj-head">
            <h2>Unassigned</h2>
            <span className="num proj-count">
              {unassigned.length} {unassigned.length === 1 ? "conversation" : "conversations"}
            </span>
          </div>
          {unassigned.length === 0
            ? <p className="proj-none">Every conversation is in a project.</p>
            : unassigned.map((r) => (
                <Conversation key={r.id} row={r} projects={projects} onOpen={onOpen} onAssign={onAssign} />
              ))}
        </section>
      </div>
    </div>
  );
}
