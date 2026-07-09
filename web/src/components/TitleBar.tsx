// macOS-style titlebar: the bough mark, the active branch chip, and a
// right-aligned run status. In live mode the status derives from real state (SSE
// connection); the sandbox-snapshot / agent chips are hidden until their backends exist,
// rather than showing invented values. Mock mode keeps the fuller design-review chrome.
import { useEffect, useRef, useState } from "react";
import { c, mono } from "../theme";
import type { ModelOption } from "../api";
import { CopyId, Dot, Logo } from "./ui";

// "12.3k" / "512" — compact token counts for the context meter.
function fmtTokens(n: number): string {
  if (n >= 1000) return `${(n / 1000).toFixed(n >= 10_000 ? 0 : 1)}k`;
  return String(n);
}

// Model name + click-to-switch menu. Falls back to a static label when no models/handler.
// Reused for the worker picker (any {id,label} list) via symbol/switchTitle.
function ModelPicker({ model, models, onSetModel, symbol = "◇", switchTitle = "Switch model" }: {
  model: string;
  models?: { id: string; label: string }[];
  onSetModel?: (m: string) => void;
  symbol?: string;
  switchTitle?: string;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, [open]);

  const short = model.replace(/^claude-/, "");
  const label = models?.find((m) => m.id === model)?.label ?? short;
  const clickable = !!(models?.length && onSetModel);

  return (
    <div ref={ref} style={{ position: "relative" }}>
      <button
        onClick={clickable ? () => setOpen((v) => !v) : undefined}
        title={clickable ? switchTitle : model}
        style={{
          display: "inline-flex",
          alignItems: "center",
          gap: 6,
          fontFamily: mono,
          fontSize: 11.5,
          color: c.muted,
          cursor: clickable ? "pointer" : "default",
        }}
      >
        <span style={{ color: c.green }}>{symbol}</span> {label}
        {clickable && <span style={{ color: c.muted2, fontSize: 9 }}>▾</span>}
      </button>
      {open && models && (
        <div
          style={{
            position: "absolute",
            top: "100%",
            right: 0,
            marginTop: 6,
            minWidth: 220,
            background: c.panel2,
            border: `1px solid ${c.border}`,
            borderRadius: 9,
            boxShadow: "0 16px 40px rgba(0,0,0,.4)",
            padding: 5,
            zIndex: 30,
          }}
        >
          {models.map((m) => (
            <button
              key={m.id}
              onClick={() => {
                onSetModel?.(m.id);
                setOpen(false);
              }}
              style={{
                display: "flex",
                alignItems: "center",
                justifyContent: "space-between",
                gap: 12,
                width: "100%",
                textAlign: "left",
                padding: "7px 10px",
                borderRadius: 6,
                fontSize: 12,
                color: m.id === model ? c.text : c.muted,
                background: m.id === model ? c.panelInset : "transparent",
              }}
            >
              <span>{m.label}</span>
              {m.id === model && <span style={{ color: c.green }}>✓</span>}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

// A ~200k context budget for the meter; the exact window varies by model but this
// is a stable yardstick for "how full is the conversation".
const CONTEXT_BUDGET = 200_000;

export function TitleBar({
  branch = "main · migrate-auth",
  right,
  live = false,
  connected = false,
  model,
  models,
  usage,
  onSetModel,
  worker,
  workerOptions,
  onSetWorker,
  workspace,
  sessionId,
  subagentsRunning = 0,
  onShowMap,
}: {
  branch?: string;
  right?: React.ReactNode;
  live?: boolean;
  connected?: boolean;
  // Live-mode glanceables: the model turns run on, and the repo this session edits.
  model?: string;
  models?: ModelOption[];
  usage?: {
    contextTokens: number;
    outputTokens: number;
    inputTokens?: number;
    tree?: { inputTokens: number; outputTokens: number; sessions: number };
  };
  onSetModel?: (model: string) => void;
  // The worker micro-tasks run on ("local" or a model id) — global, not per session.
  worker?: string;
  workerOptions?: { id: string; label: string }[];
  onSetWorker?: (worker: string) => void;
  workspace?: string | null;
  // When set, a copy chip next to the branch chip copies this head's session id.
  sessionId?: string | null;
  // Background subagents with a turn in flight, across ALL sessions — the badge
  // keeps delegated work visible from anywhere. Click-through opens the map.
  subagentsRunning?: number;
  onShowMap?: () => void;
}) {
  const repo = workspace ? workspace.replace(/\/+$/, "").split("/").pop() : null;
  const ctxPct = usage ? Math.min(100, Math.round((usage.contextTokens / CONTEXT_BUDGET) * 100)) : 0;
  const ctxColor = ctxPct >= 85 ? c.red : ctxPct >= 60 ? c.amber : c.green;
  // Estimated spend for this session PLUS its subagent subtree, at the active
  // model's rates. An estimate: sessions can switch models mid-flight and cache
  // reads are billed cheaper — this is the honest order of magnitude, not a bill.
  const pricing = models?.find((m) => m.id === model)?.pricing;
  const tree = usage?.tree ?? (usage
    ? { inputTokens: usage.inputTokens ?? 0, outputTokens: usage.outputTokens, sessions: 0 }
    : undefined);
  const cost = pricing && tree
    ? (tree.inputTokens * pricing.in + tree.outputTokens * pricing.out) / 1_000_000
    : undefined;
  // In live mode, surface the model, the context meter, and the event-stream link.
  const liveRight = (
    <div style={{ display: "flex", alignItems: "center", gap: 16, fontFamily: mono, fontSize: 11.5, color: c.muted2 }}>
      {subagentsRunning > 0 && (
        <button
          onClick={onShowMap}
          title={`${subagentsRunning} subagent${subagentsRunning === 1 ? "" : "s"} running — open the map`}
          style={{
            display: "inline-flex",
            alignItems: "center",
            gap: 6,
            padding: "2px 9px",
            borderRadius: 6,
            border: `1px solid ${c.border2}`,
            color: c.amber,
            fontFamily: mono,
            fontSize: 11,
          }}
        >
          <span
            style={{
              width: 7,
              height: 7,
              borderRadius: "50%",
              background: c.amber,
              animation: "boughPulse 1s steps(2) infinite",
            }}
          />
          ◆ {subagentsRunning} running
        </button>
      )}
      {usage && usage.contextTokens > 0 && (
        <span
          title={`context ${usage.contextTokens.toLocaleString()} tokens · ${usage.outputTokens.toLocaleString()} output this session`}
          style={{ display: "inline-flex", alignItems: "center", gap: 7, color: c.muted }}
        >
          <span style={{ width: 44, height: 5, borderRadius: 3, background: c.panelInset, overflow: "hidden", display: "inline-block" }}>
            <span style={{ display: "block", height: "100%", width: `${ctxPct}%`, background: ctxColor }} />
          </span>
          {fmtTokens(usage.contextTokens)}
        </span>
      )}
      {cost !== undefined && cost > 0 && tree && (
        <span
          title={[
            `~$${cost.toFixed(2)} estimated at ${model} rates`,
            `${tree.inputTokens.toLocaleString()} in · ${tree.outputTokens.toLocaleString()} out`,
            tree.sessions > 0 ? `across this session + ${tree.sessions} subagent branch${tree.sessions === 1 ? "" : "es"}` : "this session",
          ].join("\n")}
          style={{ color: c.muted }}
        >
          ~${cost < 0.01 ? "0.01" : cost.toFixed(2)}
          {tree.sessions > 0 && <span style={{ color: c.muted2 }}> ·◆{tree.sessions}</span>}
        </span>
      )}
      {model && <ModelPicker model={model} models={models} onSetModel={onSetModel} />}
      {worker && (
        <ModelPicker
          model={worker}
          models={workerOptions}
          onSetModel={onSetWorker}
          symbol="⚒"
          switchTitle="Switch worker (delegated fixes, digests, annotations, titles)"
        />
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
      <div style={{ display: "flex", alignItems: "center" }}>
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
