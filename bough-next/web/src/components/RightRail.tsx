// Right context rail. Two tabs share it — Network (live Claw Patrol feed + pending
// approvals) and Changes (the run's file manifest). Pending pulses the green/amber
// accent; nothing else competes. The Network panel has a compact form (main window)
// and a wide, focused form (screen 3) with filters and a detailed hold card.
import { c, mono, sans } from "../theme";
import type { DiffFile } from "../mock";
import type { NetRequest } from "../types";
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

function verbTint(verb?: string) {
  const v = (verb ?? "").toUpperCase();
  if (v.startsWith("DEL")) return { bg: "rgba(226,119,110,.14)", fg: c.red };
  return { bg: c.panelInset, fg: c.muted };
}

function PendingCard({
  req,
  wide,
  onResolve,
}: {
  req: NetRequest;
  wide: boolean;
  onResolve: (approve: boolean) => void;
}) {
  return (
    <div
      className="pulse-amber"
      style={{
        border: `1px solid ${c.amber}`,
        borderRadius: 11,
        background: "rgba(217,180,95,.07)",
        padding: wide ? 15 : 13,
        marginBottom: wide ? 20 : 18,
        position: "relative",
      }}
    >
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: wide ? 11 : 9 }}>
        <span style={{ fontSize: 10, letterSpacing: ".14em", color: c.amber, fontWeight: 600 }}>⏸ HOLD &amp; ASK</span>
        {wide && <span style={{ fontFamily: mono, fontSize: 10.5, color: c.muted2 }}>waiting 00:14</span>}
      </div>
      {wide ? (
        <>
          <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 9 }}>
            <span
              style={{
                fontFamily: mono,
                fontSize: 10.5,
                fontWeight: 600,
                padding: "2px 7px",
                borderRadius: 5,
                background: "rgba(226,119,110,.15)",
                color: c.red,
              }}
            >
              {req.verb ?? "REQ"}
            </span>
            <span style={{ fontFamily: mono, fontSize: 12.5, color: c.text }}>{req.host}</span>
          </div>
          <div
            style={{
              fontFamily: mono,
              fontSize: 12,
              color: c.text2,
              background: c.panel,
              border: `1px solid ${c.border2}`,
              borderRadius: 7,
              padding: "8px 10px",
              marginBottom: 11,
            }}
          >
            {req.action}
          </div>
        </>
      ) : (
        <>
          <div style={{ fontFamily: mono, fontSize: 12, color: c.text, marginBottom: 4 }}>{req.host}</div>
          <div style={{ fontFamily: mono, fontSize: 12.5, color: c.red, marginBottom: 8 }}>{req.action}</div>
        </>
      )}
      <div style={{ fontSize: 11.5, color: c.muted, lineHeight: 1.55, marginBottom: wide ? 14 : 12 }}>
        {req.reason}{" "}
        {req.requestedBy && (
          <>
            Requested by worker <span style={{ fontFamily: mono }}>{req.requestedBy}</span>.
          </>
        )}
      </div>
      <div style={{ display: "flex", gap: 8, marginBottom: wide ? 9 : 0 }}>
        <button
          onClick={() => onResolve(false)}
          style={{
            flex: 1,
            textAlign: "center",
            padding: wide ? 8 : 7,
            border: `1px solid ${wide ? c.red : c.hairline}`,
            color: c.red,
            borderRadius: 8,
            fontSize: wide ? 12.5 : 12,
            fontWeight: 500,
          }}
        >
          Deny
        </button>
        <button
          onClick={() => onResolve(true)}
          style={{
            flex: 1,
            textAlign: "center",
            padding: wide ? 8 : 7,
            borderRadius: 8,
            background: c.green,
            color: c.bg,
            fontSize: wide ? 12.5 : 12,
            fontWeight: 600,
          }}
        >
          Approve once
        </button>
      </div>
      {wide && (
        <div style={{ textAlign: "center", fontSize: 11, color: c.muted2 }}>
          or <span style={{ color: c.muted, textDecoration: "underline" }}>always allow deletes on octo/*</span>
        </div>
      )}
    </div>
  );
}

function FeedRow({ r }: { r: NetRequest }) {
  const denied = r.verdict === "denied";
  const tint = verbTint(r.verb);
  const rel = (() => {
    const s = Math.round((Date.now() - r.ts) / 1000);
    return s <= 0 ? "now" : `${s}s`;
  })();
  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        gap: 10,
        padding: "8px 6px",
        borderBottom: `1px solid ${c.border3}`,
      }}
    >
      <span style={{ color: denied ? c.red : c.green }}>{denied ? "✗" : "✓"}</span>
      <span
        style={{
          fontSize: 9.5,
          fontWeight: 600,
          padding: "1px 5px",
          borderRadius: 4,
          background: tint.bg,
          color: tint.fg,
        }}
      >
        {(r.verb ?? "").toUpperCase() || "—"}
      </span>
      <div style={{ minWidth: 0, flex: 1 }}>
        <div style={{ color: c.text2, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>
          {r.host}
        </div>
        <div style={{ color: denied ? c.red : c.muted2, fontSize: 10 }}>{r.action}</div>
      </div>
      <span style={{ color: c.muted2 }}>{rel}</span>
    </div>
  );
}

function NetworkPanel({
  net,
  pending,
  wide,
  gateLabel,
  onResolve,
}: {
  net: NetRequest[];
  pending: NetRequest | null;
  wide: boolean;
  gateLabel: string;
  onResolve: (approve: boolean) => void;
}) {
  const allowed = net.filter((n) => n.verdict === "allowed").length;
  const denied = net.filter((n) => n.verdict === "denied").length;
  return (
    <div style={{ flex: 1, overflowY: "auto", padding: wide ? "15px" : "14px 13px", minHeight: 0, display: "flex", flexDirection: "column" }}>
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: wide ? 12 : 14 }}>
        <button
          onClick={() => (location.hash = "bundles")}
          title="Configure gate bundles"
          style={{
            display: "inline-flex",
            alignItems: "center",
            gap: 7,
            fontFamily: mono,
            fontSize: 11.5,
            color: c.muted,
            background: "none",
            border: `1px solid ${c.border2}`,
            borderRadius: 6,
            padding: "3px 9px",
            cursor: "pointer",
          }}
        >
          <Dot /> gate · {gateLabel}
        </button>
        {wide && (
          <span style={{ fontFamily: mono, fontSize: 11, color: c.muted2 }}>
            {`${allowed} allowed · ${denied} denied`}
          </span>
        )}
      </div>

      {wide && (
        <div style={{ display: "flex", gap: 6, marginBottom: 16, fontSize: 11.5 }}>
          <span style={{ padding: "4px 11px", borderRadius: 6, background: "#262b32", color: c.text }}>All</span>
          <span style={{ padding: "4px 11px", borderRadius: 6, color: c.muted2, border: `1px solid ${c.border2}` }}>Allowed</span>
          <span style={{ padding: "4px 11px", borderRadius: 6, color: c.muted2, border: `1px solid ${c.border2}` }}>Denied</span>
          <span style={{ padding: "4px 11px", borderRadius: 6, color: c.amber, border: "1px solid rgba(217,180,95,.4)" }}>
            Pending · {pending ? 1 : 0}
          </span>
        </div>
      )}

      {pending && <PendingCard req={pending} wide={wide} onResolve={onResolve} />}

      <div style={{ display: "flex", alignItems: "center", gap: 7, marginBottom: 11 }}>
        <span style={{ fontFamily: mono, fontSize: 11, color: c.muted2 }}>LIVE FEED</span>
        {wide && (
          <>
            <span style={{ width: 6, height: 6, borderRadius: "50%", background: c.green, marginLeft: 2 }} />
            <span style={{ fontSize: 10.5, color: c.green }}>streaming</span>
          </>
        )}
      </div>
      <div style={{ display: "flex", flexDirection: "column", fontFamily: mono, fontSize: 11 }}>
        {net.map((r) => (
          <FeedRow key={r.id} r={r} />
        ))}
        {net.length === 0 && (
          <span style={{ fontFamily: sans, fontSize: 12, color: c.muted2, lineHeight: 1.5 }}>
            No requests yet. Traffic appears here once the gate is capturing egress.
          </span>
        )}
      </div>
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
  net,
  pending,
  gateLabel = "gate",
  diffs,
  selectedFile,
  onSelectFile,
  onResolve,
  onApplyAll,
  onRevert,
}: {
  tab: RailTab;
  onTab: (t: RailTab) => void;
  wide?: boolean;
  onToggleWide?: () => void;
  net: NetRequest[];
  pending: NetRequest | null;
  gateLabel?: string;
  diffs: DiffFile[];
  selectedFile: string | null;
  onSelectFile: (path: string) => void;
  onResolve: (approve: boolean) => void;
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
        pendingCount={pending ? 1 : 0}
        padY={wide ? 2 : 0}
        wide={wide}
        onToggleWide={onToggleWide}
      />
      {tab === "network" ? (
        <NetworkPanel net={net} pending={pending} wide={wide} gateLabel={gateLabel} onResolve={onResolve} />
      ) : (
        <ChangesPanel diffs={diffs} selected={selectedFile} onSelect={onSelectFile} onApplyAll={onApplyAll} onRevert={onRevert} />
      )}
    </div>
  );
}
