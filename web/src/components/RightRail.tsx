// Right context rail. Three tabs share it — Network (live Claw Patrol feed + pending
// approvals + the rule editor), Changes (the run's file manifest), and MCP (server
// registry, branch grants, connect-proof). Pending pulses the amber accent; nothing
// else competes.
import { useEffect, useState } from "react";
import { c, alpha, mono } from "../theme";
import type { DiffFile } from "../mock";
import { api, type McpConnectResult, type McpServerEntry, type McpStatus, type NetConfig, type NetStatus, type OpRule, type PluginActivation, type PluginInfo, type PolicySource } from "../api";
import type { NetRequest } from "../types";
import { Chip, Dot } from "./ui";

export type RailTab = "network" | "changes" | "mcp";

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
            <Chip style={{ background: alpha(c.amber, 18), color: c.amber }}>{pendingCount}</Chip>
          ) : (
            <Dot />
          )}
        </>
      )}
      {item("changes", <>Changes {changesCount > 0 && <Chip>{changesCount}</Chip>}</>)}
      {item("mcp", <>MCP</>)}
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

// A run of identical egress (same host + verb + action + verdict), folded into one
// feed row so a busy loop doesn't flood the panel. rep = newest; ids = every member.
interface FeedGroup {
  key: string;
  rep: NetRequest;
  ids: string[];
  count: number;
  firstTs: number;
  lastTs: number;
}

// Fold newest-first `net` into groups, preserving newest-first group order.
function groupFeed(net: NetRequest[]): FeedGroup[] {
  const by = new Map<string, FeedGroup>();
  const order: string[] = [];
  for (const r of net) {
    const key = `${r.host}|${r.verb ?? ""}|${r.action}|${r.verdict}`;
    let g = by.get(key);
    if (!g) {
      g = { key, rep: r, ids: [], count: 0, firstTs: r.ts, lastTs: r.ts };
      by.set(key, g);
      order.push(key);
    }
    g.ids.push(r.id);
    g.count++;
    g.firstTs = Math.min(g.firstTs, r.ts);
    g.lastTs = Math.max(g.lastTs, r.ts);
  }
  return order.map((k) => by.get(k)!);
}

function relTime(ts: number, now = Date.now()): string {
  const s = Math.max(0, Math.round((now - ts) / 1000));
  if (s < 5) return "just now";
  if (s < 60) return `${s}s ago`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.round(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.round(h / 24)}d ago`;
}

function DetailRow({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div style={{ display: "flex", gap: 8, fontFamily: mono, fontSize: 10.5 }}>
      <span style={{ color: c.muted2, width: 66, flex: "none" }}>{label}</span>
      <span style={{ color: c.text2, minWidth: 0, wordBreak: "break-all" }}>{value}</span>
    </div>
  );
}

// Facet fields (the classifier's parsed view — k8s resource/namespace, a plugin's
// extract() output) rendered as key=value chips.
function FieldChips({ fields }: { fields: Record<string, unknown> }) {
  const entries = Object.entries(fields).filter(([, v]) => v !== "" && v != null);
  if (entries.length === 0) return null;
  return (
    <span style={{ display: "flex", flexWrap: "wrap", gap: 4 }}>
      {entries.map(([k, v]) => (
        <span
          key={k}
          style={{ border: `1px solid ${c.border2}`, borderRadius: 5, padding: "0 5px", whiteSpace: "nowrap" }}
        >
          <span style={{ color: c.muted2 }}>{k}=</span>
          {String(v)}
        </span>
      ))}
    </span>
  );
}

// One feed row = one group. Click the body to analyze (expand full detail); the
// checkbox selects the whole group (all member ids) for ✨ Group into plugin.
function FeedGroupRow(
  { g, selected, onSelect }: { g: FeedGroup; selected: boolean; onSelect: () => void },
) {
  const [open, setOpen] = useState(false);
  const r = g.rep;
  const tint = VERDICT_TINT[r.verdict];
  return (
    <div
      style={{
        borderBottom: `1px solid ${c.border}`,
        background: selected ? alpha(c.green, 8) : open ? c.panelInset : undefined,
        borderLeft: selected ? `2px solid ${c.green}` : "2px solid transparent",
      }}
    >
      <div style={{ display: "flex", gap: 8, padding: "7px 2px", fontFamily: mono, fontSize: 11 }}>
        <input
          type="checkbox"
          checked={selected}
          onChange={onSelect}
          onClick={(e) => e.stopPropagation()}
          title="Select this group for ✨ Group into plugin"
          style={{ flex: "none", accentColor: c.green, marginTop: 2, cursor: "pointer" }}
        />
        <div
          onClick={() => setOpen((o) => !o)}
          title={open ? "Collapse" : "Click to analyze this request"}
          style={{ minWidth: 0, flex: 1, cursor: "pointer" }}
        >
          <div style={{ color: c.text2, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>
            <span style={{ color: tint, marginRight: 5 }}>●</span>
            <span style={{ color: c.muted2 }}>{r.verb ?? ""}</span> {r.host}
            {g.count > 1 && <span style={{ color: c.muted2 }}>{"  "}×{g.count}</span>}
          </div>
          <div style={{ color: c.muted2, fontSize: 10, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>
            {r.action}
            {r.reason ? ` — ${r.reason}` : ""}
          </div>
        </div>
      </div>
      {open && (
        <div style={{ display: "flex", flexDirection: "column", gap: 3, padding: "2px 2px 9px 24px" }}>
          <DetailRow label="verdict" value={<span style={{ color: tint }}>{r.verdict}</span>} />
          <DetailRow label="host" value={r.host} />
          <DetailRow label="method" value={r.verb ?? "—"} />
          <DetailRow label="action" value={r.action} />
          {r.reason && <DetailRow label="reason" value={r.reason} />}
          {r.annotation && <DetailRow label="summary" value={r.annotation} />}
          {r.fields && Object.keys(r.fields).length > 0 && (
            <DetailRow label="facets" value={<FieldChips fields={r.fields} />} />
          )}
          {r.requestedBy && <DetailRow label="by" value={r.requestedBy} />}
          <DetailRow
            label={g.count > 1 ? "seen" : "at"}
            value={g.count > 1
              ? `${g.count}× · ${relTime(g.firstTs)} → ${relTime(g.lastTs)}`
              : relTime(r.ts)}
          />
          {r.sessionId && <DetailRow label="session" value={r.sessionId.slice(0, 8)} />}
        </div>
      )}
    </div>
  );
}

// The hold-and-ask card: a request parked on the wire until the operator decides.
function HoldCard(
  { req, onResolve, onRefine, queued = 0 }: {
    req: NetRequest;
    onResolve: (approve: boolean, scope?: "once" | "session") => void;
    onRefine?: () => void;
    /** Additional holds waiting behind this one. */
    queued?: number;
  },
) {
  return (
    <div style={{ border: `1px solid ${c.amber}`, borderRadius: 8, padding: 12, display: "flex", flexDirection: "column", gap: 8, background: alpha(c.amber, 6) }}>
      <div style={{ display: "flex", alignItems: "center", gap: 7 }}>
        <Dot color={c.amber} pulse />
        <span style={{ fontFamily: mono, fontSize: 12, color: c.text }}>Approval needed</span>
        {queued > 0 && (
          <span style={{ fontFamily: mono, fontSize: 10.5, color: c.amber }}>+{queued} waiting</span>
        )}
        {onRefine && (
          <button
            onClick={onRefine}
            title="Open the rule editor and draft a refinement with AI — this request rides along as context"
            style={{ marginLeft: "auto", fontFamily: mono, fontSize: 10.5, color: c.muted, background: "none", border: `1px solid ${c.border2}`, borderRadius: 6, padding: "2px 8px" }}
          >
            ✨ refine rules
          </button>
        )}
      </div>
      <div style={{ fontFamily: mono, fontSize: 11.5, color: c.text2 }}>
        <span style={{ color: c.muted2 }}>{req.verb ?? ""}</span> {req.host}
      </div>
      <div style={{ fontFamily: mono, fontSize: 11, color: c.muted }}>{req.action}</div>
      {/* Local-worker one-liner: what this request actually does (advisory). */}
      {req.annotation && (
        <p style={{ fontSize: 12, color: c.text2, fontStyle: "italic", lineHeight: 1.5, margin: 0 }}>
          {req.annotation}
        </p>
      )}
      {req.fields && Object.keys(req.fields).length > 0 && (
        <div style={{ fontFamily: mono, fontSize: 10.5, color: c.text2 }}>
          <FieldChips fields={req.fields} />
        </div>
      )}
      {req.reason && <p style={{ fontSize: 12, color: c.muted, lineHeight: 1.5, margin: 0 }}>{req.reason}</p>}
      <div style={{ display: "flex", gap: 8, marginTop: 2 }}>
        <button
          onClick={() => onResolve(true, "once")}
          title="Allow just this request"
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
      {/* Allow this host+verb for the rest of the session (short TTL) — so a retried
          command (incl. one whose socket already timed out) passes without re-asking. */}
      <button
        onClick={() => onResolve(true, "session")}
        title="Allow this host + action for the rest of the session, so a retry passes without asking again"
        style={{ fontSize: 11, color: c.green, background: "none", border: `1px solid ${alpha(c.green, 40)}`, borderRadius: 6, padding: "5px 0" }}
      >
        Allow {req.verb ?? ""} on {req.host} for this session
      </button>
    </div>
  );
}

// One-time macOS keychain-trust nudge. Sandboxed curl/git trust the MITM CA via env
// vars, but Go tools (gh, some kubectl auth plugins) consult the system keychain and
// won't work through the proxy until the CA is trusted there. Shows the exact command
// (copyable) and a re-check that clears the card once it's done — no restart needed.
function CaTrustHint({ command }: { command: string }) {
  const [done, setDone] = useState(false);
  const [copied, setCopied] = useState(false);
  const [checking, setChecking] = useState(false);
  if (done) return null;
  const recheck = async () => {
    setChecking(true);
    try {
      const s = await api.recheckCa();
      if (s.caTrusted) setDone(true);
    } finally {
      setChecking(false);
    }
  };
  return (
    <div style={{ border: `1px solid ${c.border2}`, borderRadius: 8, padding: 12, display: "flex", flexDirection: "column", gap: 8, background: c.panelInset }}>
      <div style={{ fontFamily: mono, fontSize: 11.5, color: c.text2 }}>⚿ Trust the CA for Go tools</div>
      <p style={{ fontSize: 11.5, color: c.muted, lineHeight: 1.5, margin: 0 }}>
        Sandboxed curl/git already trust bough's CA. Go tools like <code style={{ fontFamily: mono, color: c.text2 }}>gh</code> and some kubectl auth plugins consult the macOS keychain instead — run this once so they work through Claw Patrol:
      </p>
      <code
        onClick={() => {
          navigator.clipboard?.writeText(command);
          setCopied(true);
          setTimeout(() => setCopied(false), 1200);
        }}
        title="Click to copy"
        style={{ fontFamily: mono, fontSize: 10.5, color: c.text2, background: c.bg, border: `1px solid ${c.border2}`, borderRadius: 6, padding: "6px 8px", wordBreak: "break-all", cursor: "pointer" }}
      >
        {command}
      </code>
      <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
        <button
          onClick={recheck}
          disabled={checking}
          style={{ fontFamily: mono, fontSize: 10.5, color: checking ? c.muted2 : c.green, background: "none", border: `1px solid ${c.border2}`, borderRadius: 6, padding: "3px 10px" }}
        >
          {checking ? "checking…" : "I've run it — re-check"}
        </button>
        {copied && <span style={{ fontSize: 10.5, color: c.green }}>copied</span>}
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
        <Segmented value={mode} options={["read_only", "review", "all", "yolo"] as const} onChange={setMode} />
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

// Plain-language rule drafting: describe the task ("research chocolate shops with
// exa"), the model proposes a least-privilege config, and the draft lands in the
// editor below for review — Save is what makes it live. When a hold is pending it
// rides along as context, so "let this one through but keep writes held" works.
function SuggestBox(
  { sessionId, onDraft }: {
    sessionId: string | null;
    onDraft: (d: { config: NetConfig; rationale: string }) => void;
  },
) {
  const [prompt, setPrompt] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const go = async () => {
    if (!prompt.trim() || busy) return;
    setBusy(true);
    setErr(null);
    try {
      onDraft(await api.suggestPolicy(prompt, sessionId));
    } catch (e) {
      setErr((e as Error).message || "suggestion failed");
    } finally {
      setBusy(false);
    }
  };
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
      <textarea
        value={prompt}
        onChange={(e) => setPrompt(e.target.value)}
        rows={2}
        placeholder="Describe the task (e.g. research chocolate shops in Boston with exa) or a refinement (allow this, but hold GraphQL mutations)…"
        style={{ fontFamily: mono, fontSize: 11.5, color: c.text2, background: c.bg, border: `1px solid ${c.border2}`, borderRadius: 6, padding: "6px 8px", resize: "vertical" }}
      />
      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
        <button
          onClick={go}
          disabled={busy || !prompt.trim()}
          style={{ fontFamily: mono, fontSize: 11, color: busy ? c.muted2 : c.green, background: "none", border: `1px solid ${c.border2}`, borderRadius: 6, padding: "3px 10px" }}
        >
          {busy ? "drafting…" : "✨ Draft rules with AI"}
        </button>
        {err && <span style={{ fontSize: 11, color: c.red }}>{err}</span>}
      </div>
    </div>
  );
}

const KIND_TINT: Record<OpRule["kind"], string> = {
  read: c.green,
  write: c.amber,
  unknown: c.muted2,
};

// A plugin's declarative classifier table, rendered as data — match → kind (→ verb).
function OpsTable({ ops }: { ops: OpRule[] }) {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 1 }}>
      {ops.map((op, i) => (
        <div key={i} style={{ display: "flex", gap: 8, fontFamily: mono, fontSize: 10.5 }}>
          <span style={{ color: c.text2, whiteSpace: "nowrap" }}>{op.match}</span>
          <span style={{ color: KIND_TINT[op.kind] }}>{op.kind}</span>
          {op.verb && <span style={{ color: c.muted2 }}>→ {op.verb}</span>}
        </div>
      ))}
    </div>
  );
}

// Classifier plugins: teach the gate one provider's verb vocabulary so DESTRUCTIVE
// operations can be held/denied per-op while reads flow. Files are a shared LIBRARY;
// what this panel toggles is the open branch's ACTIVATIONS (inherited by its
// children), each with its own optional TTL — so one plugin can run open-ended here
// and lapse after 2h elsewhere. Creation stays skill-first: /net-plugin drafts,
// installs, and live-tests against real traffic.
function PluginsPanel({ sessionId, activations, onPolicyChanged, reloadSignal = 0 }: {
  sessionId: string | null;
  activations: PluginActivation[];
  onPolicyChanged: () => void;
  reloadSignal?: number;
}) {
  const [dir, setDir] = useState("");
  const [plugins, setPlugins] = useState<PluginInfo[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  // Per-plugin TTL choice for the NEXT enable ("" = no expiry).
  const [ttls, setTtls] = useState<Record<string, string>>({});
  // Cards are collapsed to one row by default — the library grows; details on demand.
  const [open, setOpen] = useState<Set<string>>(new Set());
  const toggleOpen = (name: string) =>
    setOpen((prev) => {
      const next = new Set(prev);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });

  const refresh = async () => {
    try {
      const { dir, plugins } = await api.listPlugins();
      setDir(dir);
      setPlugins(plugins);
    } catch {
      setPlugins([]);
    }
  };
  useEffect(() => {
    refresh();
  }, [reloadSignal]); // re-fetch after a "group into plugin" installs a new one

  const reload = async () => {
    setBusy(true);
    setErr(null);
    try {
      setPlugins((await api.reloadPlugins()).plugins);
    } catch (e) {
      setErr((e as Error).message || "reload failed");
    } finally {
      setBusy(false);
    }
  };

  const openInEditor = async (name: string) => {
    setErr(null);
    try {
      await api.openPlugin(name);
    } catch (e) {
      setErr((e as Error).message || "could not open editor");
    }
  };

  const toggle = async (name: string, on: boolean) => {
    setBusy(true);
    setErr(null);
    try {
      await api.setPlugin(name, on, sessionId, on ? ttls[name] || undefined : undefined);
      onPolicyChanged(); // the branch's policy row changed server-side — re-sync
    } catch (e) {
      setErr((e as Error).message || "toggle failed");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
        <span style={{ fontFamily: mono, fontSize: 10, letterSpacing: ".1em", color: c.muted2 }}>
          PLUGINS {plugins && plugins.length > 0 && <Chip>{plugins.length}</Chip>}
        </span>
        <button
          onClick={reload}
          disabled={busy}
          title="Re-load the plugins dir after editing a file (no restart)"
          style={{ marginLeft: "auto", fontFamily: mono, fontSize: 10.5, color: c.muted, background: "none", border: `1px solid ${c.border2}`, borderRadius: 6, padding: "2px 8px" }}
        >
          ↻ Reload
        </button>
      </div>
      <p style={{ fontSize: 11.5, color: c.muted, lineHeight: 1.5, margin: 0 }}>
        A plugin maps one provider's API onto verbs the rule set can gate, so specific
        destructive operations are held or denied while reads flow. Expired plugins stop
        gating and their hosts fail closed again.
      </p>
      {plugins?.map((p) => {
        const act = activations.find((a) => a.name === p.name);
        const expired = act?.expires !== undefined && Date.parse(act.expires) <= Date.now();
        const on = !!act && !expired;
        const tint = p.status === "error" ? c.red : on ? c.green : expired ? c.amber : c.muted2;
        const opened = open.has(p.name);
        return (
          <div key={p.file} style={{ display: "flex", flexDirection: "column", gap: 3, borderLeft: `2px solid ${tint}`, paddingLeft: 8 }}>
            <div
              onClick={() => toggleOpen(p.name)}
              title={opened ? "Collapse" : "Show hosts + ops table"}
              style={{ display: "flex", alignItems: "center", gap: 6, cursor: "pointer" }}
            >
              <span style={{ fontFamily: mono, fontSize: 9, color: c.muted2, width: 8, flex: "none" }}>
                {opened ? "▾" : "▸"}
              </span>
              <span style={{ fontFamily: mono, fontSize: 11.5, color: c.text2 }}>{p.name}</span>
              {p.status === "error"
                ? <span style={{ fontFamily: mono, color: c.red, fontSize: 10 }}>broken</span>
                : (
                  <span style={{ fontFamily: mono, color: tint, fontSize: 10 }}>
                    {on
                      ? act?.expires ? `on · until ${new Date(act.expires).toLocaleTimeString()}` : "on"
                      : expired
                      ? `expired ${new Date(act!.expires!).toLocaleTimeString()}`
                      : "off"}
                  </span>
                )}
              {p.status !== "error" && (
                <span
                  onClick={(e) => e.stopPropagation()}
                  style={{ marginLeft: "auto", display: "flex", gap: 5, alignItems: "center" }}
                >
                  {!on && (
                    <select
                      value={ttls[p.name] ?? ""}
                      onChange={(e) => setTtls((prev) => ({ ...prev, [p.name]: e.target.value }))}
                      title="TTL for THIS activation only — the same plugin can run with a different one elsewhere"
                      style={{ fontFamily: mono, fontSize: 10, color: c.text2, background: c.bg, border: `1px solid ${c.border2}`, borderRadius: 6, padding: "2px 4px" }}
                    >
                      <option value="">no expiry</option>
                      <option value="2h">2h</option>
                      <option value="24h">24h</option>
                      <option value="7d">7d</option>
                    </select>
                  )}
                  <button
                    onClick={() => toggle(p.name, !on)}
                    disabled={busy}
                    title={on
                      ? "Turn this plugin off for this branch (its hosts fail closed again)"
                      : "Turn this plugin on for this branch (children inherit it)"}
                    style={{ fontFamily: mono, fontSize: 10.5, color: busy ? c.muted2 : on ? c.red : c.green, background: "none", border: `1px solid ${c.border2}`, borderRadius: 6, padding: "2px 8px" }}
                  >
                    {on ? "Disable" : "Enable"}
                  </button>
                </span>
              )}
            </div>
            {opened && (
              <>
                <span style={{ fontFamily: mono, fontSize: 10, color: c.muted2, wordBreak: "break-all" }}>
                  {p.hosts.join(", ")}
                  {p.hasClassify ? " · +classify()" : ""}
                  {p.hasGate ? " · +gate()" : ""}
                </span>
                {p.description && (
                  <span style={{ fontSize: 10.5, color: c.muted }}>{p.description}</span>
                )}
                {p.ops && <OpsTable ops={p.ops} />}
                {p.error && <span style={{ fontFamily: mono, fontSize: 10.5, color: c.red }}>{p.error}</span>}
                <span style={{ display: "flex", alignItems: "center", gap: 6 }}>
                  <button
                    onClick={() => openInEditor(p.name)}
                    title="Open this plugin's definition in your editor (BOUGH_EDITOR, else the OS text editor); hit Reload after saving"
                    style={{ fontFamily: mono, fontSize: 10.5, color: c.green, background: "none", border: `1px solid ${c.border2}`, borderRadius: 6, padding: "2px 8px", whiteSpace: "nowrap", flex: "none" }}
                  >
                    ✎ Edit
                  </button>
                  <span style={{ fontFamily: mono, fontSize: 9.5, color: c.muted2, wordBreak: "break-all" }}>{p.file}</span>
                </span>
              </>
            )}
          </div>
        );
      })}
      {plugins && plugins.length === 0 && (
        <div style={{ fontSize: 11.5, color: c.muted2 }}>
          No plugins yet.{dir ? ` They live in ${dir}.` : ""}
        </div>
      )}
      <div style={{ fontSize: 11, color: c.muted2 }}>
        Create one by typing <span style={{ fontFamily: mono, color: c.green }}>/net-plugin</span>{" "}
        in the composer — the agent drafts the table, installs it, and live-tests the
        verdicts against real traffic.
      </div>
      {err && <span style={{ fontSize: 10.5, color: c.red, wordBreak: "break-all" }}>{err}</span>}
    </div>
  );
}

// Split an edited command line into argv, honoring '…' and "…" quoting so paths
// with spaces survive the round-trip back into {command, args}.
function splitCommandLine(s: string): string[] {
  const out: string[] = [];
  const re = /"([^"]*)"|'([^']*)'|(\S+)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(s))) out.push(m[1] ?? m[2] ?? m[3]);
  return out;
}

// MCP servers: the global registry with this branch's activations, live
// connections, and OAuth state. Reading and toggling live here; "Test" proves a
// server actually runs (spawns it under the session's confinement and lists its
// tools); the command/url line is click-to-edit and saves through the per-server
// PUT (validated server-side, connections dropped so the old process can't keep
// serving). Creating NEW entries stays skill-first via /mcp.
function McpPanel({ sessionId }: { sessionId: string | null }) {
  const [status, setStatus] = useState<McpStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [ttls, setTtls] = useState<Record<string, string>>({});
  const [open, setOpen] = useState<Set<string>>(new Set());
  // Per-server outcome of the last "Test": tools on success, error text on failure.
  const [proofs, setProofs] = useState<Record<string, McpConnectResult>>({});
  // Remote-server auth handoff: the URL the human must open, per server.
  const [authUrls, setAuthUrls] = useState<Record<string, string>>({});
  // Click-to-edit draft of one server's command line (stdio) or url (remote).
  const [editing, setEditing] = useState<{ name: string; text: string } | null>(null);

  const refresh = async () => {
    try {
      setStatus(await api.mcpStatus(sessionId));
      setErr(null);
    } catch (e) {
      setErr((e as Error).message || "failed to load MCP state");
    }
  };
  useEffect(() => {
    refresh();
  }, [sessionId]);

  const toggleOpen = (name: string) =>
    setOpen((prev) => {
      const next = new Set(prev);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });

  const toggle = async (name: string, on: boolean) => {
    setBusy(true);
    setErr(null);
    try {
      await api.setMcpServer(name, on, sessionId, on ? ttls[name] || undefined : undefined);
      await refresh();
    } catch (e) {
      setErr((e as Error).message || "toggle failed");
    } finally {
      setBusy(false);
    }
  };

  const test = async (name: string) => {
    if (!sessionId) return;
    setBusy(true);
    setErr(null);
    try {
      const res = await api.connectMcpServer(name, sessionId);
      setProofs((prev) => ({ ...prev, [name]: res }));
      await refresh();
    } catch (e) {
      setProofs((prev) => ({
        ...prev,
        [name]: { server: name, connected: false, error: (e as Error).message },
      }));
    } finally {
      setBusy(false);
    }
  };

  const saveEdit = async (name: string, cfg: McpServerEntry) => {
    if (!editing || editing.name !== name) return;
    const text = editing.text.trim();
    setBusy(true);
    setErr(null);
    try {
      // Same transport kind as before; env survives untouched. An empty line, a
      // bad url, etc. come back as the server's 400 message — the draft stays
      // open so the user can fix it.
      const entry = cfg.url
        ? { url: text, env: cfg.env ?? {} }
        : (() => {
          const argv = splitCommandLine(text);
          return { command: argv[0] ?? "", args: argv.slice(1), env: cfg.env ?? {} };
        })();
      await api.putMcpServer(name, entry);
      setEditing(null);
      // The old process was dropped server-side; the last proof is stale now.
      setProofs((prev) => {
        const next = { ...prev };
        delete next[name];
        return next;
      });
      await refresh();
    } catch (e) {
      setErr((e as Error).message || "save failed");
    } finally {
      setBusy(false);
    }
  };

  const authorize = async (name: string) => {
    setBusy(true);
    setErr(null);
    try {
      const res = await api.startMcpAuth(name);
      if (res.status === "authorized") await refresh();
      else {
        setAuthUrls((prev) => ({ ...prev, [name]: res.authorizationUrl }));
        window.open(res.authorizationUrl, "_blank");
      }
    } catch (e) {
      setErr((e as Error).message || "auth failed");
    } finally {
      setBusy(false);
    }
  };

  const servers = Object.entries(status?.registry.servers ?? {});
  return (
    <div style={{ flex: 1, minHeight: 0, overflowY: "auto", padding: "12px 14px", display: "flex", flexDirection: "column", gap: 8 }}>
      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
        <span style={{ fontFamily: mono, fontSize: 10, letterSpacing: ".1em", color: c.muted2 }}>
          SERVERS {servers.length > 0 && <Chip>{servers.length}</Chip>}
        </span>
        <button
          onClick={refresh}
          disabled={busy}
          title="Re-read the registry, activations, and live connections"
          style={{ marginLeft: "auto", fontFamily: mono, fontSize: 10.5, color: c.muted, background: "none", border: `1px solid ${c.border2}`, borderRadius: 6, padding: "2px 8px" }}
        >
          ↻ Refresh
        </button>
      </div>
      <p style={{ fontSize: 11.5, color: c.muted, lineHeight: 1.5, margin: 0 }}>
        MCP servers are defined once in a global registry; enabling one grants its tools
        to this branch's turns. Every tool call still passes the egress gate. Test spawns
        the server now and lists its tools — proof it runs before a turn depends on it.
      </p>
      {servers.map(([name, cfg]) => {
        const conn = status?.connections.find((x) => x.server === name);
        const on = status?.active.includes(name) ?? false;
        const remote = !!cfg.url;
        const authorized = !remote || (status?.auth[name]?.authorized ?? false);
        const proof = proofs[name];
        const tint = conn?.alive ? c.green : on ? c.amber : c.muted2;
        const opened = open.has(name);
        const transport = remote ? cfg.url : [cfg.command, ...(cfg.args ?? [])].join(" ");
        return (
          <div key={name} style={{ display: "flex", flexDirection: "column", gap: 3, borderLeft: `2px solid ${tint}`, paddingLeft: 8 }}>
            <div
              onClick={() => toggleOpen(name)}
              title={opened ? "Collapse" : "Show transport, auth, and last test"}
              style={{ display: "flex", alignItems: "center", gap: 6, cursor: "pointer" }}
            >
              <span style={{ fontFamily: mono, fontSize: 9, color: c.muted2, width: 8, flex: "none" }}>
                {opened ? "▾" : "▸"}
              </span>
              <span style={{ fontFamily: mono, fontSize: 11.5, color: c.text2 }}>{name}</span>
              <span style={{ fontFamily: mono, fontSize: 10, color: tint }}>
                {conn?.alive
                  ? `connected · ${conn.toolCount} tools`
                  : on
                  ? "enabled · connects next turn"
                  : remote && !authorized
                  ? "not authorized"
                  : "off"}
              </span>
              <span
                onClick={(e) => e.stopPropagation()}
                style={{ marginLeft: "auto", display: "flex", gap: 5, alignItems: "center" }}
              >
                {!on && (
                  <select
                    value={ttls[name] ?? ""}
                    onChange={(e) => setTtls((prev) => ({ ...prev, [name]: e.target.value }))}
                    title="TTL for THIS activation only — a lapsed grant fails closed"
                    style={{ fontFamily: mono, fontSize: 10, color: c.text2, background: c.bg, border: `1px solid ${c.border2}`, borderRadius: 6, padding: "2px 4px" }}
                  >
                    <option value="">no expiry</option>
                    <option value="2h">2h</option>
                    <option value="24h">24h</option>
                    <option value="7d">7d</option>
                  </select>
                )}
                <button
                  onClick={() => toggle(name, !on)}
                  disabled={busy || !sessionId}
                  title={on
                    ? "Remove this branch's grant and drop the connection"
                    : "Grant this server's tools to this branch (mcp() appears next turn)"}
                  style={{ fontFamily: mono, fontSize: 10.5, color: busy ? c.muted2 : on ? c.red : c.green, background: "none", border: `1px solid ${c.border2}`, borderRadius: 6, padding: "2px 8px" }}
                >
                  {on ? "Disable" : "Enable"}
                </button>
              </span>
            </div>
            {opened && (
              <>
                {editing?.name === name
                  ? (
                    <span style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                      <textarea
                        value={editing.text}
                        onChange={(e) => setEditing({ name, text: e.target.value })}
                        onKeyDown={(e) => {
                          if (e.key === "Enter" && !e.shiftKey) {
                            e.preventDefault();
                            saveEdit(name, cfg);
                          }
                          if (e.key === "Escape") setEditing(null);
                        }}
                        rows={2}
                        autoFocus
                        spellCheck={false}
                        style={{ fontFamily: mono, fontSize: 10.5, color: c.text, background: c.bg, border: `1px solid ${c.border2}`, borderRadius: 6, padding: "4px 6px", resize: "vertical", width: "100%" }}
                      />
                      <span style={{ display: "flex", alignItems: "center", gap: 6 }}>
                        <button
                          onClick={() => saveEdit(name, cfg)}
                          disabled={busy}
                          title={remote
                            ? "Save the new URL (existing connections drop; re-Test after)"
                            : "Save the new command (quotes keep spaces together; env is untouched; existing connections drop — re-Test after)"}
                          style={{ fontFamily: mono, fontSize: 10.5, color: c.green, background: "none", border: `1px solid ${c.border2}`, borderRadius: 6, padding: "2px 8px" }}
                        >
                          Save
                        </button>
                        <button
                          onClick={() => setEditing(null)}
                          style={{ fontFamily: mono, fontSize: 10.5, color: c.muted, background: "none", border: `1px solid ${c.border2}`, borderRadius: 6, padding: "2px 8px" }}
                        >
                          Cancel
                        </button>
                        <span style={{ fontSize: 9.5, color: c.muted2 }}>↵ save · esc cancel</span>
                      </span>
                    </span>
                  )
                  : (
                    <span
                      onClick={() => setEditing({ name, text: transport ?? "" })}
                      title={remote ? "Click to edit the URL" : "Click to edit the command"}
                      style={{ fontFamily: mono, fontSize: 10, color: c.muted2, wordBreak: "break-all", cursor: "text" }}
                    >
                      {transport} <span style={{ color: c.muted2, opacity: .7 }}>✎</span>
                    </span>
                  )}
                {remote && (
                  <span style={{ display: "flex", alignItems: "center", gap: 6 }}>
                    <span style={{ fontFamily: mono, fontSize: 10, color: authorized ? c.green : c.amber }}>
                      {authorized ? "authorized" : "needs OAuth"}
                    </span>
                    {!authorized && (
                      <button
                        onClick={() => authorize(name)}
                        disabled={busy}
                        title="Start the OAuth flow — approve in the browser tab, then Refresh"
                        style={{ fontFamily: mono, fontSize: 10.5, color: c.green, background: "none", border: `1px solid ${c.border2}`, borderRadius: 6, padding: "2px 8px" }}
                      >
                        Authorize
                      </button>
                    )}
                    {authUrls[name] && !authorized && (
                      <a href={authUrls[name]} target="_blank" rel="noreferrer" style={{ fontSize: 10.5, color: c.blue }}>
                        approval link
                      </a>
                    )}
                  </span>
                )}
                <span style={{ display: "flex", alignItems: "center", gap: 6 }}>
                  <button
                    onClick={() => test(name)}
                    disabled={busy || !sessionId || (remote && !authorized)}
                    title="Spawn/connect the server for this branch NOW and list its tools"
                    style={{ fontFamily: mono, fontSize: 10.5, color: c.green, background: "none", border: `1px solid ${c.border2}`, borderRadius: 6, padding: "2px 8px", flex: "none" }}
                  >
                    ▸ Test
                  </button>
                  {proof && (proof.connected
                    ? (
                      <span style={{ fontSize: 10.5, color: c.muted, wordBreak: "break-word" }}>
                        <span style={{ color: c.green, fontFamily: mono }}>runs</span>
                        {" · "}
                        {(proof.tools ?? []).map((t) => t.name).join(", ")}
                      </span>
                    )
                    : (
                      <span style={{ fontFamily: mono, fontSize: 10.5, color: c.red, wordBreak: "break-all" }}>
                        {proof.error || "failed"}
                      </span>
                    ))}
                </span>
                {conn?.stderrTail && (
                  <pre style={{ fontFamily: mono, fontSize: 9.5, color: c.muted2, margin: 0, whiteSpace: "pre-wrap", wordBreak: "break-all", maxHeight: 96, overflowY: "auto" }}>
                    {conn.stderrTail}
                  </pre>
                )}
              </>
            )}
          </div>
        );
      })}
      {status && servers.length === 0 && (
        <div style={{ fontSize: 11.5, color: c.muted2 }}>
          No servers registered yet.
        </div>
      )}
      <div style={{ fontSize: 11, color: c.muted2 }}>
        Register or debug one by typing <span style={{ fontFamily: mono, color: c.green }}>/mcp</span>{" "}
        in the composer — the agent writes the entry, enables it, and proves it runs.
      </div>
      {!sessionId && (
        <div style={{ fontSize: 11, color: c.muted2 }}>
          Open a session to enable or test servers for a branch.
        </div>
      )}
      {err && <span style={{ fontSize: 10.5, color: c.red, wordBreak: "break-all" }}>{err}</span>}
    </div>
  );
}

// bough runs the egress firewall in-process: this panel shows its status, the live
// feed, the hold-and-ask cards, and the editable rule set.
function NetworkPanel(
  { status, net, pending, pendingCount = 0, onResolve, policy, policySource, onSavePolicy, onOverridePolicy, onClearPolicyOverride, onPolicyChanged, sessionOpen, sessionId, wide }: {
    status: NetStatus;
    net: NetRequest[];
    pending: NetRequest | null;
    pendingCount?: number;
    onResolve: (approve: boolean, scope?: "once" | "session") => void;
    policy: NetConfig | null;
    policySource: PolicySource | null;
    onSavePolicy: (cfg: NetConfig) => void;
    onOverridePolicy: () => void;
    onClearPolicyOverride: () => void;
    onPolicyChanged: () => void;
    sessionOpen: boolean;
    sessionId: string | null;
    wide: boolean;
  },
) {
  const [editing, setEditing] = useState(false);
  // An AI draft awaiting review; seeds the editor until saved or discarded.
  const [draft, setDraft] = useState<{ config: NetConfig; rationale: string; n: number } | null>(null);
  // Feed rows picked for grouping into rules.
  const [sel, setSel] = useState<Set<string>>(new Set());
  const [grouping, setGrouping] = useState(false);
  const [groupErr, setGroupErr] = useState<string | null>(null);
  const groups = groupFeed(net);
  // A group is "selected" when all its member ids are; toggling flips the whole group.
  const toggleGroup = (g: FeedGroup) =>
    setSel((prev) => {
      const next = new Set(prev);
      const all = g.ids.every((id) => next.has(id));
      for (const id of g.ids) {
        if (all) next.delete(id);
        else next.add(id);
      }
      return next;
    });
  // Synthesize a classifier plugin from the selected requests, install + enable it
  // for this branch. It appears in the Plugins panel below, ready to edit (✎ Edit).
  const [pluginReload, setPluginReload] = useState(0);
  const [groupMsg, setGroupMsg] = useState<string | null>(null);
  const group = async () => {
    if (sel.size === 0 || grouping) return;
    setGrouping(true);
    setGroupErr(null);
    setGroupMsg(null);
    try {
      const { name } = await api.pluginFromRequests([...sel], sessionId);
      setSel(new Set());
      setPluginReload((n) => n + 1); // refresh the Plugins panel's library
      onPolicyChanged(); // pick up the new activation
      setGroupMsg(`Created & enabled plugin “${name}” — edit it below.`);
    } catch (e) {
      setGroupErr((e as Error).message || "could not create plugin");
    } finally {
      setGrouping(false);
    }
  };
  const dotColor = status.running ? c.green : status.enabled ? c.amber : c.muted2;
  const statusLabel = status.running ? "proxy up" : status.enabled ? "starting" : "off";
  // YOLO: this branch's enforcement is off — everything flows, the feed logs shadow
  // verdicts. Session-scoped: the toggle writes/updates the branch's policy override.
  const yolo = policy?.mode === "yolo";
  const [yoloBusy, setYoloBusy] = useState(false);
  const toggleYolo = async () => {
    if (!sessionId || yoloBusy) return;
    setYoloBusy(true);
    try {
      await api.setYolo(sessionId, !yolo);
      onPolicyChanged(); // refetch the effective config; the button re-derives from it
    } finally {
      setYoloBusy(false);
    }
  };
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
        {sessionOpen && sessionId && policy && (
          <button
            onClick={toggleYolo}
            disabled={yoloBusy}
            title={yolo
              ? "YOLO is ON for this branch — nothing is gated, the feed logs what would have been held or denied. Click to restore gating."
              : "Run this branch ungated: log every request, limit nothing. Toggle off to restore."}
            style={{
              fontFamily: mono,
              fontSize: 11,
              fontWeight: 700,
              letterSpacing: ".06em",
              color: yolo ? c.bg : c.red,
              background: yolo ? c.red : "none",
              border: `1px solid ${c.red}`,
              borderRadius: 6,
              padding: "3px 9px",
              opacity: yoloBusy ? 0.6 : 1,
              cursor: "pointer",
            }}
          >
            YOLO
          </button>
        )}
      </div>

      {yolo && (
        <div style={{ display: "flex", alignItems: "center", gap: 7, border: `1px solid ${c.red}`, borderRadius: 8, padding: "7px 10px", background: alpha(c.red, 7) }}>
          <Dot color={c.red} pulse />
          <span style={{ fontSize: 12, color: c.red, lineHeight: 1.45 }}>
            YOLO — this branch is ungated. Every request flows; rows below show what the rules would have held or denied.
          </span>
        </div>
      )}

      {!status.enabled && (
        <p style={{ fontSize: 12.5, color: c.muted, lineHeight: 1.55, margin: 0 }}>
          Egress gating is off. Start bough with <code style={{ fontFamily: mono, color: c.text2 }}>BOUGH_CLAWPATROL=1</code> to route sandbox traffic through the in-process proxy.
        </p>
      )}

      {status.running && status.caTrusted === false && status.caTrustCommand && (
        <CaTrustHint command={status.caTrustCommand} />
      )}

      {pending && (
        <HoldCard
          req={pending}
          queued={Math.max(0, pendingCount - 1)}
          onResolve={onResolve}
          onRefine={() => setEditing(true)}
        />
      )}

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
            <SuggestBox
              sessionId={sessionId}
              onDraft={(d) => setDraft((prev) => ({ ...d, n: (prev?.n ?? 0) + 1 }))}
            />
            {draft && (
              <div style={{ border: `1px solid ${c.border2}`, borderRadius: 6, padding: "8px 10px", display: "flex", flexDirection: "column", gap: 5 }}>
                <span style={{ fontFamily: mono, fontSize: 10.5, color: c.green }}>
                  AI draft — review below, then Save rules to apply
                </span>
                <p style={{ fontSize: 12, color: c.muted, lineHeight: 1.5, margin: 0 }}>{draft.rationale}</p>
                <button
                  onClick={() => setDraft(null)}
                  style={{ alignSelf: "flex-start", fontFamily: mono, fontSize: 10.5, color: c.muted2, background: "none", border: "none", padding: 0, textDecoration: "underline" }}
                >
                  discard draft
                </button>
              </div>
            )}
            <RuleEditor
              // Re-seed the editor's local state when the scope flips or a new AI
              // draft arrives (override/clear/draft all change the key).
              key={`${policySource?.scope ?? "global"}:${policySource?.sessionId ?? ""}:d${draft?.n ?? 0}`}
              policy={draft?.config ?? policy}
              onSave={(cfg) => {
                setDraft(null);
                onSavePolicy(cfg);
              }}
            />
          </div>
        )
        : <p style={{ fontSize: 12, color: c.muted2, margin: 0 }}>Loading rules…</p>)}

      <div style={{ display: "flex", flexDirection: "column", minHeight: 0 }}>
        <span style={{ fontFamily: mono, fontSize: 10, letterSpacing: ".1em", color: c.muted2, margin: "0 0 4px 2px" }}>
          FEED {net.length > 0 && <Chip>{groups.length}{groups.length !== net.length ? ` / ${net.length}` : ""}</Chip>}
        </span>
        {sel.size > 0 && (
          <div style={{ display: "flex", alignItems: "center", gap: 8, padding: "5px 2px 7px" }}>
            <span style={{ fontFamily: mono, fontSize: 10.5, color: c.green }}>{sel.size} selected</span>
            <button
              onClick={group}
              disabled={grouping}
              title="Build a classifier plugin from the selected requests, install + enable it for this branch"
              style={{ fontFamily: mono, fontSize: 10.5, color: grouping ? c.muted2 : c.green, background: "none", border: `1px solid ${c.border2}`, borderRadius: 6, padding: "2px 8px" }}
            >
              {grouping ? "creating…" : "✨ Group into plugin"}
            </button>
            <button
              onClick={() => setSel(new Set())}
              style={{ fontFamily: mono, fontSize: 10.5, color: c.muted2, background: "none", border: "none", padding: 0, textDecoration: "underline" }}
            >
              clear
            </button>
            {groupErr && <span style={{ fontSize: 10.5, color: c.red }}>{groupErr}</span>}
          </div>
        )}
        {groupMsg && (
          <span style={{ fontFamily: mono, fontSize: 10.5, color: c.green, padding: "0 2px 6px" }}>
            {groupMsg}
          </span>
        )}
        {net.length === 0
          ? <div style={{ fontSize: 12, color: c.muted2, padding: "8px 2px" }}>No egress yet. Gated requests from sandbox commands appear here.</div>
          : (
            // Its own scroll so a long feed doesn't push the rule editor / plugins
            // off-screen — identical requests fold into one row (× count).
            <div style={{ maxHeight: wide ? 460 : 320, overflowY: "auto" }}>
              {groups.map((g) => (
                <FeedGroupRow
                  key={g.key}
                  g={g}
                  selected={g.ids.every((id) => sel.has(id))}
                  onSelect={() => toggleGroup(g)}
                />
              ))}
            </div>
          )}
      </div>

      {status.enabled && (
        <div style={{ borderTop: `1px solid ${c.border}`, paddingTop: 12 }}>
          <PluginsPanel sessionId={sessionId} activations={policy?.plugins ?? []} onPolicyChanged={onPolicyChanged} reloadSignal={pluginReload} />
        </div>
      )}
    </div>
  );
}

function fileTint(s: DiffFile["status"]) {
  return s === "A" ? c.green : s === "D" ? c.red : c.amber;
}

const SOURCE_LABEL: Record<NonNullable<DiffFile["source"]>, string> = {
  jj: "REPO · jj",
  shadow: "REPO",
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
  onAdopt,
}: {
  diffs: DiffFile[];
  selected: string | null;
  onSelect: (path: string) => void;
  onApplyAll: () => void;
  onRevert: () => void;
  // Present only when the open session is a subagent branch: squash its changes
  // into the spawner's workspace.
  onAdopt?: () => void;
}) {
  const totAdd = diffs.reduce((a, f) => a + f.added, 0);
  const totRem = diffs.reduce((a, f) => a + f.removed, 0);

  if (diffs.length === 0) {
    return (
      <div style={{ flex: 1, overflowY: "auto", padding: "20px 14px", minHeight: 0 }}>
        <div style={{ fontSize: 12.5, color: c.muted2, lineHeight: 1.55 }}>
          No changes staged. When a turn edits files in the session workspace, they land here
          for review — nothing is written back until you apply.
          {onAdopt && (
            <>
              {" "}This is a subagent branch: when it has changes, adopt them into its
              parent from here.
            </>
          )}
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
  const hasRepo = diffs.some((f) => f.source === "jj" || f.source === "shadow");

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
          {onAdopt && (
            <button
              onClick={onAdopt}
              title="Squash this subagent branch's changes into its parent session's workspace"
              style={{ fontSize: 11, color: c.green, fontWeight: 600, padding: "4px 10px", borderRadius: 6, border: `1px solid ${c.green}` }}
            >
              ◆ Adopt into parent
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
  pendingCount,
  onResolve,
  policy,
  policySource,
  onSavePolicy,
  onOverridePolicy,
  onClearPolicyOverride,
  onPolicyChanged,
  sessionOpen = false,
  sessionId = null,
  diffs,
  selectedFile,
  onSelectFile,
  onApplyAll,
  onRevert,
  onAdopt,
}: {
  tab: RailTab;
  onTab: (t: RailTab) => void;
  wide?: boolean;
  onToggleWide?: () => void;
  netStatus: NetStatus;
  net: NetRequest[];
  pending: NetRequest | null;
  /** Total holds waiting (shown one at a time); default derives from `pending`. */
  pendingCount?: number;
  onResolve: (approve: boolean, scope?: "once" | "session") => void;
  policy: NetConfig | null;
  policySource?: PolicySource | null;
  onSavePolicy: (cfg: NetConfig) => void;
  onOverridePolicy?: () => void;
  onClearPolicyOverride?: () => void;
  /** Called after a plugin enable/disable changed the rules server-side. */
  onPolicyChanged?: () => void;
  /** True when a session is open — enables the branch-scope controls. */
  sessionOpen?: boolean;
  /** The open session, for branch-scoped AI rule drafting. */
  sessionId?: string | null;
  diffs: DiffFile[];
  selectedFile: string | null;
  onSelectFile: (path: string) => void;
  onApplyAll: () => void;
  onRevert: () => void;
  // Present only for subagent branches (see ChangesPanel).
  onAdopt?: () => void;
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
        pendingCount={pendingCount ?? (pending ? 1 : 0)}
        padY={wide ? 2 : 0}
        wide={wide}
        onToggleWide={onToggleWide}
      />
      {tab === "network" ? (
        <NetworkPanel
          status={netStatus}
          net={net}
          pending={pending}
          pendingCount={pendingCount ?? (pending ? 1 : 0)}
          onResolve={onResolve}
          policy={policy}
          policySource={policySource ?? null}
          onSavePolicy={onSavePolicy}
          onOverridePolicy={onOverridePolicy ?? (() => {})}
          onClearPolicyOverride={onClearPolicyOverride ?? (() => {})}
          onPolicyChanged={onPolicyChanged ?? (() => {})}
          sessionOpen={sessionOpen}
          sessionId={sessionId}
          wide={wide}
        />
      ) : tab === "mcp" ? (
        <McpPanel sessionId={sessionId} />
      ) : (
        <ChangesPanel diffs={diffs} selected={selectedFile} onSelect={onSelectFile} onApplyAll={onApplyAll} onRevert={onRevert} onAdopt={onAdopt} />
      )}
    </div>
  );
}
