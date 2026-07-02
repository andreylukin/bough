// Right context rail. Two tabs share it — Network (live Claw Patrol feed + pending
// approvals) and Changes (the run's file manifest). Pending pulses the green/amber
// accent; nothing else competes. The Network panel has a compact form (main window)
// and a wide, focused form (screen 3) with filters and a detailed hold card.
import { c, mono } from "../theme";
import type { DiffFile } from "../mock";
import type { NetStatus } from "../api";
import { Chip, Dot } from "./ui";

export type RailTab = "network" | "changes";

function TabHeader({
  tab,
  onTab,
  changesCount,
  pendingCount,
  padY = 0,
  wide,
  onToggleWide,
}: {
  tab: RailTab;
  onTab: (t: RailTab) => void;
  changesCount: number;
  pendingCount: number;
  padY?: number;
  wide?: boolean;
  onToggleWide?: () => void;
}) {
  const item = (t: RailTab, label: React.ReactNode) => {
    const active = t === tab;
    return (
      <button
        onClick={() => onTab(t)}
        style={{
          display: "flex",
          alignItems: "center",
          gap: 8,
          padding: "0 12px",
          fontSize: 12.5,
          fontWeight: active ? 500 : 400,
          color: active ? c.text : c.muted2,
          borderBottom: active ? `2px solid ${c.green}` : "2px solid transparent",
        }}
      >
        {label}
      </button>
    );
  };
  return (
    <div
      style={{
        height: 38 + padY,
        flex: "none",
        display: "flex",
        alignItems: "stretch",
        borderBottom: `1px solid ${c.border}`,
        padding: "0 6px",
      }}
    >
      {item(
        "network",
        <>
          Network{" "}
          {pendingCount > 0 ? (
            <Chip style={{ background: "rgba(217,180,95,.18)", color: c.amber }}>{pendingCount}</Chip>
          ) : (
            <Dot />
          )}
        </>
      )}
      {item("changes", <>Changes {changesCount > 0 && <Chip>{changesCount}</Chip>}</>)}
      {onToggleWide && (
        <button
          onClick={onToggleWide}
          title={wide ? "Collapse rail" : "Focus rail"}
          style={{ marginLeft: "auto", marginRight: 6, color: c.muted2, fontSize: 13, padding: "0 8px" }}
        >
          {wide ? "⤡" : "⤢"}
        </button>
      )}
    </div>
  );
}

// The Network rail is now a thin status surface for Claw Patrol — bough runs the
// firewall but Claw Patrol owns the live feed and human approvals on its dashboard.
function NetworkPanel({ status, wide }: { status: NetStatus; wide: boolean }) {
  const dotColor = status.running ? c.green : status.enabled ? c.amber : c.muted2;
  const line = !status.enabled
    ? "Egress gating is off. Start bough with BOUGH_CLAWPATROL=1 to route sandbox traffic through Claw Patrol."
    : !status.available
    ? "clawpatrol binary not found on PATH — egress is NOT gated."
    : status.running && status.external
    ? "Routing sandbox egress through the existing Claw Patrol client (Clawpatrol.app). Audit + approvals live in the Claw Patrol dashboard."
    : status.running
    ? "Claw Patrol gateway is up. Sandbox egress routes through it; audit + approvals are on its dashboard."
    : "Claw Patrol gateway not reachable — routing is off (fail-open). Run `clawpatrol join` on this machine to onboard it.";
  return (
    <div style={{ flex: 1, overflowY: "auto", padding: wide ? "15px" : "14px 13px", minHeight: 0, display: "flex", flexDirection: "column", gap: 14 }}>
      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
        <Dot color={dotColor} pulse={status.running} />
        <span style={{ fontFamily: mono, fontSize: 12, color: c.text }}>Claw Patrol</span>
        <span style={{ fontFamily: mono, fontSize: 10.5, color: c.muted2 }}>
          {status.running ? (status.external ? "clawpatrol.app" : "gateway up") : status.enabled ? "off" : "disabled"}
        </span>
      </div>
      <p style={{ fontSize: 12.5, color: c.muted, lineHeight: 1.55, margin: 0 }}>{line}</p>
      {status.running && !status.external && status.dashboardUrl && (
        <a
          href={status.dashboardUrl}
          target="_blank"
          rel="noreferrer"
          style={{ fontSize: 12, color: c.green, textDecoration: "none", border: `1px solid ${c.border2}`, borderRadius: 6, padding: "6px 11px", alignSelf: "flex-start" }}
        >
          Open Claw Patrol dashboard ↗
        </a>
      )}
      <button
        onClick={() => (location.hash = "bundles")}
        title="Configure the policy bundles that shape the gateway"
        style={{ display: "inline-flex", alignItems: "center", gap: 7, fontFamily: mono, fontSize: 11.5, color: c.muted, background: "none", border: `1px solid ${c.border2}`, borderRadius: 6, padding: "5px 10px", cursor: "pointer", alignSelf: "flex-start" }}
      >
        ⚙ Policy bundles
      </button>
    </div>
  );
}

function fileTint(s: DiffFile["status"]) {
  return s === "A" ? c.green : s === "D" ? c.red : c.amber;
}

const SOURCE_LABEL: Record<NonNullable<DiffFile["source"]>, string> = {
  jj: "REPO · jj",
  clonefile: "CONFIG · clonefile",
};

function FileRow({ f, active, onSelect }: { f: DiffFile; active: boolean; onSelect: () => void }) {
  const done = f.applied === "full";
  const partial = f.applied === "partial";
  return (
    <button
      onClick={onSelect}
      style={{
        display: "flex",
        alignItems: "center",
        gap: 9,
        padding: "9px 10px",
        borderRadius: 7,
        textAlign: "left",
        width: "100%",
        background: active ? c.panelInset : "transparent",
        border: active ? `1px solid ${c.border}` : "1px solid transparent",
      }}
    >
      <span style={{ color: fileTint(f.status), fontWeight: 600 }}>{f.status}</span>
      <div style={{ minWidth: 0, flex: 1 }}>
        <div style={{ color: active ? c.text : c.text2, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>
          {f.path}
        </div>
        <div style={{ color: done ? c.green : c.muted2, fontSize: 10 }}>{f.meta}</div>
      </div>
      {done && <span style={{ color: c.green }}>✓</span>}
      {partial && <span style={{ color: c.green, fontSize: 10 }}>◐</span>}
    </button>
  );
}

function ChangesPanel({
  diffs,
  selected,
  onSelect,
  onApplyAll,
  onRevert,
}: {
  diffs: DiffFile[];
  selected: string | null;
  onSelect: (path: string) => void;
  onApplyAll: () => void;
  onRevert: () => void;
}) {
  const totAdd = diffs.reduce((a, f) => a + f.added, 0);
  const totRem = diffs.reduce((a, f) => a + f.removed, 0);

  if (diffs.length === 0) {
    return (
      <div style={{ flex: 1, overflowY: "auto", padding: "20px 14px", minHeight: 0 }}>
        <div style={{ fontSize: 12.5, color: c.muted2, lineHeight: 1.55 }}>
          No changes staged. When a turn edits files in the session workspace, they land here
          for review — nothing is written back until you apply.
        </div>
      </div>
    );
  }

  // Group by source (jj repo / clonefile config) when tagged; mock files are untagged
  // and render as one flat list.
  const grouped = diffs.some((f) => f.source);
  const sources = grouped
    ? ([...new Set(diffs.map((f) => f.source))].filter(Boolean) as NonNullable<DiffFile["source"]>[])
    : [];
  const hasRepo = diffs.some((f) => f.source === "jj");

  return (
    <div style={{ flex: 1, overflowY: "auto", padding: "14px 12px", minHeight: 0 }}>
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: 14 }}>
        <span style={{ fontFamily: mono, fontSize: 11, color: c.muted2 }}>
          {diffs.length} files · <span style={{ color: c.green }}>+{totAdd}</span>{" "}
          <span style={{ color: c.red }}>−{totRem}</span>
        </span>
        <div style={{ display: "flex", gap: 7 }}>
          {(!grouped || hasRepo) && (
            <button onClick={onRevert} style={{ fontSize: 11, color: c.muted, padding: "4px 9px", border: `1px solid ${c.border}`, borderRadius: 6 }}>
              Revert
            </button>
          )}
          <button onClick={onApplyAll} style={{ fontSize: 11, color: c.bg, fontWeight: 600, padding: "4px 10px", borderRadius: 6, background: c.green }}>
            Apply all
          </button>
        </div>
      </div>

      {grouped ? (
        sources.map((src) => (
          <div key={src} style={{ marginBottom: 14 }}>
            <div style={{ fontFamily: mono, fontSize: 10, letterSpacing: ".1em", color: c.muted2, margin: "0 0 6px 2px" }}>
              {SOURCE_LABEL[src]}
            </div>
            <div style={{ display: "flex", flexDirection: "column", gap: 2, fontFamily: mono, fontSize: 11.5 }}>
              {diffs
                .filter((f) => f.source === src)
                .map((f) => (
                  <FileRow key={f.path} f={f} active={f.path === selected} onSelect={() => onSelect(f.path)} />
                ))}
            </div>
          </div>
        ))
      ) : (
        <div style={{ display: "flex", flexDirection: "column", gap: 2, fontFamily: mono, fontSize: 11.5 }}>
          {diffs.map((f) => (
            <FileRow key={f.path} f={f} active={f.path === selected} onSelect={() => onSelect(f.path)} />
          ))}
        </div>
      )}
    </div>
  );
}

export function RightRail({
  tab,
  onTab,
  wide = false,
  onToggleWide,
  netStatus,
  diffs,
  selectedFile,
  onSelectFile,
  onApplyAll,
  onRevert,
}: {
  tab: RailTab;
  onTab: (t: RailTab) => void;
  wide?: boolean;
  onToggleWide?: () => void;
  netStatus: NetStatus;
  diffs: DiffFile[];
  selectedFile: string | null;
  onSelectFile: (path: string) => void;
  onApplyAll: () => void;
  onRevert: () => void;
}) {
  return (
    <div
      style={{
        width: wide ? 452 : 338,
        maxWidth: "100%", // in the phone drawer the viewport, not this, is the cap
        flex: "none",
        background: c.panel2,
        borderLeft: `1px solid ${c.border}`,
        display: "flex",
        flexDirection: "column",
        minHeight: 0,
        transition: "width .2s",
      }}
    >
      <TabHeader
        tab={tab}
        onTab={onTab}
        changesCount={diffs.length}
        pendingCount={0}
        padY={wide ? 2 : 0}
        wide={wide}
        onToggleWide={onToggleWide}
      />
      {tab === "network" ? (
        <NetworkPanel status={netStatus} wide={wide} />
      ) : (
        <ChangesPanel diffs={diffs} selected={selectedFile} onSelect={onSelectFile} onApplyAll={onApplyAll} onRevert={onRevert} />
      )}
    </div>
  );
}
