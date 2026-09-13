import { useCallback, useEffect, useMemo, useState } from "react";
import type { Project, Row } from "./types";
import { StatusMark } from "./status";
import { plainTitle, untitled } from "./render";
import { Back } from "./app";
import { Select } from "./select";
import { askConfirm, askText } from "./dialog";

const clock = (iso: string) =>
  new Date(iso).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });

function Conversation({ row, projects, onOpen, onAssign }: {
  row: Row; projects: Project[];
  onOpen: (id: string) => void; onAssign: (id: string, project: string) => void;
}) {
  return (
    <div className="proj-row">
      <button className="proj-open" onClick={() => onOpen(row.id)}>
        {plainTitle(row.title) || untitled(row.id)}
      </button>
      <StatusMark status={row.status} />
      <span className="num proj-when">{clock(row.modified)}</span>
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
 * Filing 150 conversations one dropdown at a time is not a feature
 * anyone uses. Every session here started in the home directory, so it
 * records no repo at all — but the paths it touched name one, and that
 * is enough to offer the grouping ready-made. Nothing is filed until
 * you say so: these are suggestions, not projects.
 */
/**
 * A project is an area of work, not a repo. Someone with hundreds of
 * repos has a handful of areas, and "uni-fmds-prototype-py" names a
 * checkout, not a thing you are doing — so repos are picked in groups
 * and the project is named by the person, not the path.
 */
function ByRepo() {
  const [groups, setGroups] = useState<RepoGroup[] | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [name, setName] = useState("");
  const [touchedName, setTouchedName] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");

  const load = useCallback(() => {
    fetch("/api/projects/by-repo")
      .then((r) => r.json())
      .then((d) => setGroups(d.groups ?? []))
      .catch(() => setGroups([]));
  }, []);
  useEffect(load, [load]);

  const chosen = useMemo(() => [...picked], [picked]);
  const suggestion = useMemo(() => suggestName(chosen), [chosen]);
  // The name follows the selection until you type your own.
  useEffect(() => { if (!touchedName) setName(suggestion); }, [suggestion, touchedName]);

  const toggle = (repo: string) => setPicked((prev) => {
    const next = new Set(prev);
    next.has(repo) ? next.delete(repo) : next.add(repo);
    return next;
  });

  const make = async () => {
    setBusy(true);
    setErr("");
    try {
      const r = await fetch("/api/projects/from-repo", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ repos: chosen, name: name.trim() }),
      });
      if (!r.ok) throw new Error((await r.json().catch(() => ({}))).error ?? `HTTP ${r.status}`);
      setPicked(new Set());
      setTouchedName(false);
      load();
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  if (groups === null || groups.length === 0) return null;
  const total = groups.reduce((n, g) => n + g.count, 0);
  const picking = chosen.length > 0;
  const covered = groups.filter((g) => picked.has(g.repo)).reduce((n, g) => n + g.count, 0);

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
        Read from the paths each conversation actually worked in. Pick the repos that belong
        to one area of work and name it — a project can hold several repos.
      </p>
      {err && <p className="err">{err}</p>}

      {groups.map((g) => (
        <div key={g.repo} className="hk2-row">
          <label className="hk2-line rp-pick">
            <input type="checkbox" checked={picked.has(g.repo)} onChange={() => toggle(g.repo)} />
            <span className="mono hk2-name">{g.repo}</span>
            <span className="hk2-facts">
              {g.count} {g.count === 1 ? "conversation" : "conversations"}
            </span>
          </label>
        </div>
      ))}

      {picking && (
        <div className="rp-bar">
          <label className="rp-name">
            <span className="ctl-label">Project name</span>
            <input className="field" value={name} placeholder="What is this work?"
                   onChange={(e) => { setTouchedName(true); setName(e.target.value); }} />
          </label>
          <span className="hk2-facts">
            {chosen.length} {chosen.length === 1 ? "repo" : "repos"} · {covered}{" "}
            {covered === 1 ? "conversation" : "conversations"}
          </span>
          <button className="btn btn-primary" disabled={busy || !name.trim()} onClick={make}>
            {busy ? "Making…" : "Make a project"}
          </button>
          <button className="btn" onClick={() => { setPicked(new Set()); setTouchedName(false); }}>
            Clear
          </button>
        </div>
      )}
    </section>
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
          <button className="btn btn-primary" onClick={async () => {
            const name = await askText("New project", { placeholder: "What is this work?", action: "Create" });
            if (name) onCreate(name);
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
                <button className="link" onClick={async () => {
                  const name = await askText("Rename project", { initial: p.name, action: "Rename" });
                  if (name) onRename(p.id, name);
                }}>Rename</button>
                <button className="link" onClick={async () => {
                  const ok = await askConfirm(`Delete “${p.name}”?`,
                    `Its ${list.length} conversation${list.length === 1 ? "" : "s"} stay, unassigned.`,
                    { action: "Delete project", danger: true });
                  if (ok) onDelete(p.id);
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
        
