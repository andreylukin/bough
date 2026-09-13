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
  | "done";

export interface Ask {
  id: string;
  text: string;
  options: string[];
  seq: number;
}

/** A named grouping of conversations. A label, not a container. */
export interface Project {
  id: string;
  name: string;
}

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
  /** Lines in the session's running log (GET /api/sessions/{id}/turns); absent when none. */
  turns?: number;
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
