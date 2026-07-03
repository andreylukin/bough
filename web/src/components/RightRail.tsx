// Right context rail. Two tabs share it — Network (live Claw Patrol feed + pending
// approvals + the rule editor) and Changes (the run's file manifest). Pending pulses
// the amber accent; nothing else competes.
import { useState } from "react";
import { c, mono } from "../theme";
import type { DiffFile } from "../mock";
import type { NetConfig, NetStatus, PolicySource } from "../api";
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

const VERDICT_TINT: Record<NetRequest["verdict"], string> = {
  allowed: c.green,
  denied: c.red,
  pending: c.amber,
};

// One row in the live egress feed.
function FeedRow({ r }: { r: NetRequest }) {
  return (
    <div style={{ display: "flex", gap: 8, padding: "7px 2px", borderBottom: `1px solid ${c.border}`, fontFamily: mono, fontSize: 11 }}>
      <span style={{ color: VERDICT_TINT[r.verdict], width: 8, flex: "none" }}>●</span>
      <div style={{ minWidth: 0, flex: 1 }}>
        <div style={{ color: c.text2, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>
          <span style={{ color: c.muted2 }}>{r.verb ?? ""}</span> {r.host}
        </div>
        <div style={{ color: c.muted2, fontSize: 10, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>
          {r.action}
          {r.reason ? ` — ${r.reason}` : ""}
        </div>
      </div>
    </div>
  );
}

// The hold-and-ask card: a request parked on the wire until the operator decides.
function HoldCard({ req, onResolve }: { req: NetRequest; onResolve: (approve: boolean) => void }) {
  return (
    <div style={{ border: `1px solid ${c.amber}`, borderRadius: 8, padding: 12, display: "flex", flexDirection: "column", gap: 8, background: "rgba(217,180,95,.06)" }}>
      <div style={{ display: "flex", alignItems: "center", gap: 7 }}>
        <Dot color={c.amber} pulse />
        <span style={{ fontFamily: mono, fontSize: 12, color: c.text }}>Approval needed</span>
      </div>
      <div style={{ fontFamily: mono, fontSize: 11.5, color: c.text2 }}>
        <span style={{ color: c.muted2 }}>{req.verb ?? ""}</span> {req.host}
      </div>
      <div style={{ fontFamily: mono, fontSize: 11, color: c.muted }}>{req.action}</div>
      {req.reason && <p style={{ fontSize: 12, color: c.muted, lineHeight: 1.5, margin: 0 }}>{req.reason}</p>}
      <div style={{ display: "flex", gap: 8, marginTop: 2 }}>
        <button
          onClick={() => onResolve(true)}
          style={{ flex: 1, fontSize: 12, fontWeight: 600, color: c.bg, background: c.green, borderRadius: 6, padding: "6px 0" }}
        >
          Approve
        </button>
        <button
          onClick={() => onResolve(false)}
          style={{ flex: 1, fontSize: 12, color: c.red, border: `1px solid ${c.border2}`, borderRadius: 6, padding: "6px 0" }}
        >
          Deny
        </button>
      </div>
    </div>
  );
}

// A 2- or 3-way segmented control for a policy enum.
function Segmented<T extends string>(
  { value, options, onChange }: { value: T; options: readonly T[]; onChange: (v: T) => void },
) {
  return (
    <div style={{ display: "flex", gap: 4 }}>
      {options.map((o) => {
        const on = o === value;
        return (
          <button
            key={o}
            onClick={() => onChange(o)}
            style={{
              flex: 1,
              fontFamily: mono,
              fontSize: 11,
              padding: "5px 0",
              borderRadius: 6,
              color: on ? c.bg : c.muted,
              fontWeight: on ? 600 : 400,
              background: on ? c.green : "transparent",
              border: `1px solid ${on ? c.green : c.border2}`,
            }}
          >
            {o}
          </button>
        );
      })}
    </div>
  );
}

function ListField(
  { label, value, onChange }: { label: string; value: string; onChange: (v: string) => void },
) {
  return (
    <label style={{ display: "flex", flexDirection: "column", gap: 4 }}>
      <span style={{ fontFamily: mono, fontSize: 10, letterSpacing: ".08em", color: c.muted2 }}>{label}</span>
      <textarea
        value={value}
        onChange={(e) => onChange(e.target.value)}
        spellCheck={false}
        rows={value.split("\n").length + 1}
        style={{
          fontFamily: mono,
          fontSize: 11,
          color: c.text2,
          background: c.panelInset,
          border: `1px solid ${c.border}`,
          borderRadius: 6,
          padding: "6px 8px",
          resize: "vertical",
          minHeight: 30,
        }}
      />
    </label>
  );
}

const toLines = (xs: string[]) => xs.join("\n");
const fromLines = (s: string) => s.split("\n").map((x) => x.trim()).filter(Boolean);

// The rule editor — the configurable allow/deny/hold sets. Local edit state seeds from
// the live policy; Save PUTs it (the gate hot-swaps, no restart).
function RuleEditor({ policy, onSave }: { policy: NetConfig; onSave: (cfg: NetConfig) => void }) {
  const [mode, setMode] = useState(policy.mode);
  const [hostMiss, setHostMiss] = useState(policy.hostMiss);
  const [allowHosts, setAllowHosts] = useState(toLines(policy.allowHosts));
  const [denyHosts, setDenyHosts] = useState(toLines(policy.denyHosts));
  const [holdVerbs, setHoldVerbs] = useState(toLines(policy.holdVerbs));
  const [denyVerbs, setDenyVerbs] = useState(toLines(policy.denyVerbs));
  const [allowVerbs, setAllowVerbs] = useState(toLines(policy.allowVerbs));

  const save = () =>
    onSave({
      ...policy,
      mode,
      hostMiss,
      allowHosts: fromLines(allowHosts),
      denyHosts: fromLines(denyHosts),
      holdVerbs: fromLines(holdVerbs),
      denyVerbs: fromLines(denyVerbs),
      allowVerbs: fromLines(allowVerbs),
    });

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12, border: `1px solid ${c.border}`, borderRadius: 8, padding: 12 }}>
      <div style={{ display: "flex", flexDirection: "column", gap: 5 }}>
        <span style={{ fontFamily: mono, fontSize: 10, letterSpacing: ".08em", color: c.muted2 }}>MODE · allowed hosts</span>
        <Segmented value={mode} options={["read_only", "review", "all"] as const} onChange={setMode} />
      </div>
      <div style={{ display: "flex", flexDirection: "column", gap: 5 }}>
        <span style={{ fontFamily: mono, fontSize: 10, letterSpacing: ".08em", color: c.muted2 }}>OFF-ALLOWLIST HOST</span>
        <Segmented value={hostMiss} options={["allow", "deny", "hold"] as const} onChange={setHostMiss} />
      </div>
      <ListField label="ALLOW HOSTS" value={allowHosts} onChange={setAllowHosts} />
      <ListField label="DENY HOSTS" value={denyHosts} onChange={setDenyHosts} />
      <ListField label="HOLD VERBS" value={holdVerbs} onChange={setHoldVerbs} />
      <ListField label="DENY VERBS" value={denyVerbs} onChange={setDenyVerbs} />
      <ListField label="ALLOW VERBS" value={allowVerbs} onChange={setAllowVerbs} />
      <button
        onClick={save}
        style={{ fontSize: 12, fontWeight: 600, color: c.bg, background: c.green, borderRadius: 6, padding: "7px 0" }}
      >
        Save rules
      </button>
    </div>
  );
}

// Where the shown rules come from, plus the copy-on-write override controls. Only
// rendered when a session is open (global-only view has nothing to scope).
function ScopeBar(
  { source, onOverride, onClear }: {
    source: PolicySource;
    onOverride: () => void;
    onClear: () => void;
  },
) {
  const owned = source.scope === "session";
  const label = owned
    ? "rules: this branch"
    : source.scope === "inherited"
    ? `rules: inherited from ${source.sessionId?.slice(0, 8)}`
    : "rules: global";
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
      <span style={{ fontFamily: mono, fontSize: 10.5, color: owned ? c.green : c.muted2 }}>
        {label}
      </span>
      <button
        onClick={owned ? onClear : onOverride}
        title={owned
          ? "Drop this branch's override; it inherits its ancestor's / the global rules again"
          : "Pin this branch to its own copy of these rules; edits then apply to this branch only"}
        style={{ marginLeft: "auto", fontFamily: mono, fontSize: 10.5, color: c.muted, background: "none", border: `1px solid ${c.border2}`, borderRadius: 6, padding: "2px 8px" }}
      >
        {owned ? "remove override" : "override for this branch"}
      </button>
    </div>
  );
}

// bough runs the egress firewall in-process: this panel shows its status, the live
// feed, the hold-and-ask cards, and the editable rule set.
function NetworkPanel(
  { status, net, pending, onResolve, policy, policySource, onSavePolicy, onOverridePolicy, onClearPolicyOverride, sessionOpen, wide }: {
    status: NetStatus;
    net: NetRequest[];
    pending: NetRequest | null;
    onResolve: (approve: boolean) => void;
    policy: NetConfig | null;
    policySource: PolicySource | null;
    onSavePolicy: (cfg: NetConfig) => void;
    onOverridePolicy: () => void;
    onClearPolicyOverride: () => void;
    sessionOpen: boolean;
    wide: boolean;
  },
) {
  const [editing, setEditing] = useState(false);
  const dotColor = status.running ? c.green : status.enabled ? c.amber : c.muted2;
  const statusLabel = status.running ? "proxy up" : status.enabled ? "starting" : "off";
  return (
    <div style={{ flex: 1, overflowY: "auto", padding: wide ? "15px" : "14px 13px", minHeight: 0, display: "flex", flexDirection: "column", gap: 14 }}>
      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
        <Dot color={dotColor} pulse={status.running} />
        <span style={{ fontFamily: mono, fontSize: 12, color: c.text }}>Claw Patrol</span>
        <span style={{ fontFamily: mono, fontSize: 10.5, color: c.muted2 }}>{statusLabel}</span>
        <button
          onClick={() => setEditing((e) => !e)}
          title="Edit the allow/deny/hold rule set"
          style={{ marginLeft: "auto", fontFamily: mono, fontSize: 11, color: editing ? c.green : c.muted, background: "none", border: `1px solid ${c.border2}`, borderRadius: 6, padding: "3px 9px" }}
        >
          ⚙ Rules
        </button>
      </div>

      {!status.enabled && (
        <p style={{ fontSize: 12.5, color: c.muted, lineHeight: 1.55, margin: 0 }}>
          Egress gating is off. Start bough with <code style={{ fontFamily: mono, color: c.text2 }}>BOUGH_CLAWPATROL=1</code> to route sandbox traffic through the in-process proxy.
        </p>
      )}

      {pending && <HoldCard req={pending} onResolve={onResolve} />}

      {editing && (policy
        ? (
          <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
            {sessionOpen && policySource && (
              <ScopeBar
                source={policySource}
                onOverride={onOverridePolicy}
                onClear={onClearPolicyOverride}
              />
            )}
            <RuleEditor
              // Re-seed the editor's local state when the scope flips (override/clear).
              key={`${policySource?.scope ?? "global"}:${policySource?.sessionId ?? ""}`}
              policy={policy}
              onSave={(cfg) => onSavePolicy(cfg)}
            />
          </div>
        )
        : <p style={{ fontSize: 12, color: c.muted2, margin: 0 }}>Loading rules…</p>)}

      <div style={{ display: "flex", flexDirection: "column" }}>
        <span style={{ fontFamily: mono, fontSize: 10, letterSpacing: ".1em", color: c.muted2, margin: "0 0 4px 2px" }}>
          FEED {net.length > 0 && <Chip>{net.length}</Chip>}
        </span>
        {net.length === 0
          ? <div style={{ fontSize: 12, color: c.muted2, padding: "8px 2px" }}>No egress yet. Gated requests from sandbox commands appear here.</div>
          : net.map((r) => <FeedRow key={r.id} r={r} />)}
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
  netStatus,
  net,
  pending,
  onResolve,
  policy,
  policySource,
  onSavePolicy,
  onOverridePolicy,
  onClearPolicyOverride,
  sessionOpen = false,
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
  net: NetRequest[];
  pending: NetRequest | null;
  onResolve: (approve: boolean) => void;
  policy: NetConfig | null;
  policySource?: PolicySource | null;
  onSavePolicy: (cfg: NetConfig) => void;
  onOverridePolicy?: () => void;
  onClearPolicyOverride?: () => void;
  /** True when a session is open — enables the branch-scope controls. */
  sessionOpen?: boolean;
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
        pendingCount={pending ? 1 : 0}
        padY={wide ? 2 : 0}
        wide={wide}
        onToggleWide={onToggleWide}
      />
      {tab === "network" ? (
        <NetworkPanel
          status={netStatus}
          net={net}
          pending={pending}
          onResolve={onResolve}
          policy={policy}
          policySource={policySource ?? null}
          onSavePolicy={onSavePolicy}
          onOverridePolicy={onOverridePolicy ?? (() => {})}
          onClearPolicyOverride={onClearPolicyOverride ?? (() => {})}
          sessionOpen={sessionOpen}
          wide={wide}
        />
      ) : (
        <ChangesPanel diffs={diffs} selected={selectedFile} onSelect={onSelectFile} onApplyAll={onApplyAll} onRevert={onRevert} />
      )}
    </div>
  );
}
