import { useCallback, useEffect, useMemo, useState } from "react";
import type { Project, Row } from "./types";
import { StatusMark } from "./status";
import { plainTitle } from "./render";
import { Back } from "./app";

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


interface RepoGroup { repo: string; count: number; sessions: string[] }

/**
 * Filing 150 conversations one dropdown at a time is not a feature
 * anyone uses. Every session here started in the home directory, so it
 * records no repo at all — but the paths it touched name one, and that
 * is enough to offer the grouping ready-made. Nothing is filed until
 * you say so: these are suggestions, not projects.
 */
function ByRepo() {
  const [groups, setGroups] = useState<RepoGroup[] | null>(null);
  const [busy, setBusy] = useState("");
  const [err, setErr] = useState("");

  const load = useCallback(() => {
    fetch("/api/projects/by-repo")
      .then((r) => r.json())
      .then((d) => setGroups(d.groups ?? []))
      .catch(() => setGroups([]));
  }, []);
  useEffect(load, [load]);

  const file = async (repo: string) => {
    setBusy(repo);
    setErr("");
    try {
      const r = await fetch("/api/projects/from-repo", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ repo }),
      });
      if (!r.ok) throw new Error((await r.json().catch(() => ({}))).error ?? `HTTP ${r.status}`);
      load();
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy("");
    }
  };

  if (groups === null) return null;
  if (groups.length === 0) return null;
  const total = groups.reduce((n, g) => n + g.count, 0);

  return (
    <section className="proj">
      <div className="proj-head">
        <h2>Group by repo</h2>
        <span className="num proj-count">
          {total} unfiled {total === 1 ? "conversation" : "conversations"} across{" "}
          {groups.length} {groups.length === 1 ? "repo" : "repos"}
        </span>
      </div>
      <p className="proj-none">
        Read from the paths each conversation actually worked in. Making a project files
        every conversation in that group; you can rename or delete it afterwards.
      </p>
      {err && <p className="err">{err}</p>}
      {groups.map((g) => (
        <div key={g.repo} className="hk2-row">
          <div className="hk2-line">
            <span className="mono hk2-name">{g.repo}</span>
            <span className="hk2-facts">
              {g.count} {g.count === 1 ? "conversation" : "conversations"}
            </span>
            <span className="hk2-actions">
              <button className="btn" disabled={busy === g.repo} onClick={() => file(g.repo)}>
                {busy === g.repo ? "Making…" : "Make a project"}
              </button>
            </span>
          </div>
        </div>
      ))}
    </section>
  );
}

export function ProjectsView({ projects, rows, onOpen, onBack, onAssign, onCreate, onRename, onDelete }: {
  projects: Project[]; rows: Row[];
  onOpen: (id: string) => void;
  onBack?: () => void;
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
        <Back onBack={onBack} />
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
        <ByRepo />
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
        
