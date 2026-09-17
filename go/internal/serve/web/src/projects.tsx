import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { OrbDetail, OrbFile, Project, Row } from "./types";
import { api } from "./api";
import { ProjectOrb } from "./orb";
import { STATUS, StatusMark, TESTS_FAILED_GLYPH, shownStatus } from "./status";
import { EmptyState, humanError } from "./loading";
import { hasOwnTitle, sessionTitle } from "./render";
import { Back, ago } from "./app";
import { Select } from "./select";
import { askConfirm, askText } from "./dialog";
import { idTail } from "./palette";

const clock = (iso: string) =>
  new Date(iso).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
const byName = (a: Project, b: Project) => a.name.localeCompare(b.name, undefined, { sensitivity: "base", numeric: true });

function Conversation({ row, repo, projects, onOpen, onAssign, picked, onPick, locked, twin = false }: {
  row: Row; projects: Project[];
  /** Another listed session reads the same: the id tail tells them apart, as in the sidebar. */
  twin?: boolean;
  /** The session's repo, own or inferred; omitted where a group head already names it. */
  repo?: string;
  onOpen: (id: string) => void; onAssign: (id: string, project: string) => void;
  picked: boolean; onPick: (id: string) => void;
  /** A bulk move is running: the selection it snapshotted cannot change under it. */
  locked: boolean;
}) {
  // No title yet: what the session is about beats an opaque id.
  const title = sessionTitle(row);
  const chip = twin || !hasOwnTitle(row);
  const status = shownStatus(row);
  const at = row.lastAt || row.modified;
  const current = projects.find((p) => p.id === row.project)?.name ?? "Unassigned";
  return (
    <div className={"proj-row" + (picked ? " proj-picked" : "")}>
      <label className="proj-check">
        <input type="checkbox" checked={picked} disabled={locked} onChange={() => onPick(row.id)} />
        <span className="visually-hidden">Select {title}</span>
      </label>
      {/* Title and when in one target, so a phone row is two short lines, not three. */}
      <button className="proj-open" onClick={() => onOpen(row.id)}
              aria-label={[title + (chip ? ` ${idTail(row.id)}` : ""), repo, ago(at), STATUS[status]?.label ?? status].filter(Boolean).join(", ")}>
        <span className="proj-title" title={title}>{title}{chip && <span className="mono row-id"> {idTail(row.id)}</span>}</span>
        {/* The repo column stays even when empty, so every row's age and status line up. */}
        <span className="mono proj-repo">{repo}</span>
        {/* The sidebar's format: how long ago, with the date on hover. */}
        <span className="num proj-when" title={clock(at)}>{ago(at)}</span>
      </button>
      <StatusMark status={status} />
      {/* With no project to move to, a one-option menu is a dead end. */}
      {projects.length > 0 && (
        <div className="proj-move" title={`In ${current}`}>
          {/* The pill is an action; the current project is its tooltip and the menu's check. */}
          <Select label={`Move to project, now in ${current}`} value={row.project ?? ""} align="end"
                  onChange={(p) => onAssign(row.id, p)}
                  options={[{ value: "", label: "Unassigned", short: "Move…" }, ...[...projects].sort(byName).map((p) => ({ value: p.id, label: p.name, short: "Move…" }))]} />
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
function OrbSection({ project, onOpen, onChanged, titles }: {
  project: Project; onOpen: (id: string) => void; onChanged: () => Promise<void> | void;
  titles: Record<string, string>;
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
    <ProjectOrb project={project} detail={detail} log={log} error={err} onOpen={onOpen} titles={titles}
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

const UNKNOWN = "No repo detected";
/** The bulk bar's "New project…" option: no project id can collide with it. */
const NEW = "\u0000new";

const LEDE = "A project groups sessions from any repo under one name. Moving a session only files it here; nothing inside it changes.";

function Chevron() {
  return (
    <svg className="proj-chev" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
         strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M9 6l6 6-6 6" /></svg>
  );
}

/** A group's rarer actions behind one button; destructive ones never sit inline. */
function GroupMenu({ name, items }: { name: string; items: { label: string; danger?: boolean; run: () => void }[] }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const away = (e: Event) => { if (!ref.current?.contains(e.target as Node)) setOpen(false); };
    const esc = (e: KeyboardEvent) => { if (e.key === "Escape") { setOpen(false); ref.current?.querySelector("button")?.focus(); } };
    document.addEventListener("pointerdown", away);
    document.addEventListener("keydown", esc);
    return () => { document.removeEventListener("pointerdown", away); document.removeEventListener("keydown", esc); };
  }, [open]);
  return (
    <div className="proj-menu" ref={ref}>
      <button className="btn btn-ghost btn-sm proj-more" aria-haspopup="menu" aria-expanded={open}
              aria-label={`More actions for ${name}`} onClick={() => setOpen((v) => !v)}>
        <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
          <circle cx="5" cy="12" r="1.6" /><circle cx="12" cy="12" r="1.6" /><circle cx="19" cy="12" r="1.6" />
        </svg>
      </button>
      {open && (
        <div className="overflow-menu proj-menu-pop" role="menu">
          {items.map((it) => (
            <button key={it.label} role="menuitem" className={it.danger ? "proj-menu-danger" : undefined}
                    onClick={() => { setOpen(false); it.run(); }}>{it.label}</button>
          ))}
        </div>
      )}
    </div>
  );
}

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
  // Collapsed groups, by project id ("" is Unassigned); every group starts open.
  const [folded, setFolded] = useState<Set<string>>(new Set());
  const fold = (id: string) => setFolded((prev) => { const n = new Set(prev); n.has(id) ? n.delete(id) : n.add(id); return n; });
  // Controlled by the route when the app passes it; a story drives it locally.
  const [orbLocal, setOrbLocal] = useState<string>();
  const orbId = onOrbOpen ? orbOpen : orbLocal;
  const toggleOrb = (id: string) => (onOrbOpen ?? setOrbLocal)(orbId === id ? undefined : id);

  const inferred = useInferredRepos();
  const repoOf = useMemo(() => {
    const m = new Map<string, string>();
    for (const g of inferred.groups ?? []) for (const id of g.sessions) if (!m.has(id)) m.set(id, g.repo);
    // A trailing slash left pop() an empty string; the last non-empty segment is the repo.
    return (r: Row) => r.repo?.split("/").filter(Boolean).pop() || m.get(r.id) || UNKNOWN;
  }, [inferred.groups]);

  const needle = filter.trim().toLowerCase();
  // Every section filters alike: title, the session's own repo path, or the repo its edits name.
  const shown = useMemo(() => needle
    ? rows.filter((r) => sessionTitle(r).toLowerCase().includes(needle) || (r.repo ?? "").toLowerCase().includes(needle)
        || (repoOf(r) !== UNKNOWN && repoOf(r).toLowerCase().includes(needle)))
    : rows, [rows, needle, repoOf]);

  const byProject = useMemo(() => {
    const m = new Map<string, Row[]>();
    for (const r of shown) {
      const k = r.project ?? "";
      if (!m.has(k)) m.set(k, []);
      m.get(k)!.push(r);
    }
    return m;
  }, [shown]);
  // Unfiltered counts, so a filtered head reads "2 of 9 match", not "2 sessions".
  const totals = useMemo(() => {
    const m = new Map<string, number>();
    for (const r of rows) m.set(r.project ?? "", (m.get(r.project ?? "") ?? 0) + 1);
    return m;
  }, [rows]);
  // One phrasing for every section head: the count, then how many repos those sessions span.
  const countLabel = (k: string, rs: Row[]) => {
    const n = rs.length;
    const repos = new Set(rs.map(repoOf).filter((x) => x !== UNKNOWN)).size;
    return (needle ? `${n} of ${totals.get(k) ?? 0} match` : `${n} ${n === 1 ? "session" : "sessions"}`)
      + (repos > 0 ? ` · ${repos} ${repos === 1 ? "repo" : "repos"}` : "");
  };
  // Titles that read the same across the page get the id tail beside them.
  const twins = useMemo(() => {
    const seen = new Map<string, number>();
    for (const r of shown) { const t = sessionTitle(r).toLowerCase(); seen.set(t, (seen.get(t) ?? 0) + 1); }
    return (r: Row) => (seen.get(sessionTitle(r).toLowerCase()) ?? 0) > 1;
  }, [shown]);
  const titles = useMemo(() => Object.fromEntries(rows.map((r) => [r.id, r.title])), [rows]);
  const sorted = useMemo(() => [...projects].sort(byName), [projects]);

  const unassigned = byProject.get("") ?? [];
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
                  picked={selected.has(r.id)} onPick={pick} locked={moving} twin={twins(r)} />
  ));

  return (
    <div className={"thread" + (ids.length > 0 ? " proj-picking" : "")}>
      <header className="thread-head page-head proj-page-head">
        <Back onBack={onBack} />
        <div className="head-main">
          <h1 title={LEDE}>Projects</h1>
          <span className="head-repo">
            {projects.length} configured {projects.length === 1 ? "project" : "projects"} · {detected} detected {detected === 1 ? "repo" : "repos"}
          </span>
        </div>
        {/* With no projects the empty state carries the one New project button. */}
        {(projects.length > 0 || needle) && (
          <div className="head-side">
            <button className="btn btn-primary" onClick={() => { void createProject(); }}>New project…</button>
          </div>
        )}
      </header>

      <div className="scroll proj-body">
        <div className="proj-filter">
          <input className="field" type="search" value={filter} placeholder="Filter sessions by title or repo"
                 aria-label="Filter sessions" onChange={(e) => setFilter(e.target.value)} />
        </div>
        {needle && shown.length === 0 && <p className="proj-nomatch" role="status">No sessions match “{filter.trim()}” <button type="button" className="btn btn-ghost btn-sm" onClick={() => setFilter("")}>Clear filter</button></p>}
        {projects.length === 0 && !needle && (
          <EmptyState card title="No projects yet" action={{ label: "New project…", onClick: () => { void createProject(); } }}>
            {LEDE}
          </EmptyState>
        )}

        {sorted.map((p) => {
          const rs = byProject.get(p.id) ?? [];
          // A filter hides projects with nothing matching, unless its orb is open.
          if (needle && rs.length === 0 && orbId !== p.id) return null;
          const all = totals.get(p.id) ?? 0;
          return (
            <section key={p.id} className="proj">
              <div className="proj-head">
                <button className="proj-fold" aria-expanded={!folded.has(p.id)} onClick={() => fold(p.id)}>
                  <h2>{p.name}</h2>
                  <span className="num proj-count">{countLabel(p.id, rs)}</span>
                  <Chevron />
                </button>
                {!p.slug && (
                  <button className="btn btn-sm" aria-expanded={orbId === p.id} onClick={() => toggleOrb(p.id)}>Add orb</button>
                )}
                <GroupMenu name={p.name} items={[
                  ...(p.slug ? [{ label: orbId === p.id ? "Hide orb" : "Orb", run: () => toggleOrb(p.id) }] : []),
                  { label: "Rename…", run: () => { void askText("Rename project", { initial: p.name, action: "Rename", onSubmit: (name) => onRename(p.id, name) }); } },
                  { label: "Delete…", danger: true, run: async () => {
                    const ok = await askConfirm(`Delete “${p.name}”?`,
                      `Its ${all} session${all === 1 ? "" : "s"} stay, unassigned.`,
                      { action: "Delete project", danger: true });
                    if (ok) onDelete(p.id);
                  } },
                ]} />
              </div>
              {orbId === p.id && <OrbSection project={p} onOpen={onOpen} onChanged={onOrbChanged} titles={titles} />}
              {folded.has(p.id) ? null : rs.length === 0
                ? <p className="proj-none">{needle ? "Nothing here matches the filter." : "Nothing here yet. Move a session in from below."}</p>
                : list(rs)}
            </section>
          );
        })}

        <section className="proj">
          <div className="proj-head">
            <button className="proj-fold" aria-expanded={!folded.has("")} onClick={() => fold("")}>
              <h2>Unassigned</h2>
              <span className="num proj-count">{countLabel("", unassigned)}</span>
              <Chevron />
            </button>
          </div>
          {inferred.err
            ? (
              <p className="proj-banner" role="status">
                <svg className="state-mark" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5"
                     strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">{TESTS_FAILED_GLYPH}</svg>
                Inferred repos unavailable · {humanError(inferred.err)}
                <button className="link" onClick={inferred.load}>Retry</button>
              </p>
            )
            : inferred.groups === null && <p className="rp-state" role="status">Reading repos…</p>}
          {folded.has("") ? null : unassigned.length === 0
            ? <p className="proj-none">{needle ? "Nothing unassigned matches the filter." : rows.length === 0 ? "No sessions yet." : "Every session is in a project."}</p>
            : byRepo.map(([repo, rs]) => {
              const on = rs.every((r) => selected.has(r.id));
              return (
                <div key={repo} className="rp-repo">
                  <div className="rp-repo-head">
                    <label className="rp-pick">
                      <input type="checkbox" checked={on} disabled={moving} onChange={() => pickMany(rs.map((r) => r.id), !on)} />
                      <span className={repo === UNKNOWN ? "rp-norepo" : "mono hk2-name"}>{repo}</span>
                      <span className="visually-hidden">: select all</span>
                    </label>
                    <span className="num proj-count">{rs.length} {rs.length === 1 ? "session" : "sessions"}</span>
                    {/* rs is already filtered; the head above carries "N of M match". */}
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
          {/* Choosing a project is the move; "New project…" files the selection into what it makes. */}
          <Select label="Move to project" value="" align="start" placeholder="Move to project"
                  options={[...sorted.map((p) => ({ value: p.id, label: p.name })), { value: NEW, label: "New project…" }]}
                  onChange={(v) => { if (moving) return; if (v === NEW) void createProject(); else void assign(v); }} />
          {failed && (
            <span className="err rp-err" role="alert">
              {failed.n} not moved <button className="link" disabled={moving} onClick={() => { void assign(failed.project); }}>Retry</button>
            </span>
          )}
          <button className="btn btn-ghost rp-clear" disabled={moving} onClick={() => { setSelected(new Set()); setFailed(null); }}>Clear</button>
        </div>
      )}
    </div>
  );
}
