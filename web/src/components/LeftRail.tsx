// Left rail: the current head as a readable top-to-bottom outline, plus the switchable
// list of all heads grouped by workspace directory (newest group first). The live
// outline node pulses green. "⤢ Map" opens the heads map.
// Each group header has a "+" that instantly creates a session in that repo; the
// top-level "+" reveals a one-field form (workspace path only — titles are generated
// by the title worker from the first message).
import { useEffect, useState } from "react";
import { c, mono } from "../theme";
import type { Head, OutlineNode } from "../mock";
import type { HeadGroup } from "../live";

function OutlineRow({ node, last }: { node: OutlineNode; last: boolean }) {
  const running = node.state === "running";
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
            border: `2px solid ${c.green}`,
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
            background: node.state === "done" ? c.green : c.hairline,
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
        <div style={{ fontFamily: mono, fontSize: 10.5, color: c.muted2, marginTop: 3 }}>{head.meta}</div>
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

function GroupHeader({ group, onNewInGroup }: { group: HeadGroup; onNewInGroup?: () => void }) {
  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        justifyContent: "space-between",
        marginBottom: 8,
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
        {group.label} · {group.heads.length}
      </span>
      {onNewInGroup && (
        <button
          onClick={onNewInGroup}
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

      <div style={{ flex: 1, overflowY: "auto", padding: "14px 12px", minHeight: 0 }}>
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
        <div style={{ position: "relative", paddingLeft: 16, marginBottom: 20 }}>
          <div style={{ position: "absolute", left: 4, top: 6, bottom: 6, width: 1.5, background: c.border }} />
          {outline.map((n, i) => (
            <OutlineRow key={i} node={n} last={i === outline.length - 1} />
          ))}
        </div>

        {groups.map((g, i) => (
          <div key={g.key} style={{ marginBottom: i === groups.length - 1 ? 0 : 18 }}>
            <GroupHeader
              group={g}
              onNewInGroup={onCreateSession && (() => onCreateSession(g.workspace ?? ""))}
            />
            <div style={{ display: "flex", flexDirection: "column", gap: 7 }}>
              {g.heads.map((h) => (
                <HeadCard
                  key={h.id}
                  head={h}
                  onClick={() => onSelectHead(h.id)}
                  onArchive={onArchiveHead && (() => onArchiveHead(h.id))}
                />
              ))}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
