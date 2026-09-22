// The one place that knows the wire. Everything else takes typed values.
import type { Ask, Event, Line, OrbBuild, OrbDetail, OrbFile, OrbRemovePlan, OrbState, OrbSummary, Project, ProjectDetail, Row, SessionMode } from "./types";

// The control room's UI is embedded in the binary it was served from,
// so a tab left open across `bough update` keeps running the old JS
// while the server has the new one — the update looks like it did not
// land. Every answer names the build that gave it; the first one this
// page saw is the one it is running.
let loadedBuild = "";
let onNewBuild: (() => void) | undefined;
/** Called once when the server starts answering from a different build. */
export function watchBuild(f: () => void) { onNewBuild = f; }

function noteBuild(res: Response) {
  const got = res.headers.get("X-Bough-Build");
  if (!got) return;
  if (!loadedBuild) { loadedBuild = got; return; }
  if (got !== loadedBuild) {
    const f = onNewBuild;
    onNewBuild = undefined; // ask once
    f?.();
  }
}

async function req<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(path, {
    ...init,
    headers: init?.body ? { "content-type": "application/json" } : undefined,
  });
  noteBuild(res);
  if (!res.ok) {
    // The API answers errors as {"error": "..."} — surface that text,
    // not a bare status code, so the UI can say what actually failed.
    let detail = `${res.status} ${res.statusText}`;
    try {
      const body = (await res.json()) as { error?: string };
      if (body.error) detail = body.error;
    } catch {
      /* a non-JSON error body is not worth masking the status */
    }
    // The status rides along: a 404 is "not here", anything else is "could not ask".
    throw Object.assign(new Error(detail), { status: res.status });
  }
  return (await res.json()) as T;
}

const post = (path: string, body?: unknown) =>
  req<{ ok: true }>(path, { method: "POST", body: body ? JSON.stringify(body) : undefined });

export type KeyState = "ok" | "rejected" | "unset" | "unknown";
export interface SetupProvider { name: string; env: string; set: boolean }
/** checkout is the git checkout a session started in path may write; absent means read-only. */
export interface SetupFolder { path: string; exists: boolean; checkout?: string }
export interface Setup { providers: SetupProvider[]; envFile: string; home: string; folder: SetupFolder }
export interface Change { path: string; add: number; del: number; new?: boolean }
export interface Edit extends Change { patch: boolean }
/** Session edits (what its turns changed), one turn's edits, or the working tree (everything uncommitted). */
export type Scope = "session" | "turn" | "tree";
/** One line of a session's running log, written by the small model per turn. */
export interface TurnLine { turn: number; text: string; at: string; test?: { cmd: string; exit: number } }

export const api = {
  /** The session's running log, one caveman line per finished turn. */
  turns: (id: string) => req<{ turns: TurnLine[] }>(`/api/sessions/${id}/turns`).then((r) => r.turns ?? []),
  /** What is uncommitted in the session's working tree, live from git. */
  changes: (id: string) => req<{ repo: boolean; files: Change[] }>(`/api/sessions/${id}/changes`),
  /** One file's unified diff against HEAD, 3 lines of context. */
  diff: (id: string, path: string, scope: Scope = "tree", turn?: number) => req<{ diff: string }>(`/api/sessions/${id}/diff?path=${encodeURIComponent(path)}${scope === "tree" ? "" : "&scope=" + scope}${scope === "turn" ? "&turn=" + turn : ""}`).then((r) => r.diff),
  /** The files this session's turns changed, measured from its first checkpoint; patch false when none was recorded. */
  edits: (id: string, turn?: number) => req<{ repo: boolean; files: Edit[] }>(`/api/sessions/${id}/edits${turn === undefined ? "" : "?turn=" + turn}`),
  /** Ask the session to stop one of its background jobs. */
  killJob: (id: string, job: number) => post(`/api/sessions/${id}/jobs/${job}/kill`),
  /** Mark what a session has recorded so far as seen, taking it out of Needs you. */
  ack: (id: string) => post(`/api/sessions/${id}/ack`),

  /** Where a new session starts, from the server that knows. */
  home: () => req<{ home: string }>("/api/health").then((r) => r.home ?? ""),

  /** First-run state: which providers have a key, and whether a folder (default: where serve started) is a writable checkout. */
  setup: (cwd?: string) => req<Setup>(`/api/setup${cwd ? `?cwd=${encodeURIComponent(cwd)}` : ""}`),
  /** Ask the provider whether its key is accepted: a key that is only present used to read as working. */
  checkKey: (provider: string) => req<{ state: KeyState; detail?: string }>(`/api/setup/check?provider=${encodeURIComponent(provider)}`),
  /** Record a provider key in ~/.bough/env; sessions started afterwards use it. */
  setKey: (provider: string, key: string) =>
    req<{ providers: SetupProvider[] }>("/api/setup/key", { method: "POST", body: JSON.stringify({ provider, key }) }).then((r) => r.providers),

  sessions: (all = false) =>
    req<{ sessions: Row[] }>(`/api/sessions${all ? "?all=1" : ""}`).then((r) => r.sessions),

  /**
   * A session and its transcript. `since` asks for entries after a
   * history seq, which is how the live view catches up without
   * refetching the whole thread.
   */
  session: (id: string, since = 0) =>
    req<{ session: Row; entries: Line[] }>(
      `/api/sessions/${id}${since > 0 ? `?since=${since}` : ""}`,
    ),

  /** mode omitted is local; project is the slug of the project whose orb it runs in. */
  create: (cwd: string, prompt: string, mode?: SessionMode, project?: string) =>
    req<{ session: Row }>("/api/sessions", {
      method: "POST",
      body: JSON.stringify(mode === "project" ? { cwd, prompt, mode, project } : { cwd, prompt }),
    }).then((r) => r.session),

  /** A thread the main thread hands work to: a child session of it, in the project, started on prompt. */
  createThread: (cwd: string, prompt: string, slug: string, spawnedBy: string) =>
    req<{ session: Row; queued: boolean }>("/api/sessions", {
      method: "POST",
      body: JSON.stringify({ cwd, prompt, slug, spawnedBy }),
    }).then((r) => r.session),

  prompt: (id: string, text: string) => post(`/api/sessions/${id}/prompt`, { text }),
  /** Save a pasted image on the server; the path is what a prompt references. */
  attach: async (img: Blob) => {
    const res = await fetch("/api/attachments", { method: "POST", headers: { "content-type": img.type }, body: img });
    const body = (await res.json().catch(() => ({}))) as { path?: string; error?: string };
    if (!res.ok || !body.path) throw new Error(body.error ?? `${res.status} ${res.statusText}`);
    return body.path;
  },
  /** Save any other file into the session's scratchpad; returns its path. */
  attachFile: async (id: string, f: File) => {
    const res = await fetch(`/api/sessions/${id}/files?name=${encodeURIComponent(f.name)}`, { method: "POST", body: f });
    const body = await res.json().catch(() => ({}));
    if (!res.ok) throw new Error(body.error || `HTTP ${res.status}`);
    return body.path as string;
  },
  attachmentURL: (path: string) => `/api/attachments?path=${encodeURIComponent(path)}`,
  answer: (id: string, text: string, ask?: string) => post(`/api/sessions/${id}/answer`, { text, ask }),
  interrupt: (id: string) => post(`/api/sessions/${id}/interrupt`),
  rename: (id: string, title: string) => post(`/api/sessions/${id}/rename`, { title }),
  /** stopChildren stops its running and queued background agents first. */
  archive: (id: string, opts?: { stopChildren?: boolean }) =>
    post(`/api/sessions/${id}/archive`, opts?.stopChildren ? { stopChildren: true } : undefined),
  /** The background agents a session started, queued ones included. */
  children: (id: string) => req<{ children: Row[] }>(`/api/sessions/${id}/children`).then((r) => r.children ?? []),
  /** A background agent's status, title and last reply, read on behalf of the parent that started it. */
  agent: (id: string, parent: string) =>
    req<{ status: string; title: string; reply: string; project: string; spawnedBy: string }>(`/api/sessions/${id}/agent?parent=${encodeURIComponent(parent)}`),
  /** Interrupt a running background agent, or drop a queued one. */
  stopAgent: (id: string) => req<{ ok: true; was: "running" | "queued" | "idle" }>(`/api/sessions/${id}/stop`, { method: "POST", body: "{}" }),
  model: (id: string, model: string, plugin?: string) => post(`/api/sessions/${id}/model`, { model, plugin }),
  effort: (id: string, effort: string) => post(`/api/sessions/${id}/effort`, { effort }),
  assign: (id: string, project: string) => post(`/api/sessions/${id}/project`, { project }),

  projects: () => req<{ projects: Project[] }>("/api/projects").then((r) => r.projects ?? []),
  newProject: (name: string) =>
    req<{ project: Project }>("/api/projects", { method: "POST", body: JSON.stringify({ name }) })
      .then((r) => r.project),
  renameProject: (slug: string, name: string) => post(`/api/projects/${slug}/rename`, { name }),
  /** Removes the definition directory and everything keyed by the slug; conversations stay. */
  deleteProject: (slug: string) => req<{ ok: true }>(`/api/projects/${slug}`, { method: "DELETE" }),
  /** One project's page: its main thread, its threads and their orbs. Reading never starts anything. */
  project: (slug: string) => req<ProjectDetail>(`/api/projects/${slug}`),
  /** Send to the project, which is its main thread — created on the first message. */
  messageProject: (slug: string, text: string) =>
    req<{ ok: true; main: string }>(`/api/projects/${slug}/message`, { method: "POST", body: JSON.stringify({ text }) }),
  orb: (slug: string) => req<OrbDetail>(`/api/projects/${slug}/orb`),
  putOrbFile: (slug: string, name: OrbFile, text: string) =>
    req<{ ok: true; orb: OrbSummary }>(`/api/projects/${slug}/orb/files/${encodeURIComponent(name)}`, { method: "PUT", body: JSON.stringify({ text }) }),
  buildOrb: (slug: string) => req<{ build: OrbBuild }>(`/api/projects/${slug}/orb/build`, { method: "POST", body: "{}" }),
  buildLog: (slug: string, offset: number) =>
    req<{ text: string; offset: number; state: OrbBuild["state"] }>(`/api/projects/${slug}/orb/build/log?offset=${offset}`),
  sessionOrb: (id: string) => req<{ orb: OrbState | null }>(`/api/sessions/${id}/orb`).then((r) => r.orb),
  /** Why a session's orb failed: its error and the tail of resume.log. */
  sessionOrbLog: (id: string) => req<{ status: string; error: string; phase?: string; log?: string; image: string; text: string }>(`/api/sessions/${id}/orb/log`),
  /** The image build a session waits on, from offset: poll while its orb is building. */
  sessionBuildLog: (id: string, offset: number) =>
    req<{ text: string; offset: number; state: string; status: string; startedAt?: string; endedAt?: string }>(`/api/sessions/${id}/orb/build/log?offset=${offset}`),
  stopOrb: (id: string) => post(`/api/sessions/${id}/orb/stop`),
  orbRemovePlan: (id: string) => req<{ plan: OrbRemovePlan; live: boolean }>(`/api/sessions/${id}/orb/remove`),
  removeOrb: (id: string) => req<{ ok: true; plan: OrbRemovePlan }>(`/api/sessions/${id}/orb`, { method: "DELETE" }),
  unarchive: (id: string) => post(`/api/sessions/${id}/unarchive`),
};

/**
 * Subscribe to a session's live events.
 *
 * The server deliberately sends unnamed frames, so one `message`
 * handler sees every kind — including kinds added after this code was
 * written. Returns an unsubscribe.
 */
export function subscribe(id: string, onEvent: (ev: Event) => void): () => void {
  const src = new EventSource(`/api/sessions/${id}/events`);
  src.onmessage = (m) => {
    try {
      onEvent(JSON.parse(m.data) as Event);
    } catch {
      /* a malformed frame is dropped, never fatal to the stream */
    }
  };
  return () => src.close();
}

export type { Ask, Event, Line, Project, ProjectDetail, Row };
