// Left rail: the current head as a readable top-to-bottom outline, plus the switchable
// list of all heads grouped by workspace directory (newest group first). The live
// outline node pulses green. "⤢ Map" opens the heads map.
// Each group header has a "+" that instantly creates a session in that repo; the
// top-level "+" reveals a one-field form (workspace path only — titles are generated
// by the title worker from the first message).
import { useEffect, useState } from "react";
import { c, mono } from "../theme";
import type { Head, OutlineNode } from "../mock";
import { cacheRemainingMs, fmtWarmth, type HeadGroup } from "../live";
import { useNow } from "../useNow";

// The current-thread outline is multicolor by speaker: your turns are blue,
// the AI's turns (supervisor / worker) are green, system notes stay neutral.
const roleColor: Record<NonNullable<OutlineNode["role"]>, string> = {
  user: c.blue,
  supervisor: c.green,
  worker: c.green,
  system: c.muted2,
};

function OutlineRow({ node, last }: { node: OutlineNode; last: boolean }) {
  const running = node.state === "running";
  const dot = node.role ? roleColor[node.role] : node.state === "done" ? c.green : c.hairline;
  return (
    <div style={{ position: "relative", marginBottom: last ? 0 : 13 }}>
      {running ? (
        <span
          className="pulse-green"
          style={{
            position: "absolute",
            left: -16,
            top: 3,
            width: 9,
            height: 9,
            borderRadius: "50%",
            background: c.panel,
            border: `2px solid ${dot}`,
          }}
        />
      ) : (
        <span
          style={{
            position: "absolute",
            left: -15,
            top: 4,
            width: 7,
            height: 7,
            borderRadius: "50%",
            background: dot,
          }}
        />
      )}
      <div style={{ fontSize: 12.5, color: running ? c.text : c.muted, fontWeight: running ? 500 : 400 }}>
        {node.label} {node.note && <span style={{ color: c.green }}>{node.note}</span>}
      </div>
    </div>
  );
}

// No hover on touch screens — affordances that reveal on hover must just be there.
const noHover = window.matchMedia("(hover: none)").matches;

function HeadCard(
  { head, onClick, onArchive }: { head: Head; onClick: () => void; onArchive?: () => void },
) {
  const [hover, setHover] = useState(false);
  const now = useNow();
  // Prompt-cache warmth: the provider keeps this thread's prefix hot for ~5 min
  // after its last request (refreshed on every hit). Warm = the next message is
  // cheap/fast. Deliberately calm: just a ⚡ glyph that fades away when cold —
  // the remaining time lives in the hover tooltip, never as a visible countdown.
  const cacheMs = cacheRemainingMs({ lastLlmAt: head.cacheAt ?? null, busy: head.busy }, now);
  const cachePct = head.cacheShare != null ? `${Math.round(head.cacheShare * 100)}%` : null;
  const dashed = head.status === "compacted";
  const glyphColor =
    head.glyph === "⎇" ? c.green : head.glyph === "⊟" ? c.amber : c.muted2;
  return (
    <div
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      style={{ position: "relative" }}
    >
      <button
        onClick={onClick}
        style={{
          textAlign: "left",
          width: "100%",
          background: head.active ? c.panelInset : c.panel3,
          border: head.active
            ? `1px solid ${c.border}`
            : dashed
              ? `1px dashed ${c.hairline}`
              : `1px solid ${c.border2}`,
          borderLeft: head.active ? `2px solid ${c.green}` : undefined,
          borderRadius: 7,
          padding: "8px 10px",
        }}
      >
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: 6,
            fontSize: 12,
            color: head.active ? c.text : c.muted,
            fontWeight: head.active ? 500 : 400,
            paddingRight: 16,
          }}
        >
          <span style={{ color: glyphColor }}>{head.glyph}</span> {head.label}
          {head.busy
            ? (
              <span
                className="pulse-green"
                title="a turn is running"
                style={{
                  flex: "none",
                  width: 8,
                  height: 8,
                  borderRadius: "50%",
                  background: c.panel,
                  border: `2px solid ${c.green}`,
                  marginLeft: "auto",
                }}
              />
            )
            : head.status === "failed"
            ? (
              <span
                title="last turn failed or was stopped"
                style={{ flex: "none", color: c.red, fontSize: 10, marginLeft: "auto" }}
              >
                ✗
              </span>
            )
            : head.unseen
            ? (
              <span
                title="finished — take a look"
                style={{
                  flex: "none",
                  width: 8,
                  height: 8,
                  borderRadius: "50%",
                  background: c.green,
                  marginLeft: "auto",
                }}
              />
            )
            : null}
        </div>
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: 6,
            fontFamily: mono,
            fontSize: 10.5,
            color: c.muted2,
            marginTop: 3,
          }}
        >
          <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
            {head.meta}
          </span>
          {cacheMs > 0 && (
            <span
              title={head.busy
                ? "prompt cache warm — being refreshed by the running turn"
                : `prompt cache warm${
                  cachePct ? ` — ${cachePct} of the context is cached` : ""
                } · ${fmtWarmth(cacheMs)} left; sending a message refreshes it`}
              style={{ marginLeft: "auto", flex: "none", color: c.green, opacity: 0.75 }}
            >
              ⚡
            </span>
          )}
        </div>
      </button>
      {onArchive && (hover || noHover) && (
        <button
          onClick={(e) => {
            e.stopPropagation();
            onArchive();
          }}
          title="Archive this session (it leaves the list; the thread is kept)"
          style={{
            position: "absolute",
            top: 6,
            right: 6,
            width: noHover ? 24 : 18,
            height: noHover ? 24 : 18,
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            borderRadius: 5,
            border: `1px solid ${c.border2}`,
            background: c.panel2,
            color: c.muted,
            fontSize: 10,
            lineHeight: 1,
          }}
        >
          ✕
        </button>
      )}
    </div>
  );
}

// Recursive checks over a head's nested branches (children of children included).
function anyHead(heads: Head[] | undefined, pred: (h: Head) => boolean): boolean {
  return (heads ?? []).some((h) => pred(h) || anyHead(h.children, pred));
}

// One head plus its branched-off sessions (forks/compactions/subagents), folded
// behind a toggle so a burst of subagents doesn't swamp the rail. Collapsed by
// default; auto-expands while the open session is inside, and the toggle carries
// the busy pulse / unseen dot so hidden activity stays visible.
function HeadNode(
  { head, onSelectHead, onArchiveHead }: {
    head: Head;
    onSelectHead: (id: string) => void;
    onArchiveHead?: (id: string) => void;
  },
) {
  const kids = head.children ?? [];
  const [open, setOpen] = useState(false);
  const activeInside = anyHead(kids, (h) => !!h.active);
  useEffect(() => {
    if (activeInside) setOpen(true);
  }, [activeInside]);
  const busyInside = anyHead(kids, (h) => !!h.busy);
  const unseenInside = anyHead(kids, (h) => !!h.unseen);
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 5 }}>
      <HeadCard
        head={head}
        onClick={() => onSelectHead(head.id)}
        onArchive={onArchiveHead && (() => onArchiveHead(head.id))}
      />
      {kids.length > 0 && (
        <button
          onClick={() => setOpen((o) => !o)}
          style={{
            display: "flex",
            alignItems: "center",
            gap: 6,
            marginLeft: 10,
            padding: "2px 4px",
            fontSize: 10.5,
            fontFamily: mono,
            color: c.muted2,
            textAlign: "left",
          }}
        >
          <span>{open ? "▾" : "▸"}</span>
          {kids.length} {kids.length === 1 ? "branch" : "branches"}
          {busyInside && (
            <span
              className="pulse-green"
              title="a turn is running in a branch"
              style={{
                width: 7,
                height: 7,
                borderRadius: "50%",
                background: c.panel,
                border: `2px solid ${c.green}`,
              }}
            />
          )}
          {!open && !busyInside && unseenInside && (
            <span
              title="a branch finished — take a look"
              style={{ width: 7, height: 7, borderRadius: "50%", background: c.green }}
            />
          )}
        </button>
      )}
      {open && kids.length > 0 && (
        <div
          style={{
            marginLeft: 10,
            paddingLeft: 8,
            borderLeft: `1px solid ${c.border2}`,
            display: "flex",
            flexDirection: "column",
            gap: 5,
          }}
        >
          {kids.map((k) => (
            <HeadNode key={k.id} head={k} onSelectHead={onSelectHead} onArchiveHead={onArchiveHead} />
          ))}
        </div>
      )}
    </div>
  );
}

function NewSessionForm({ onCreate }: { onCreate: (workspace: string) => void }) {
  const [workspace, setWorkspace] = useState("");
  const submit = () => onCreate(workspace.trim());
  return (
    <div style={{ padding: "10px 12px", borderBottom: `1px solid ${c.border}`, display: "flex", flexDirection: "column", gap: 7 }}>
      <input
        autoFocus
        style={{
          width: "100%",
          background: c.panel,
          border: `1px solid ${c.border}`,
          borderRadius: 6,
          padding: "6px 8px",
          color: c.text,
          fontFamily: mono,
          fontSize: 11.5,
          outline: "none",
        }}
        placeholder="workspace — a repo path, e.g. ~/repos/app"
        value={workspace}
        onChange={(e) => setWorkspace(e.target.value)}
        onKeyDown={(e) => e.key === "Enter" && submit()}
      />
      <span style={{ fontSize: 10.5, color: c.muted2, lineHeight: 1.4 }}>
        A git repo the agent may edit (sandboxed). Leave empty for a chat-only session.
        The title writes itself from your first message.
      </span>
      <button
        onClick={submit}
        style={{
          background: c.green,
          color: c.bg,
          borderRadius: 6,
          padding: "6px 0",
          fontSize: 11.5,
          fontWeight: 600,
        }}
      >
        Create session
      </button>
    </div>
  );
}

function GroupHeader(
  { group, expanded, onToggle, onNewInGroup }: {
    group: HeadGroup;
    expanded: boolean;
    onToggle: () => void;
    onNewInGroup?: () => void;
  },
) {
  return (
    <div
      onClick={onToggle}
      title={expanded ? "Collapse this directory" : "Show this directory's sessions"}
      style={{
        display: "flex",
        alignItems: "center",
        justifyContent: "space-between",
        marginBottom: 8,
        cursor: "pointer",
      }}
    >
      <span
        title={group.workspace ?? "chat-only sessions"}
        style={{
          fontSize: 10.5,
          letterSpacing: ".12em",
          color: c.muted2,
          fontWeight: 600,
          textTransform: "uppercase",
          overflow: "hidden",
          textOverflow: "ellipsis",
          whiteSpace: "nowrap",
        }}
      >
        <span style={{ display: "inline-block", width: 12, fontSize: 9 }}>
          {expanded ? "▾" : "▸"}
        </span>
        {group.label} · {group.heads.length}
      </span>
      {onNewInGroup && (
        <button
          onClick={(e) => {
            e.stopPropagation();
            onNewInGroup();
          }}
          title={group.workspace ? `New session in ${group.workspace}` : "New chat session"}
          style={{
            flex: "none",
            fontSize: 12,
            lineHeight: 1,
            color: c.muted,
            padding: "2px 7px",
            border: `1px solid ${c.border2}`,
            borderRadius: 5,
          }}
        >
          +
        </button>
      )}
    </div>
  );
}

export function LeftRail({
  groups,
  outline,
  openFormSignal,
  onOpenMap,
  onSelectHead,
  onCreateSession,
  onArchiveHead,
}: {
  groups: HeadGroup[];
  outline: OutlineNode[];
  // Bumped by the command palette's "New session" to open the create form.
  openFormSignal?: number;
  onOpenMap: () => void;
  onSelectHead: (id: string) => void;
  onCreateSession?: (workspace: string) => void;
  onArchiveHead?: (id: string) => void;
}) {
  const [creating, setCreating] = useState(false);
  // Directory groups are collapsed by default (there can be dozens of heads). A
  // collapsed group still surfaces heads that are busy, active, or have live work
  // in their nested branches — the click target for reopening is the header itself.
  const [expandedGroups, setExpandedGroups] = useState<Set<string>>(new Set());
  const toggleGroup = (key: string) =>
    setExpandedGroups((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  const alive = (h: Head): boolean =>
    !!h.busy || !!h.active || anyHead(h.children, (k) => !!k.busy || !!k.active);
  useEffect(() => {
    if (openFormSignal) setCreating(true);
  }, [openFormSignal]);
  const headCount = groups.reduce((n, g) => n + g.heads.length, 0);
  return (
    <div
      style={{
        width: 266,
        flex: "none",
        background: c.panel2,
        borderRight: `1px solid ${c.border}`,
        display: "flex",
        flexDirection: "column",
        minHeight: 0,
      }}
    >
      <div
        style={{
          height: 38,
          flex: "none",
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          padding: "0 14px",
          borderBottom: `1px solid ${c.border}`,
        }}
      >
        <span style={{ fontSize: 11, letterSpacing: ".14em", color: c.muted2, fontWeight: 600 }}>
          HEADS · {headCount}
        </span>
        <div style={{ display: "flex", gap: 6 }}>
          {onCreateSession && (
            <button
              onClick={() => setCreating((v) => !v)}
              title="New session"
              style={{ fontSize: 13, color: creating ? c.green : c.muted, padding: "3px 9px", border: `1px solid ${c.border}`, borderRadius: 6, lineHeight: 1 }}
            >
              +
            </button>
          )}
          <button
            onClick={onOpenMap}
            style={{
              display: "inline-flex",
              alignItems: "center",
              gap: 6,
              fontSize: 11.5,
              color: c.muted,
              padding: "3px 9px",
              border: `1px solid ${c.border}`,
              borderRadius: 6,
            }}
          >
            ⤢ Map
          </button>
        </div>
      </div>

      {creating && onCreateSession && (
        <NewSessionForm
          onCreate={(w) => {
            onCreateSession(w);
            setCreating(false);
          }}
        />
      )}

      {/* CURRENT THREAD — capped with its own scroll so a long thread never buries
          the heads list below it. Only shown when there's an outline to draw. */}
      {outline.length > 0 && (
        <div
          style={{
            flex: "none",
            maxHeight: "38%",
            overflowY: "auto",
            padding: "14px 12px 10px",
            borderBottom: `1px solid ${c.border}`,
          }}
        >
          <div
            style={{
              fontSize: 10.5,
              letterSpacing: ".12em",
              color: c.green,
              fontWeight: 600,
              marginBottom: 11,
            }}
          >
            CURRENT THREAD
          </div>
          <div style={{ position: "relative", paddingLeft: 16 }}>
            <div style={{ position: "absolute", left: 4, top: 6, bottom: 6, width: 1.5, background: c.border }} />
            {outline.map((n, i) => (
              <OutlineRow key={i} node={n} last={i === outline.length - 1} />
            ))}
          </div>
        </div>
      )}

      {/* Heads list — its own scroll region. */}
      <div style={{ flex: 1, overflowY: "auto", padding: "14px 12px", minHeight: 0 }}>
        {groups.map((g, i) => (
          <div key={g.key} style={{ marginBottom: i === groups.length - 1 ? 0 : 18 }}>
            <GroupHeader
              group={g}
              expanded={expandedGroups.has(g.key)}
              onToggle={() => toggleGroup(g.key)}
              onNewInGroup={onCreateSession && (() => onCreateSession(g.workspace ?? ""))}
            />
            <div style={{ display: "flex", flexDirection: "column", gap: 7 }}>
              {g.heads
                .filter((h) => expandedGroups.has(g.key) || alive(h))
                .map((h) => (
                  <HeadNode
                    key={h.id}
                    head={h}
                    onSelectHead={onSelectHead}
                    onArchiveHead={onArchiveHead}
                  />
                ))}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
