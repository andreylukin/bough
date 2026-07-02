// macOS-style titlebar: traffic lights, the bough mark, the active branch chip, and a
// right-aligned run status. In live mode the status derives from real state (SSE
// connection); the sandbox-snapshot / agent chips are hidden until their backends exist,
// rather than showing invented values. Mock mode keeps the fuller design-review chrome.
import { c, mono } from "../theme";
import { CopyId, Dot, Logo } from "./ui";

function Light() {
  return <span style={{ width: 12, height: 12, borderRadius: "50%", background: "#3a414c" }} />;
}

export function TitleBar({
  branch = "main · migrate-auth",
  right,
  live = false,
  connected = false,
  model,
  workspace,
  sessionId,
}: {
  branch?: string;
  right?: React.ReactNode;
  live?: boolean;
  connected?: boolean;
  // Live-mode glanceables: the model turns run on, and the repo this session edits.
  model?: string;
  workspace?: string | null;
  // When set, a copy chip next to the branch chip copies this head's session id.
  sessionId?: string | null;
}) {
  // "claude-opus-4-8" → "opus-4-8": the family+version is what you glance for.
  const shortModel = model ? model.replace(/^claude-/, "") : "";
  const repo = workspace ? workspace.replace(/\/+$/, "").split("/").pop() : null;
  // In live mode, surface the model, the workspace, and the event-stream link.
  const liveRight = (
    <div style={{ display: "flex", alignItems: "center", gap: 16, fontFamily: mono, fontSize: 11.5, color: c.muted2 }}>
      {shortModel && (
        <span style={{ display: "inline-flex", alignItems: "center", gap: 6, color: c.muted }}>
          <span style={{ color: c.green }}>◇</span> {shortModel}
        </span>
      )}
      <span style={{ display: "flex", alignItems: "center", gap: 6, color: c.muted }}>
        <Dot color={connected ? c.green : c.muted2} pulse={connected} />
        {connected ? "connected" : "reconnecting…"}
      </span>
    </div>
  );
  return (
    <div
      style={{
        height: 46,
        flex: "none",
        background: c.panel3,
        borderBottom: `1px solid ${c.border}`,
        display: "flex",
        alignItems: "center",
        padding: "0 16px",
        gap: 16,
      }}
    >
      <div style={{ display: "flex", gap: 8 }}>
        <Light />
        <Light />
        <Light />
      </div>
      <div style={{ marginLeft: 8, display: "flex", alignItems: "center" }}>
        <Logo size={16} />
      </div>
      <span style={{ fontWeight: 600, fontSize: 14 }}>bough</span>
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 7,
          padding: "3px 10px",
          background: c.panelInset,
          border: `1px solid ${c.border}`,
          borderRadius: 7,
          fontFamily: mono,
          fontSize: 12,
          color: c.muted,
        }}
      >
        <span style={{ color: c.green }}>⎇</span> {branch}
      </div>
      {sessionId && <CopyId value={sessionId} title="Copy session id" />}
      {live && repo && (
        <div
          style={{
            display: "inline-flex",
            alignItems: "center",
            gap: 6,
            fontFamily: mono,
            fontSize: 11.5,
            color: c.muted2,
          }}
          title={workspace ?? undefined}
        >
          <span>▸</span> {repo}
        </div>
      )}
      <div style={{ flex: 1 }} />
      {right ?? (live ? liveRight : (
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: 18,
            fontFamily: mono,
            fontSize: 11.5,
            color: c.muted2,
          }}
        >
          <span>sandbox · snapshot 3f2a1c</span>
          <span style={{ display: "flex", alignItems: "center", gap: 6, color: c.muted }}>
            <Dot /> gate: github · live
          </span>
          <span>agent-lg</span>
        </div>
      ))}
    </div>
  );
}

export function StatusStrip({ heads, live = false, connected = false }: { heads: number; live?: boolean; connected?: boolean }) {
  return (
    <div
      style={{
        height: 28,
        flex: "none",
        background: c.panel3,
        borderTop: `1px solid ${c.border}`,
        display: "flex",
        alignItems: "center",
        padding: "0 16px",
        gap: 20,
        fontFamily: mono,
        fontSize: 11,
        color: c.muted2,
      }}
    >
      {live ? (
        // Only what's real: the event-stream link and the live head count. Snapshot,
        // agent name, and pending-review count wait for their backends.
        <span style={{ color: connected ? c.green : c.muted2 }}>
          {connected ? "● connected" : "○ reconnecting"}
        </span>
      ) : (
        <>
          <span>agent-lg</span>
          <span>sandbox · 3f2a1c</span>
          <span style={{ color: c.green }}>● gate active</span>
          <span>12 files pending review</span>
        </>
      )}
      <span style={{ marginLeft: "auto" }}>⎇ {heads} heads</span>
    </div>
  );
}
