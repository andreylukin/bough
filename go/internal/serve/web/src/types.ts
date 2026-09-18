// Mirrors the Go wire types in internal/serve/{api,status,supervisor}.go.
// Hand-written on purpose: five types over eleven endpoints did not
// justify a protobuf or OpenAPI toolchain in a repo where `go build` is
// the only build step. If these drift, tygo generates them from the Go
// structs without any of that machinery.

export type Status =
  | "idle"
  | "running"
  | "interrupted"
  | "needs-you"
  | "error"
  | "stopped"
  | "done"
  /** A background agent waiting for a running slot; never derived from history. */
  | "queued";

export interface Ask {
  id: string;
  text: string;
  options: string[];
  seq: number;
  /** tools.secret: the answer is a credential, typed into its own password field. */
  secret?: boolean;
}

/** A named grouping of conversations. A label, not a container. */
export interface Project {
  id: string;
  name: string;
  /** The ~/.bough/projects/<slug> definition this label runs its orb from; absent for a label only. */
  slug?: string;
  orb?: OrbSummary;
}

/** local runs on the host and changes no files; project runs in the project's orb. */
export type SessionMode = "local" | "project";
export type OrbStatus = "" | "building" | "starting" | "running" | "stopped" | "failed";
export interface OrbSummary { slug: string; image: string; built: boolean; build?: string; error?: string; /** Repo names project.yml declares. */ repos?: string[] }
/** One timed step of an orb start; endedAt is absent while it runs. */
export interface OrbPhase { name: string; startedAt: string; endedAt?: string; error?: string }
export interface OrbState { phase?: string; phases?: OrbPhase[]; session: string; project: string; status: OrbStatus; image?: string; container?: string; worktrees?: Record<string, string>; primary?: string; error?: string; updatedAt: string; up?: boolean; /** The session's title, archived or not. */ title?: string; /** "legacy": created before proxy tokens, so its host proxy and relay are open to the bridge. */ proxyAuth?: "token" | "legacy"; /** The container's bridge address, set each start. */ ip?: string; /** project.yml ports as created: 127.0.0.1:host → guest; error = skipped (host port in use). */ ports?: OrbPort[]; /** Portals the session opened on the running container; live = something still accepts on the host port. */ portals?: OrbPortal[] }
export interface OrbPort { host: number; guest: number; error?: string }
/** One portal: a loopback port that reaches `guest` inside the orb. The
 *  listener lives in the owning session's process, so a recorded portal
 *  outlives it; `live` is whether anything still answers. */
export interface OrbPortal { host: number; guest: number; name?: string; url: string; live: boolean }
/** What Remove orb deletes and keeps (GET /api/sessions/:id/orb/remove). */
export interface OrbRemovePlan { session: string; project: string; status: OrbStatus; container?: string; dir: string; worktrees: string[]; /** Worktrees with uncommitted changes; removal refuses while any remain. */ dirty?: string[]; bytes: number; branches: { repo: string; gitDir: string; branch: string; delete: boolean; reason: string }[] }
export interface OrbBuild { tag: string; hash: string; state: "" | "building" | "ok" | "failed"; startedAt: string; endedAt?: string; error?: string }
export type OrbFile = "project.yml" | "Dockerfile" | "setup.sh" | "resume.sh";
export interface OrbDetail { project: Project; files: Record<OrbFile, string>; hash: string; orb: OrbSummary; build: OrbBuild; orbs: OrbState[]; runtime: { name: string; available: boolean; error?: string }; preflight?: PreflightCheck[] }
/** One thing a session start needs, checked when the orb panel loads. */
export interface PreflightCheck { kind: "runtime" | "clone" | "gh" | "secret"; name: string; status: "ok" | "warn" | "fail"; detail?: string }

/** One session, as GET /api/sessions returns it. */
export interface Row {
  id: string;
  title: string;
  /** A few sentences on what the session is about; absent until the small model has named it. */
  summary?: string;
  cwd: string;
  repo?: string;
  branch?: string;
  status: Status;
  live: boolean;
  archived: boolean;
  /** A run nobody started by hand (wiki ingest, bench, test); the sidebar folds these away. */
  background?: boolean;
  /** Never sent a message: left out of the sidebar unless open, live or searched for. */
  empty?: boolean;
  entries: number;
  modified: string;
  /** When the session last wrote an entry; recency reads this, not the file mtime. */
  lastAt: string;
  ask?: Ask;
  model?: string;
  effort?: string;
  project?: string;
  /** Background jobs still running. */
  jobs?: Job[];
  /** The prompt cache after the last turn that reported one. */
  cache?: Cache;
  /** Why this session needs you ("failed", "interrupted", "tests failed"); absent once marked seen. */
  trouble?: string;
  /** The last test run exited non-zero; stays after Seen, which is not a fix. */
  testsFailed?: boolean;
  /** When that last test run's result was recorded: test status is aged from this. */
  testsAt?: string;
  /** Lines in the session's running log (GET /api/sessions/{id}/turns); absent when none. */
  turns?: number;
  /** Absent from a server older than orbs: read as local. */
  mode?: SessionMode;
  /** The git checkout a local session may edit; absent means read-only (or a project session). */
  writable?: string;
  /** up: the container runs, whatever the status (a failed setup can leave it up). */
  orb?: { project: string; status: OrbStatus; up?: boolean };
  /** The session that started this one as a background agent. */
  spawnedBy?: string;
  /** A background agent waiting for a running slot (status "queued"). */
  queued?: boolean;
  /** Background agents this session started; absent when it started none. */
  agents?: { running: number; queued: number; total: number };
  /** A failed background agent's first error line (children listing only). */
  error?: string;
}

export interface Job { id: number; cmd: string; started: string }
/** ttl is in seconds; read/write/in are the tokens of the turn ending at `at`. */
export interface Cache { at: string; ttl: number; read: number; write: number; in: number }

/** One transcript entry, from GET /api/sessions/{id}. */
export interface Line {
  seq: number;
  at: string;
  kind: string;
  text: string;
  data?: Record<string, unknown>;
}

/** One streamed event, from GET /api/sessions/{id}/events. */
export interface Event {
  session: string;
  seq: number;
  at: string;
  kind: string;
  text: string;
  extra?: Record<string, unknown>;
}
