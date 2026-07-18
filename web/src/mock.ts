// Mock data layer. The backend (src/, another agent) isn't live yet, so every screen
// renders populated from here. Toggled by USE_MOCK in App.tsx (default on in dev).
//
// The view types below SUPPLEMENT the pinned backend contract in types.ts — they are
// UI-only shapes (heads outline, worker activity, diffs, bundles, the heads-map graph)
// that will eventually be derived from real sessions/events. types.ts stays untouched.
import type { Message, NetRequest, Session } from "./types";

// ---- view types (UI-only) -------------------------------------------------

export interface Head {
  id: string;
  glyph: string; // ⎇ ↩ ◇ ⊟ ◷
  label: string;
  meta: string;
  active?: boolean;
  status?: "running" | "done" | "failed" | "compacted" | "idle";
  // A turn is in flight — the card shows a pulsing dot.
  busy?: boolean;
  // A turn finished while the session wasn't open — the card shows a solid dot.
  unseen?: boolean;
  // Epoch ms of the session's last LLM round — the prompt-cache warmth clock. The
  // card shows a decaying ⚡ countdown while now − cacheAt < the provider cache TTL.
  cacheAt?: number;
  // cachedTokens / contextTokens of that round (0..1) — the prompt's cached share.
  cacheShare?: number;
  // Sessions branched from this one (forks/compactions/subagents, via originId) —
  // the sidebar nests them under this head behind a collapsible toggle.
  children?: Head[];
}

export interface OutlineNode {
  label: string;
  state: "done" | "running" | "todo";
  note?: string; // trailing "✓" etc.
  // Who spoke this turn — drives the outline dot's color (you vs. the AI).
  role?: "user" | "supervisor" | "worker" | "system";
}

export interface WorkerActivity {
  name: string;
  status: "done" | "running" | "failed";
  meta: string;
}

// Inline activity attached to a supervisor turn (folds collapsed by default).
export interface ActivityGroup {
  messageId: string;
  workers?: WorkerActivity[];
  toolSummary?: string;
  running?: { label: string };
}

export interface DiffLine {
  kind: " " | "-" | "+";
  oldNo?: number;
  newNo?: number;
  text: string;
}
export interface Hunk {
  header: string;
  status: "applied" | "pending";
  lines: DiffLine[];
}
export interface DiffFile {
  path: string;
  status: "M" | "A" | "D";
  added: number;
  removed: number;
  meta: string;
  applied: "none" | "partial" | "full";
  hunks: Hunk[];
  // Live-only: which snapshot source the file came from (jj = repo, clonefile = config).
  // Drives the source label in the rail and the apply call. Omitted in mock.
  source?: "jj" | "clonefile" | "shadow";
}

export interface BundleParam {
  kind: "text" | "select" | "multiselect" | "toggle" | "number";
  label: string;
  hint?: string;
  value?: string;
  selected?: string[];
  available?: string[];
  on?: boolean;
}
export interface Bundle {
  id: string;
  name: string;
  glyph: string;
  publisher: string;
  version: string;
  verified: boolean;
  desc: string;
  installs: string;
  state: "install" | "installed" | "configuring";
  params?: BundleParam[];
}

export interface MapNode {
  id: string;
  label: string;
  x: number;
  y: number;
  kind: "head" | "turn";
  tone: "green" | "muted" | "dead" | "compacted";
  head?: boolean; // the live HEAD gets the big pulse
  tip?: { text: string; tone: "green" | "red" | "muted" };
}
export interface MapEdge {
  from: string;
  to: string;
  tone: "green" | "muted" | "dead" | "compacted";
}

// ---- mock instances -------------------------------------------------------

const now = Date.now();
const ago = (m: number) => now - m * 60_000;

export const heads: Head[] = [
  {
    id: "h-main",
    glyph: "⎇",
    label: "main · migrate-auth",
    meta: "5 turns · active now",
    active: true,
    status: "running",
  },
  { id: "h-edit-v3", glyph: "↩", label: 'edit · "use v3 token format"', meta: "fork · 3 turns · 6m", status: "idle" },
  { id: "h-worker-a", glyph: "◇", label: "worker · middleware A", meta: "attempt · 4 turns · 4m", status: "idle" },
  { id: "h-compact", glyph: "⊟", label: "compacted · auth research", meta: "18 → 1 · summary · 22m", status: "compacted" },
  { id: "h-edit-patch", glyph: "↩", label: 'edit · "skip PR, patch only"', meta: "fork · 1 turn · 14m", status: "idle" },
];

export const outline: OutlineNode[] = [
  { label: "Task — migrate auth → v2", state: "done", role: "user" },
  { label: "Plan · 5 steps", state: "done", role: "supervisor" },
  { label: "Encoder updated", state: "done", note: "✓", role: "worker" },
  { label: "Middleware updated", state: "done", note: "✓", role: "worker" },
  { label: "Test suite — running", state: "running", role: "supervisor" },
];

export const sessions: Session[] = heads.map((h) => ({
  id: h.id,
  parentId: h.id === "h-main" ? null : "h-main",
  title: h.label,
  kind: h.glyph === "⎇" ? "root" : h.glyph === "◇" ? "worker" : h.glyph === "⊟" ? "compaction" : "fork",
  createdAt: ago(20),
}));

export const thread: Message[] = [
  {
    id: "m1",
    sessionId: "h-main",
    role: "user",
    pending: false,
    createdAt: ago(9),
    parts: [
      {
        type: "text",
        text:
          "Migrate the auth service to the new token format (v2). Open a PR once the suite passes — don't touch prod config.",
      },
    ],
  },
  {
    id: "m2",
    sessionId: "h-main",
    role: "supervisor",
    pending: false,
    createdAt: ago(8),
    parts: [
      {
        type: "text",
        text:
          "Plan set: rewrite the token encoder, update the auth middleware, migrate the affected tests, run the suite, then open the PR. Spawning two workers to parallelise the encoder and middleware edits.",
      },
    ],
  },
  {
    id: "m3",
    sessionId: "h-main",
    role: "supervisor",
    pending: true,
    createdAt: ago(1),
    parts: [
      {
        type: "text",
        text:
          "Middleware migrated. Running the test suite now — I'll stage the diff for your review and hold the PR until you approve. One outbound call needs your sign-off in the rail.",
      },
    ],
  },
];

// Inline activity that folds under the supervisor turns.
export const activity: ActivityGroup[] = [
  {
    messageId: "m2",
    workers: [
      { name: "token-encoder", status: "done", meta: "3 files · 8 cmds" },
      { name: "middleware", status: "running", meta: "2 files" },
    ],
    toolSummary: "11 commands · 2 network calls · 5 files staged",
  },
  { messageId: "m3", running: { label: "npm test — running · 42 / 118" } },
];

export const net: NetRequest[] = [
  { id: "n1", host: "api.github.com", verb: "POST", action: "gh pr create · octo/auth", verdict: "allowed", ts: now },
  { id: "n2", host: "169.254.169.254", verb: "GET", action: "metadata · off-allowlist", verdict: "denied", reason: "off-allowlist", ts: now - 3000 },
  { id: "n3", host: "kubernetes.prod", verb: "DEL", action: "kubectl delete secret · blocked", verdict: "denied", reason: "destructive", ts: now - 8000 },
  { id: "n4", host: "registry.npmjs.org", verb: "GET", action: "@auth/token-v2 · 34kb", verdict: "allowed", ts: now - 11000 },
  { id: "n5", host: "api.github.com", verb: "GET", action: "gh actions/runs · 200", verdict: "allowed", ts: now - 15000 },
  { id: "n6", host: "api.github.com", verb: "GET", action: "gh repo · octo/auth · 200", verdict: "allowed", ts: now - 18000 },
];

export const pending: NetRequest = {
  id: "p1",
  host: "api.github.com",
  verb: "DELETE",
  action: "gh repo delete octo/legacy-auth",
  verdict: "pending",
  reason: "Classified destructive. The github bundle allows repo read/write but holds deletes for approval.",
  requestedBy: "cleanup",
  ts: now,
};

export const diffs: DiffFile[] = [
  {
    path: "auth/token.js",
    status: "M",
    added: 18,
    removed: 6,
    meta: "2 hunks · 1 applied",
    applied: "partial",
    hunks: [
      {
        header: "@@ -8,6 +8,10 @@ export function encode(claims)",
        status: "applied",
        lines: [
          { kind: " ", oldNo: 8, newNo: 8, text: "  const header = { alg: 'HS256' };" },
          { kind: "-", oldNo: 9, text: "  return sign(header, claims);" },
          { kind: "+", newNo: 9, text: "  const v2 = { ...header, typ: 'JWT2', kid };" },
          { kind: "+", newNo: 10, text: "  return sign(v2, encodeClaims(claims));" },
        ],
      },
      {
        header: "@@ -21,7 +25,11 @@ function encodeClaims(c)",
        status: "pending",
        lines: [
          { kind: " ", oldNo: 21, newNo: 25, text: "function encodeClaims(c) {" },
          { kind: "-", oldNo: 22, text: "  return btoa(JSON.stringify(c));" },
          { kind: "+", newNo: 26, text: "  const norm = normalizeClaims(c);" },
          { kind: "+", newNo: 27, text: "  return base64url(cbor(norm));" },
          { kind: " ", oldNo: 23, newNo: 28, text: "}" },
        ],
      },
    ],
  },
  { path: "auth/middleware.js", status: "M", added: 9, removed: 3, meta: "+9 −3 · pending", applied: "none", hunks: [] },
  { path: "auth/token-v2.test.js", status: "A", added: 64, removed: 0, meta: "+64 · pending", applied: "none", hunks: [] },
  { path: "package.json", status: "M", added: 1, removed: 1, meta: "+1 −1 · applied ✓", applied: "full", hunks: [] },
  { path: "auth/legacy.js", status: "D", added: 0, removed: 40, meta: "−40 · pending", applied: "none", hunks: [] },
  { path: "auth/keyring.js", status: "A", added: 47, removed: 0, meta: "+47 · pending", applied: "none", hunks: [] },
];

export const bundles: Bundle[] = [
  {
    id: "kubernetes-prod",
    name: "kubernetes-prod",
    glyph: "☸",
    publisher: "bough-verified",
    version: "v3.2",
    verified: true,
    desc: "Gate kubectl & API server calls. Reads open, mutations held.",
    installs: "12.4k",
    state: "configuring",
    params: [
      { kind: "text", label: "Cluster context", value: "prod-us-east-1" },
      { kind: "select", label: "Default namespace", value: "payments" },
      {
        kind: "multiselect",
        label: "Allowed verbs",
        selected: ["get", "list", "watch"],
        available: ["apply", "delete"],
      },
      {
        kind: "toggle",
        label: "Hold destructive actions",
        hint: "delete, drain, scale-to-0 → hold & ask",
        on: true,
      },
      { kind: "number", label: "Max requests / min", value: "120" },
    ],
  },
  {
    id: "github",
    name: "github",
    glyph: "⑂",
    publisher: "bough-verified",
    version: "v5.0",
    verified: true,
    desc: "Repo read/write, PRs, actions. Deletes hold for approval.",
    installs: "89k",
    state: "installed",
  },
  {
    id: "aws-readonly",
    name: "aws-readonly",
    glyph: "◈",
    publisher: "bough-verified",
    version: "v2.1",
    verified: true,
    desc: "Describe/List/Get across services. All writes denied.",
    installs: "41k",
    state: "install",
  },
  {
    id: "npm-registry",
    name: "npm-registry",
    glyph: "▲",
    publisher: "community",
    version: "v1.4",
    verified: false,
    desc: "Fetch from registries; publish held. Scoped allowlist.",
    installs: "58k",
    state: "install",
  },
];

// The heads-map graph (screen 2). Coordinates follow the design's spine layout.
export const mapNodes: MapNode[] = [
  { id: "task", label: "Task", x: 70, y: 300, kind: "turn", tone: "green" },
  { id: "plan", label: "Plan", x: 200, y: 300, kind: "turn", tone: "green" },
  { id: "encoder", label: "Encoder", x: 340, y: 300, kind: "turn", tone: "green" },
  { id: "mware", label: "M'ware", x: 480, y: 300, kind: "turn", tone: "green" },
  { id: "tests", label: "Tests · HEAD", x: 620, y: 300, kind: "head", tone: "green", head: true },

  { id: "edit-a", label: "↩ edit · v3 format", x: 340, y: 170, kind: "turn", tone: "muted" },
  { id: "edit-b", label: "", x: 480, y: 170, kind: "turn", tone: "muted" },
  { id: "edit-head", label: "", x: 620, y: 170, kind: "head", tone: "muted", tip: { text: "head", tone: "muted" } },

  { id: "wa-1", label: "◇ worker · m'ware A", x: 480, y: 410, kind: "turn", tone: "muted" },
  { id: "wa-head", label: "", x: 620, y: 410, kind: "head", tone: "green", tip: { text: "✓", tone: "green" } },

  { id: "wb-1", label: "◇ worker · m'ware B", x: 480, y: 490, kind: "turn", tone: "dead" },
  { id: "wb-head", label: "", x: 620, y: 490, kind: "head", tone: "dead", tip: { text: "✗ failed", tone: "red" } },

  { id: "compact", label: "compacted · 18 → 1", x: 200, y: 140, kind: "turn", tone: "compacted" },
];

export const mapEdges: MapEdge[] = [
  { from: "task", to: "plan", tone: "green" },
  { from: "plan", to: "encoder", tone: "green" },
  { from: "encoder", to: "mware", tone: "green" },
  { from: "mware", to: "tests", tone: "green" },
  { from: "plan", to: "edit-a", tone: "muted" },
  { from: "edit-a", to: "edit-b", tone: "muted" },
  { from: "edit-b", to: "edit-head", tone: "muted" },
  { from: "encoder", to: "wa-1", tone: "muted" },
  { from: "wa-1", to: "wa-head", tone: "muted" },
  { from: "encoder", to: "wb-1", tone: "dead" },
  { from: "wb-1", to: "wb-head", tone: "dead" },
  { from: "task", to: "compact", tone: "compacted" },
];
