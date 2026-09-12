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
  cwd: string;
  repo?: string;
  branch?: string;
  status: Status;
  live: boolean;
  archived: boolean;
  entries: number;
  modified: string;
  ask?: Ask;
  model?: string;
  effort?: string;
  project?: string;
}

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
