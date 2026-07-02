// Adapters from live backend shapes to the UI view models the components render.
// The mock layer (mock.ts) hand-authors these; live mode derives them from real
// sessions/threads/bundle summaries so the same components serve both paths.
import type { Bundle, BundleParam, DiffFile, DiffLine, Head, Hunk, OutlineNode } from "./mock";
import type { BundleSummary, Message, Session, WireDiff, WireFileDiff } from "./types";

const kindGlyph: Record<Session["kind"], string> = {
  root: "⎇",
  fork: "↩",
  worker: "◇",
  compaction: "⊟",
};

function relTime(ms: number): string {
  const s = Math.round((Date.now() - ms) / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m`;
  return `${Math.round(m / 60)}h`;
}

/** Sessions → the switchable heads list. The open session is the active head. */
export function headsFromSessions(sessions: Session[], currentId: string | null): Head[] {
  return sessions.map((s) => ({
    id: s.id,
    glyph: kindGlyph[s.kind],
    label: s.title,
    meta: `${s.kind} · ${relTime(s.createdAt)}`,
    active: s.id === currentId,
    status: s.id === currentId ? "running" : "idle",
    busy: s.busy,
    unseen: s.unseen,
  }));
}

// A sidebar section: one workspace directory (or the chat-only bucket) and its
// sessions, newest first. The section's "+" creates a session in that workspace.
export interface HeadGroup {
  key: string; // workspace path, "" for chat-only sessions
  label: string; // directory basename, or "chat"
  workspace: string | null;
  heads: Head[];
}

/** Sessions → sidebar groups by workspace dir; groups and heads sort newest-first. */
export function headGroupsFromSessions(sessions: Session[], currentId: string | null): HeadGroup[] {
  const byWorkspace = new Map<string, Session[]>();
  for (const s of sessions) {
    const key = s.workspace ?? "";
    byWorkspace.set(key, [...(byWorkspace.get(key) ?? []), s]);
  }
  return [...byWorkspace.entries()]
    .map(([key, group]) => {
      const sorted = [...group].sort((a, b) => b.createdAt - a.createdAt);
      return {
        key,
        label: key ? key.replace(/\/+$/, "").split("/").pop() || key : "chat",
        workspace: key || null,
        heads: headsFromSessions(sorted, currentId),
        latest: sorted[0]?.createdAt ?? 0,
      };
    })
    .sort((a, b) => b.latest - a.latest)
    .map(({ latest: _latest, ...g }) => g);
}

/** The open thread → the current-head outline. One node per non-empty turn. */
export function outlineFromThread(thread: Message[]): OutlineNode[] {
  return thread
    .map((m): OutlineNode | null => {
      const text = m.parts.find((p) => p.type === "text");
      const label = text && "text" in text ? text.text.split("\n")[0].slice(0, 42) : m.role;
      if (!label) return null;
      return { label, state: m.pending ? "running" : "done" };
    })
    .filter((n): n is OutlineNode => n !== null);
}

// A backend param type maps to exactly one UI control.
function paramToControl(p: BundleSummary["params"][number]): BundleParam {
  switch (p.type) {
    case "bool":
      return { kind: "toggle", label: p.name, hint: p.description, on: p.default === true };
    case "hostList":
      return {
        kind: "multiselect",
        label: p.name,
        hint: p.description,
        selected: Array.isArray(p.default) ? p.default : [],
        available: [],
      };
    case "host":
    case "string":
    default:
      return { kind: "text", label: p.name, hint: p.description, value: String(p.default ?? "") };
  }
}

// ---- changes review -------------------------------------------------------

const STATUS: Record<WireFileDiff["status"], DiffFile["status"]> = {
  added: "A",
  modified: "M",
  deleted: "D",
};

// "@@ -12,7 +12,9 @@ ctx" → starting old/new line numbers (default 1 if absent).
function hunkStart(header: string): { oldNo: number; newNo: number } {
  const m = /@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(header);
  return { oldNo: m ? Number(m[1]) : 1, newNo: m ? Number(m[2]) : 1 };
}

// Turn the raw unified-diff lines (markers kept) into numbered display lines.
function parseHunk(header: string, raw: string[]): { hunk: Hunk; added: number; removed: number } {
  let { oldNo, newNo } = hunkStart(header);
  let added = 0;
  let removed = 0;
  const lines: DiffLine[] = [];
  for (const l of raw) {
    if (l === "\\ No newline at end of file") continue;
    const kind = (l[0] ?? " ") as DiffLine["kind"];
    const text = l.slice(1);
    if (kind === "+") {
      lines.push({ kind, newNo, text });
      newNo++;
      added++;
    } else if (kind === "-") {
      lines.push({ kind, oldNo, text });
      oldNo++;
      removed++;
    } else {
      lines.push({ kind: " ", oldNo, newNo, text });
      oldNo++;
      newNo++;
    }
  }
  // Live apply is per-file; the backend has no per-hunk applied state, so every hunk
  // reads as pending. (Mock keeps applied/partial hunks as design intent.)
  return { hunk: { header, status: "pending", lines }, added, removed };
}

// Flatten the 0..2 backend diffs into the UI's per-file model, tagged by source.
export function diffsToFiles(diffs: WireDiff[]): DiffFile[] {
  const out: DiffFile[] = [];
  for (const d of diffs) {
    for (const f of d.files) {
      let added = 0;
      let removed = 0;
      const hunks = f.hunks.map((h) => {
        const p = parseHunk(h.header, h.lines);
        added += p.added;
        removed += p.removed;
        return p.hunk;
      });
      const meta = f.hunks.length
        ? `+${added} −${removed} · ${f.hunks.length} hunk${f.hunks.length === 1 ? "" : "s"}`
        : f.status === "deleted"
          ? "deleted"
          : "no textual diff";
      out.push({
        path: f.path,
        status: STATUS[f.status],
        added,
        removed,
        meta,
        applied: "none",
        hunks,
        source: d.source,
      });
    }
  }
  return out;
}

// Fields the registry doesn't carry yet (publisher/signature/install count/icon) get
// neutral defaults; what the backend does provide (name/version/description/params/
// installed) is shown faithfully.
export function bundleFromSummary(b: BundleSummary): Bundle {
  return {
    id: b.name,
    name: b.name,
    glyph: "◆",
    publisher: "community",
    version: `v${b.version}`,
    verified: false,
    desc: b.description,
    installs: "—",
    state: b.installed ? "installed" : "install",
    params: b.params.map(paramToControl),
  };
}
