import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { OrbDetail, OrbFile, Project, Row } from "./types";
import { api } from "./api";
import { ProjectOrb } from "./orb";
import { StatusMark, shownStatus } from "./status";
import { plainTitle, untitled } from "./render";
import { Back } from "./app";
import { Select } from "./select";
import { askConfirm, askText } from "./dialog";

const clock = (iso: string) =>
  new Date(iso).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });

function Conversation({ row, repo, projects, onOpen, onAssign, picked, onPick, locked }: {
  row: Row; projects: Project[];
  /** The session's repo, own or inferred; omitted where a group head already names it. */
  repo?: string;
  onOpen: (id: string) => void; onAssign: (id: string, project: string) => void;
  picked: boolean; onPick: (id: string) => void;
  /** A bulk move is running: the selection it snapshotted cannot change under it. */
  locked: boolean;
}) {
  // No title yet: what the session is about beats an opaque id.
  const title = plainTitle(row.title) || row.summary?.split(/(?<=[.!?])\s/)[0] || untitled(row.id);
  return (
    <div className={"proj-row" + (picked ? " proj-picked" : "")}>
      <label className="proj-check">
        <input type="checkbox" checked={picked} disabled={locked} onChange={() => onPick(row.id)} />
        <span className="visually-hidden">Select {title}</span>
      </label>
      {/* Title and when in one target, so a phone row is two short lines, not three. */}
      <button className="proj-open" onClick={() => onOpen(row.id)}>
        <span className="proj-title">{title}</span>
        {repo && <span className="mono proj-repo">{repo}</span>}
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


/**
 * The orb surface for one label, with its fetching: the detail, and the
 * build log polled once a second while a build runs (the log endpoint
 * hands back an offset, so each poll reads only what is new).
 */
function OrbSection({ project, onOpen, onChanged }: {
  project: Project; onOpen: (id: string) => void; onChanged: () => Promise<void> | void;
}) {
  const [detail, setDetail] = useState<OrbDetail>();
  const [err, setErr] = useState("");
  const [log, setLog] = useState("");
  const offset = useRef(0);

  const load = useCallback(() => {
    if (!project.slug) return;
    api.orb(project.id).then((d) => { setDetail(d); setErr(""); }, (e: unknown) => setErr(e instanceof Error ? e.message : String(e)));
  }, [project.id, project.slug]);
  useEffect(load, [load]);

  const state = detail?.build.state;
  useEffect(() => {
    if (!project.slug) return;
    let stop = false;
    offset.current = 0; setLog("");
    const tick = async () => {
      try {
        const r = await api.buildLog(project.id, offset.current);
        if (stop) return;
        if (r.text) setLog((l) => l + r.text);
        offset.current = r.offset;
        if (r.state === "building") { setTimeout(tick, 1000); return; }
        // It finished between polls: the detail's image and state are stale.
        if (state === "building") load();
      } catch { /* no log yet is the common case, not an error worth a banner */ }
    };
    void tick();
    return () => { stop = true; };
  }, [project.id, project.slug, state, load]);

  const run = async (fn: () => Promise<unknown>) => {
    try { await fn(); } catch (e) { setErr(e instanceof Error ? e.message : String(e)); }
    await onChanged(); load();
  };

  return (
    <ProjectOrb project={project} detail={detail} log={log} error={err} onOpen={onOpen}
      onAttach={() => { void run(() => api.attachOrb(project.id)); }}
      onDetach={async () => {
        const ok = await askConfirm(`Detach the orb from “${project.name}”?`,
          `The files in ~/.bough/projects/${project.slug} stay.`, { action: "Detach", danger: true });
        if (ok) { setDetail(undefined); void run(() => api.detachOrb(project.id)); }
      }}
      onSave={async (name: OrbFile, text: string) => { await api.putOrbFile(project.id, name, text); load(); }}
      onBuild={() => { void run(() => api.buildOrb(project.id)); }}
      onStopOrb={(session) => { void run(() => api.stopOrb(session)); }} />
  );
}

interface RepoGroup { repo: string; count: number; sessions: string[] }

/**
 * Sessions started in the home directory record no repo, but the paths
 * they touched name one (GET /api/projects/by-repo). A session's own repo
 * wins; the inferred one is the fallback; neither is "Unknown repo".
 */
function useInferredRepos() {
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
  return { groups, err, load };
}

const UNKNOWN = "Unknown repo";

/**
 * A first guess at what a set of repos is called: what they share,
 * minus the organisation prefix every repo at a company carries. Two
 * "acme-*" repos suggest "acme"; unrelated ones suggest nothing,
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

export function ProjectsView({ projects, rows, onOpen, onBack, onAssign, onAssignMany, onCreate, onRename, onDelete, orbOpen, onOrbOpen, onOrbChanged = () => {} }: {
  projects: Project[]; rows: Row[];
  /** The project whose orb section is expanded (#/projects/<id>/orb). */
  orbOpen?: string;
  onOrbOpen?: (id: string | undefined) => void;
  /** An orb was attached, detached or built: the project list is stale. */
  onOrbChanged?: () => Promise<void> | void;
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
  // Controlled by the route when the app passes it; a story drives it locally.
  const [orbLocal, setOrbLocal] = useState<string>();
  const orbId = onOrbOpen ? orbOpen : orbLocal;
  const toggleOrb = (id: string) => (onOrbOpen ?? setOrbLocal)(orbId === id ? undefined : id);

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
  const inferred = useInferredRepos();
  const repoOf = useMemo(() => {
    const m = new Map<string, string>();
    for (const g of inferred.groups ?? []) for (const id of g.sessions) if (!m.has(id)) m.set(id, g.repo);
    return (r: Row) => r.repo?.split("/").pop() || m.get(r.id) || UNKNOWN;
  }, [inferred.groups]);
  // Unassigned sessions grouped by repo by default, the unknown ones last.
  const byRepo = useMemo(() => {
    const m = new Map<string, Row[]>();
    for (const r of unassigned) {
      const k = repoOf(r);
      if (!m.has(k)) m.set(k, []);
      m.get(k)!.push(r);
    }
    return [...m.entries()].sort(([a, x], [b, y]) => (a === UNKNOWN ? 1 : b === UNKNOWN ? -1 : y.length - x.length));
  }, [unassigned, repoOf]);
  // The same repo identities the sidebar shows, counted across every session.
  const detected = useMemo(() => new Set(rows.map(repoOf).filter((k) => k !== UNKNOWN)).size, [rows, repoOf]);

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
        if (ids.length && !(await assign(id))) throw new Error("the project exists, but some sessions did not move");
      } });
    // Done or cancelled, the next creation is a new project.
    made.current = null;
  };

  // Explicit: names the project after the repo, asks first, and files only that repo's unassigned sessions.
  const fromRepo = async (repo: string, rs: Row[]) => {
    const batch = rs.map((r) => r.id);
    await askText(`Create project from ${repo}`, { initial: repo, action: "Create and move",
      placeholder: "What is this work?",
      onSubmit: async (name) => {
        const id = made.current ?? (await onCreate(name)).id;
        made.current = id;
        setFailed(null); setMoving(true);
        let left: string[];
        try { left = await onAssignMany(batch, id); } catch { left = batch; }
        setMoving(false);
        if (left.length) { setSelected(new Set(left)); setFailed({ project: id, n: left.length }); throw new Error("the project exists, but some sessions did not move"); }
      } });
    made.current = null;
  };

  const list = (rs: Row[], grouped = false) => rs.map((r) => (
    <Conversation key={r.id} row={r} repo={grouped || repoOf(r) === UNKNOWN ? undefined : repoOf(r)} projects={projects} onOpen={onOpen} onAssign={onAssign}
                  picked={selected.has(r.id)} onPick={pick} locked={moving} />
  ));

  return (
    <div className="thread">
      <header className="thread-head page-head proj-page-head">
        <Back onBack={onBack} />
        <div className="head-main">
          <h1>Projects</h1>
          <span className="head-repo">
            {projects.length} configured {projects.length === 1 ? "project" : "projects"} · {detected} detected {detected === 1 ? "repo" : "repos"}
          </span>
        </div>
        <div className="head-side">
          <button className="btn btn-primary" onClick={() => { void createProject(); }}>New project</button>
        </div>
      </header>

      <div className="scroll proj-body">
        <div className="proj-filter">
          <input className="field" type="search" value={filter} placeholder="Filter sessions by title or repo"
                 aria-label="Filter sessions" onChange={(e) => setFilter(e.target.value)} />
        </div>
        <p className="proj-lede">A project groups sessions from any repo under one name. Select sessions to move them into
          a project; moving only files them here, and nothing inside a session changes.</p>

        {projects.map((p) => {
          const rs = byProject.get(p.id) ?? [];
          return (
            <section key={p.id} className="proj">
              <div className="proj-head">
                <h2>{p.name}</h2>
                <span className="num proj-count">
                  {rs.length} {rs.length === 1 ? "session" : "sessions"}
                </span>
                <button className="link" onClick={() => {
                  void askText("Rename project", { initial: p.name, action: "Rename", onSubmit: (name) => onRename(p.id, name) });
                }}>Rename</button>
                <button className="link" onClick={async () => {
                  const ok = await askConfirm(`Delete “${p.name}”?`,
                    `Its ${rs.length} session${rs.length === 1 ? "" : "s"} stay, unassigned.`,
                    { action: "Delete project", danger: true });
                  if (ok) onDelete(p.id);
                }}>Delete</button>
                <button className="link" aria-expanded={orbId === p.id} onClick={() => toggleOrb(p.id)}>
                  {p.slug ? "Orb" : "Add orb"}
                </button>
              </div>
              {orbId === p.id && <OrbSection project={p} onOpen={onOpen} onChanged={onOrbChanged} />}
              {rs.length === 0
                ? <p className="proj-none">{needle ? "Nothing here matches the filter." : "Nothing here yet. Move a session in from below."}</p>
                : list(rs)}
            </section>
          );
        })}

        <section className="proj">
          <div className="proj-head">
            <h2>Unassigned</h2>
            <span className="num proj-count">
              {unassigned.length} {unassigned.length === 1 ? "session" : "sessions"}
              {(() => { const n = byRepo.filter(([k]) => k !== UNKNOWN).length; return n > 0 && ` · ${n} ${n === 1 ? "repo" : "repos"}`; })()}
            </span>
            {unassigned.length > 0 && (
              <button className="link" onClick={() => pickMany(unassigned.map((r) => r.id), !unassigned.every((r) => selected.has(r.id)))}>
                {unassigned.every((r) => selected.has(r.id)) ? "Select none" : "Select all"}
              </button>
            )}
          </div>
          {inferred.err
            ? <p className="rp-state" role="status">Inferred repos unavailable · {inferred.err} <button className="link" onClick={inferred.load}>Retry</button></p>
            : inferred.groups === null && <p className="rp-state" role="status">Reading repos…</p>}
          {unassigned.length === 0
            ? <p className="proj-none">{needle ? "Nothing unassigned matches the filter." : "Every session is in a project."}</p>
            : byRepo.map(([repo, rs]) => {
              const on = rs.every((r) => selected.has(r.id));
              return (
                <div key={repo} className="rp-repo">
                  <div className="rp-repo-head">
                    <label className="rp-pick">
                      <input type="checkbox" checked={on} disabled={moving} onChange={() => pickMany(rs.map((r) => r.id), !on)} />
                      <span className="mono hk2-name">{repo}</span>
                      <span className="visually-hidden">: select all</span>
                    </label>
                    <span className="num proj-count">{rs.length} {rs.length === 1 ? "session" : "sessions"}</span>
                    {repo !== UNKNOWN && (
                      <button className="link" disabled={moving} onClick={() => { void fromRepo(repo, rs); }}>Create project from repo…</button>
                    )}
                  </div>
                  {list(rs, true)}
                </div>
              );
            })}
        </section>
      </div>

      {ids.length > 0 && (
        <div className="rp-bar" role="region" aria-label="Selected sessions" aria-busy={moving || undefined}>
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
