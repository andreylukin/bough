// Mirrors the backend contract in src/schema/parts.ts.
export type Role = "user" | "supervisor" | "worker" | "system";

export type Part =
  | { type: "text"; text: string }
  | { type: "reasoning"; text: string }
  | { type: "tool_call"; id: string; name: string; input: unknown }
  | { type: "tool_result"; callId: string; output: unknown; isError: boolean };

export interface Message {
  id: string;
  sessionId: string;
  role: Role;
  parts: Part[];
  pending: boolean;
  createdAt: number;
}

export type SessionKind = "root" | "fork" | "worker" | "compaction" | "subagent";

export interface Session {
  id: string;
  parentId: string | null;
  title: string;
  kind: SessionKind;
  createdAt: number;
  // The session's sandbox workspace (jj repo / snapshot root). Optional: set at
  // creation; the turn runner falls back to BOUGH_WORKSPACE/cwd when absent.
  workspace?: string | null;
  // Branch lineage (task #18): the session this fork/compaction branched from, and the
  // origin turn it diverged at. Distinct from parentId (forks are modeled as siblings).
  // The map draws a connector from originMessageId's dot to this head. Absent on roots.
  originId?: string | null;
  originMessageId?: string | null;
  // A turn is in flight (server-computed on GET /sessions; kept live from
  // message.started/finished events in the store).
  busy?: boolean;
  // How the most recent turn ended (server-computed; kept live via turn.finished).
  // Absent if the session never ran a turn. Drives ✓/✗ status affixes.
  lastTurnStatus?: "running" | "done" | "error" | "orphaned" | "interrupted";
  // Prompt-cache visibility (mirrors schema/parts.ts): last prompt size, the share
  // of it in the provider's prompt cache, and when the last LLM round finished.
  // Warm/cold is derived client-side from lastLlmAt + the ~5-min sliding TTL.
  contextTokens?: number | null;
  cachedTokens?: number | null;
  lastLlmAt?: number | null;
  // Client-only: a turn finished while this session wasn't open — needs a look.
  // Set by the store on message.finished, cleared when the session is opened.
  unseen?: boolean;
}

// SSE envelope. `data` shape depends on `type`.
export interface BoughEvent {
  type: string;
  sessionId?: string;
  seq: number;
  ts: number;
  data: unknown;
}

// A single outbound request row for the Network rail. The backend will emit
// `net.request` events with (at least) this shape; rendered defensively until then.
export interface NetRequest {
  id: string;
  /** Branch that owns this egress; absent for pre-attribution rows. */
  sessionId?: string;
  host: string;
  verb?: string;
  action: string;
  verdict: "allowed" | "denied" | "pending";
  reason?: string;
  requestedBy?: string;
  /** Facet fields — the classifier's parsed view (e.g. k8s resource/namespace). */
  fields?: Record<string, unknown>;
  /** Local-worker one-liner ("Creates a fork of repo X") — advisory, may lag. */
  annotation?: string;
  ts: number;
}

// ---- policy bundles (GET /net/bundles) ------------------------------------
// Mirrors src/net/bundles.ts BundleManifest as the server serializes it (render() is
// omitted; `installed` is added by the route). Param `type` drives the UI control.
export type BundleParamType = "string" | "host" | "hostList" | "bool";

export interface BundleParamSpec {
  name: string;
  description: string;
  type: BundleParamType;
  required: boolean;
  default?: string | string[] | boolean;
}

export interface BundleCred {
  handle: string;
  type: string;
  description: string;
}

export interface BundleSummary {
  name: string;
  version: string;
  description: string;
  params: BundleParamSpec[];
  credentials: BundleCred[];
  installed: boolean;
}

// ---- changes review (GET /sessions/:id/changes) ---------------------------
// Mirrors src/schema/changes.ts. A Hunk's `lines` are raw unified-diff lines with
// their leading ` `/`+`/`-` markers kept; the UI parses them for display.
export type FileStatus = "added" | "modified" | "deleted";
export type ChangeSource = "jj" | "clonefile";

export interface WireHunk {
  header: string;
  lines: string[];
}
export interface WireFileDiff {
  path: string;
  status: FileStatus;
  hunks: WireHunk[];
}
export interface WireDiff {
  source: ChangeSource;
  files: WireFileDiff[];
}
