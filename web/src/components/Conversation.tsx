// Center pane: the current head read top-to-bottom, plus the composer. User and
// supervisor turns render as prose; worker sub-agents and tool calls fold into quiet
// collapsed groups. The live turn streams in from the delta buffer.
import { useEffect, useRef, useState } from "react";
import { applyTheme, c, alpha, mono, sans, THEME_PRESETS, type ThemePreset } from "../theme";
import { useIsMobile } from "../useIsMobile";
import type { Message, Part, Session } from "../types";
import { api, type TurnPick } from "../api";
import type { ActivityGroup, WorkerActivity } from "../mock";
import { CopyId, Kbd, TumblingLogo } from "./ui";
import { Markdown } from "./Markdown";

const roleLabel: Record<string, { text: string; color: string }> = {
  user: { text: "YOU", color: c.muted2 },
  supervisor: { text: "BOUGH · supervisor", color: c.green },
  worker: { text: "◇ worker", color: c.muted },
  system: { text: "◆ SYSTEM", color: c.muted },
};

function clip(s: string, max: number): string {
  return s.length > max ? s.slice(0, max) + `\n… (${s.length - max} more chars)` : s;
}

// When the client first saw a tool call become the one executing (parts carry no
// timestamps, and tools in a round run serially — a call only "starts" once every
// call before it has its result). Client-side only: a reload mid-command restarts
// the clock from zero.
const callStarted = new Map<string, number>();
function startFor(callId: string): number {
  let t = callStarted.get(callId);
  if (t === undefined) {
    t = Date.now();
    callStarted.set(callId, t);
  }
  return t;
}

function fmtElapsed(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, "0")}s`;
}

function ToolGroup(
  { parts, pending, onBranch }: {
    parts: Part[];
    // The enclosing turn is still running — the first call without a result is the
    // one executing right now and carries the live elapsed clock.
    pending?: boolean;
    // Branch from a specific call: receives the position (within `parts`) of the
    // call's result — history keeps everything up to and including it.
    onBranch?: (partPos: number) => void;
  },
) {
  const [open, setOpen] = useState(false);
  const calls = parts.filter((p) => p.type === "tool_call") as Extract<Part, { type: "tool_call" }>[];
  const results = new Map(
    (parts.filter((p) => p.type === "tool_result") as Extract<Part, { type: "tool_result" }>[]).map(
      (r) => [r.callId, r]
    )
  );
  const running = pending ? calls.find((call) => !results.has(call.id)) : undefined;
  for (const id of results.keys()) callStarted.delete(id);
  // Quick calls shouldn't flash a clock — it only appears once a call has been
  // running for 3s (the tick below re-checks every second until it crosses).
  const runningMs = running ? Date.now() - startFor(running.id) : 0;
  const showClock = running !== undefined && runningMs >= 3000;
  // 1s tick while a call runs, so the elapsed label counts up live.
  const [, tick] = useState(0);
  useEffect(() => {
    if (!running) return;
    const t = window.setInterval(() => tick((n) => n + 1), 1000);
    return () => window.clearInterval(t);
  }, [running?.id]);
  if (calls.length === 0) return null;
  // Harness verdict (SPEC §5 check gating): surface it on the COLLAPSED header —
  // "did it actually pass?" must not require expanding the fold.
  const outputs = [...results.values()].map((r) =>
    typeof r.output === "string" ? r.output : JSON.stringify(r.output)
  );
  const verdict = outputs.some((o) => o.includes("[done] accepted"))
    ? { text: "✓ check passed", color: c.green }
    : outputs.some((o) => o.includes("[done] rejected"))
      ? { text: "✗ check failed", color: c.amber }
      : null;
  return (
    <div
      style={{
        maxWidth: 560,
        border: `1px solid ${c.border2}`,
        borderRadius: 9,
        overflow: "hidden",
        fontFamily: mono,
        fontSize: 11.5,
      }}
    >
      <button
        onClick={() => setOpen((o) => !o)}
        style={{
          display: "flex",
          alignItems: "center",
          gap: 9,
          padding: "8px 13px",
          width: "100%",
          textAlign: "left",
          color: c.muted2,
        }}
      >
        <span>{open ? "▾" : "▸"}</span>
        {calls.length} tool {calls.length === 1 ? "call" : "calls"}
        {verdict && <span style={{ color: verdict.color, fontWeight: 600 }}>{verdict.text}</span>}
        {showClock && (
          <span style={{ color: c.amber, display: "inline-flex", alignItems: "center", gap: 6 }}>
            <span
              className="pulse-amber"
              style={{ width: 7, height: 7, borderRadius: "50%", background: c.amber, flex: "none" }}
            />
            {fmtElapsed(runningMs)}
          </span>
        )}
        <span style={{ color: c.muted2, marginLeft: "auto", fontWeight: 400 }}>
          {calls.map((call) => call.name).join(" · ")}
        </span>
      </button>
      {open &&
        calls.map((call) => {
          const res = results.get(call.id);
          const output = res === undefined
            ? null
            : typeof res.output === "string"
              ? res.output
              : JSON.stringify(res.output, null, 2);
          // A code-mode call (run_steps) carries the program in `code`: render it as
          // real multi-line code, with the remaining fields (check/done) as a quiet
          // meta line — never as an escaped JSON blob.
          const input = (call.input ?? null) as Record<string, unknown> | null;
          const code = input && typeof input.code === "string" ? input.code : null;
          const meta = code && input
            ? Object.entries(input)
              .filter(([k]) => k !== "code")
              .map(([k, v]) => `${k}: ${typeof v === "string" ? v : JSON.stringify(v)}`)
            : [];
          return (
            <div key={call.id} style={{ borderTop: `1px solid ${c.border3}`, padding: "8px 13px" }}>
              <div style={{ color: c.muted, display: "flex", alignItems: "center", gap: 8 }}>
                <span style={{ color: c.green }}>◇</span> {call.name}
                {res && (
                  <span style={{ color: res.isError ? c.red : c.green }}>
                    {res.isError ? "✗ error" : "✓ done"}
                  </span>
                )}
                {onBranch && (
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      const callPos = parts.indexOf(call);
                      const resPos = parts.findIndex(
                        (p) => p.type === "tool_result" && p.callId === call.id,
                      );
                      onBranch(resPos >= 0 ? resPos : callPos);
                    }}
                    title="Branch here: keep history up to this call's result, then explain what to do differently"
                    style={{
                      marginLeft: "auto",
                      fontSize: 10.5,
                      color: c.muted2,
                      border: `1px solid ${c.border3}`,
                      borderRadius: 5,
                      padding: "0 6px",
                    }}
                  >
                    ⑂ branch
                  </button>
                )}
              </div>
              {meta.length > 0 && (
                <div style={{ margin: "5px 0 0", color: c.muted2, fontSize: 10.5 }}>
                  {meta.join("   ·   ")}
                </div>
              )}
              <pre
                style={{
                  margin: "6px 0 0",
                  padding: code ? "6px 8px" : 0,
                  background: code ? c.panelInset : undefined,
                  border: code ? `1px solid ${c.border3}` : undefined,
                  borderRadius: 6,
                  color: code ? c.text2 : c.muted2,
                  whiteSpace: "pre-wrap",
                  wordBreak: "break-word",
                  fontSize: 10.5,
                  lineHeight: 1.55,
                  maxHeight: 300,
                  overflowY: "auto",
                }}
              >
                {code ?? clip(JSON.stringify(call.input, null, 2), 1500)}
              </pre>
              {output !== null && output !== "" && (
                <>
                  <div style={{ margin: "6px 0 0", color: c.muted2, fontSize: 10 }}>output</div>
                  <pre
                    style={{
                      margin: "3px 0 0",
                      padding: "6px 8px",
                      background: c.panelInset,
                      border: `1px solid ${c.border3}`,
                      borderRadius: 6,
                      color: res!.isError ? c.red : c.muted,
                      whiteSpace: "pre-wrap",
                      wordBreak: "break-word",
                      fontSize: 10.5,
                      lineHeight: 1.55,
                      maxHeight: 220,
                      overflowY: "auto",
                    }}
                  >
                    {clip(output, 2000)}
                  </pre>
                </>
              )}
            </div>
          );
        })}
    </div>
  );
}

type Segment =
  | { kind: "text"; text: string; idxs: number[] }
  | { kind: "reasoning"; text: string; idxs: number[] }
  | { kind: "tools"; parts: Part[]; idxs: number[] };

// Group a turn's parts into renderable segments, preserving their order. Consecutive
// tool_call/tool_result parts fold into one collapsible ToolGroup between prose blocks.
// Each segment carries the part indexes it covers, so selection mode can address
// sections of a turn (e.g. its prose without the tool calls) when building picks.
function segmentParts(parts: Part[]): Segment[] {
  const segs: Segment[] = [];
  parts.forEach((p, i) => {
    if (p.type === "text") segs.push({ kind: "text", text: p.text, idxs: [i] });
    else if (p.type === "reasoning") segs.push({ kind: "reasoning", text: p.text, idxs: [i] });
    else {
      const last = segs[segs.length - 1];
      if (last?.kind === "tools") {
        last.parts.push(p);
        last.idxs.push(i);
      } else segs.push({ kind: "tools", parts: [p], idxs: [i] });
    }
  });
  return segs;
}

// A folded group of worker sub-agents under a supervisor turn. Collapsed by default; a
// running worker carries the amber pulse.
function WorkerGroup({ workers }: { workers: WorkerActivity[] }) {
  const [open, setOpen] = useState(false);
  const dotFor = (w: WorkerActivity) =>
    w.status === "running" ? (
      <span
        className="pulse-amber"
        style={{ width: 7, height: 7, borderRadius: "50%", background: c.amber, flex: "none" }}
      />
    ) : (
      <span style={{ color: w.status === "failed" ? c.red : c.green }}>◇</span>
    );
  return (
    <div
      style={{
        maxWidth: 560,
        border: `1px solid ${c.border2}`,
        borderRadius: 9,
        overflow: "hidden",
        marginBottom: 10,
      }}
    >
      <button
        onClick={() => setOpen((o) => !o)}
        style={{
          display: "flex",
          alignItems: "center",
          gap: 9,
          padding: "9px 13px",
          width: "100%",
          textAlign: "left",
          background: c.panel3,
          fontSize: 12,
          color: c.muted,
          borderBottom: open ? `1px solid ${c.border2}` : "none",
        }}
      >
        <span style={{ color: c.muted2 }}>{open ? "▾" : "▸"}</span> {workers.length} workers
      </button>
      {open &&
        workers.map((w, i) => (
          <div
            key={w.name}
            style={{
              display: "flex",
              alignItems: "center",
              gap: 10,
              padding: "9px 13px",
              fontFamily: mono,
              fontSize: 11.5,
              color: c.muted,
              borderBottom: i < workers.length - 1 ? `1px solid ${c.border3}` : "none",
            }}
          >
            {dotFor(w)}
            <span>worker · {w.name}</span>
            <span style={{ color: w.status === "failed" ? c.red : w.status === "done" ? c.green : c.amber }}>
              {w.status === "done" ? "✓ done" : w.status === "failed" ? "✗ failed" : "⋯ running"}
            </span>
            <span style={{ color: c.muted2 }}>· {w.meta}</span>
            <span style={{ marginLeft: "auto", color: c.muted2 }}>▸</span>
          </div>
        ))}
    </div>
  );
}

// The quiet "N commands · M network calls" summary chip, and the live running bar.
function ActivityView({ group }: { group: ActivityGroup }) {
  return (
    <div style={{ marginTop: 4 }}>
      {group.workers && <WorkerGroup workers={group.workers} />}
      {group.toolSummary && (
        <div
          style={{
            maxWidth: 560,
            display: "inline-flex",
            alignItems: "center",
            gap: 9,
            padding: "8px 13px",
            border: `1px solid ${c.border2}`,
            borderRadius: 9,
            fontFamily: mono,
            fontSize: 11.5,
            color: c.muted2,
          }}
        >
          <span>▸</span> {group.toolSummary}
        </div>
      )}
      {group.running && (
        <div
          style={{
            maxWidth: 560,
            display: "inline-flex",
            alignItems: "center",
            gap: 9,
            padding: "9px 13px",
            border: `1px solid ${c.border2}`,
            borderRadius: 9,
            fontFamily: mono,
            fontSize: 11.5,
            color: c.muted,
          }}
        >
          <span
            className="pulse-amber"
            style={{ width: 7, height: 7, borderRadius: "50%", background: c.amber, flex: "none" }}
          />
          {group.running.label}
        </div>
      )}
    </div>
  );
}

// Subagent branches spawned by this turn (matched on originMessageId): one card per
// branch, pulsing while it runs, click-through to open its session on the map/thread.
// `column` renders the side-rail variant (vertical stack beside the turn's prose);
// without it the cards flow inline under the turn (mobile fallback).
function SubagentChips({ subs, onOpen, column = false }: {
  subs: Session[];
  onOpen?: (id: string) => void;
  column?: boolean;
}) {
  const [expandedIds, setExpandedIds] = useState<Set<string>>(new Set());
  
  // Auto-collapse finished subagents (remove from expanded set when busy → false)
  useEffect(() => {
    setExpandedIds(prev => {
      const next = new Set(prev);
      subs.forEach(s => {
        if (!s.busy && next.has(s.id)) {
          next.delete(s.id);
        }
      });
      return next.size === prev.size ? prev : next;
    });
  }, [subs]);
  
  const visibleSubs = subs.filter(s => expandedIds.has(s.id) || s.busy);
  
  return (
    <div
      style={column
        ? { display: "flex", flexDirection: "column", gap: 7 }
        : { display: "flex", flexWrap: "wrap", gap: 8, marginTop: 10 }}
    >
      {column && (
        <div style={{ fontSize: 10, letterSpacing: ".12em", color: c.muted2, fontWeight: 600 }}>
          ◆ BRANCHES
        </div>
      )}
      {visibleSubs.map((s) => (
        <button
          key={s.id}
          onClick={() => onOpen?.(s.id)}
          title="open this subagent's branch"
          style={{
            display: "flex",
            alignItems: "center",
            gap: 7,
            padding: "5px 11px",
            borderRadius: 8,
            border: `1px solid ${c.border2}`,
            background: c.panel3,
            color: c.text2,
            fontSize: 12,
            fontFamily: mono,
            cursor: "pointer",
            ...(column ? { width: "100%", textAlign: "left" as const } : {}),
          }}
        >
          <span
            style={{
              width: 7,
              height: 7,
              borderRadius: "50%",
              flex: "none",
              background: s.busy ? c.amber : c.green,
              animation: s.busy ? "boughPulse 1s steps(2) infinite" : undefined,
            }}
          />
          <span
            style={column
              ? { overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }
              : undefined}
          >
            ◆ {s.title.replace(/^subagent · /, "")}
          </span>
        </button>
      ))}
    </div>
  );
}

function TurnView({
  msg,
  live,
  activity,
  subagents = [],
  onOpenSession,
  subagentThread = false,
  chipsAside = false,
  editable,
  onEdit,
  selecting,
  selectedParts,
  onPick,
  onPickParts,
  onBranchAt,
}: {
  msg: Message;
  live?: string;
  activity?: ActivityGroup;
  subagents?: Session[];
  onOpenSession?: (id: string) => void;
  subagentThread?: boolean;
  // Wide screens park the subagent cards in a sticky column beside the turn's prose;
  // on phones they fall back to flowing under it.
  chipsAside?: boolean;
  editable: boolean;
  onEdit: (id: string, text: string) => void;
  selecting: boolean;
  // Part indexes of this turn currently picked; undefined/empty = not picked.
  selectedParts?: Set<number>;
  onPick: (id: string, shift: boolean) => void;
  // Toggle one section (a segment's part indexes) in/out of the selection.
  onPickParts: (id: string, idxs: number[]) => void;
  // Branch from inside this turn: keep parts[0..partIdx], send `text` as the
  // correction on the new branch ("don't try it that way").
  onBranchAt?: (partIdx: number, text: string) => void;
}) {
  // Inside a subagent's own thread its replies are the subagent speaking, not the
  // supervisor — mislabeling was genuinely disorienting in user testing.
  const label = msg.role === "supervisor" && subagentThread
    ? { text: "◆ BOUGH · subagent", color: c.green }
    : roleLabel[msg.role] ?? roleLabel.worker;
  const isUser = msg.role === "user";
  // Parts render IN ORDER (text → tools → text …), not flattened by type — flattening
  // glues prose from different rounds together and buries tool activity at the bottom.
  const segments = segmentParts(msg.parts);
  const body = msg.parts
    .filter((p): p is Extract<Part, { type: "text" }> => p.type === "text")
    .map((t) => t.text)
    .join("\n");
  const showCursor = msg.pending && live !== undefined;

  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");

  // Branch-from-inside-the-turn: which segment the cut sits after (composer anchor)
  // and the part index history keeps up to. Set by a segment's ⑂ or a tool call's.
  const [branchAt, setBranchAt] = useState<{ seg: number; cut: number } | null>(null);
  const [branchDraft, setBranchDraft] = useState("");
  const branchable = !!onBranchAt && !isUser && !msg.pending && !selecting;

  function confirmBranch() {
    const t = branchDraft.trim();
    if (!t || !branchAt || !onBranchAt) return;
    onBranchAt(branchAt.cut, t);
    setBranchAt(null);
    setBranchDraft("");
  }

  // Edit-to-fork mode: replace the turn's text and re-send on a new branch.
  if (editing) {
    return (
      <div style={{ marginBottom: 24 }}>
        <div style={{ fontSize: 10.5, letterSpacing: ".14em", color: label.color, fontWeight: 600, marginBottom: 8 }}>
          {label.text} · editing → fork
        </div>
        <textarea
          autoFocus
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          style={{
            width: "100%",
            maxWidth: 640,
            minHeight: 60,
            resize: "vertical",
            background: c.panel3,
            border: `1px solid ${c.amber}`,
            borderRadius: 9,
            padding: "10px 12px",
            color: c.text,
            fontFamily: sans,
            fontSize: 14.5,
            lineHeight: 1.6,
            outline: "none",
          }}
        />
        <div style={{ display: "flex", gap: 8, marginTop: 8 }}>
          <button
            onClick={() => {
              const t = draft.trim();
              if (t) onEdit(msg.id, t);
              setEditing(false);
            }}
            style={{ fontSize: 12, fontWeight: 600, color: c.bg, background: c.green, borderRadius: 7, padding: "6px 12px" }}
          >
            Fork &amp; resend
          </button>
          <button
            onClick={() => setEditing(false)}
            style={{ fontSize: 12, color: c.muted, border: `1px solid ${c.border}`, borderRadius: 7, padding: "6px 12px" }}
          >
            Cancel
          </button>
        </div>
      </div>
    );
  }

  const selected = (selectedParts?.size ?? 0) > 0;
  // Some (not all) of the turn's parts picked — the border goes dashed as a cue.
  const partial = selected && selectedParts!.size < msg.parts.length;

  return (
    <div
      onClick={selecting ? (e) => onPick(msg.id, e.shiftKey) : undefined}
      style={{
        marginBottom: 24,
        position: "relative",
        cursor: selecting ? "pointer" : undefined,
        borderRadius: 9,
        padding: selecting ? "8px 10px" : undefined,
        margin: selecting ? "0 -10px 16px" : undefined,
        border: selecting
          ? `1px ${partial ? "dashed" : "solid"} ${selected ? c.amber : "transparent"}`
          : undefined,
        background: selected ? alpha(c.amber, 8) : undefined,
        // Shift-click extends the selection; without this the browser also selects text.
        userSelect: selecting ? "none" : undefined,
      }}
    >
      <div
        style={{
          fontSize: 10.5,
          letterSpacing: ".14em",
          color: label.color,
          fontWeight: 600,
          marginBottom: 8,
          display: "flex",
          alignItems: "center",
          gap: 10,
        }}
      >
        {label.text}
        {!selecting && (
          // session/turn address — each turn carries its HOME session id, so on a fork
          // this points at the ancestor head an inherited turn actually lives in.
          <CopyId value={`${msg.sessionId}/${msg.id}`} title="Copy session/turn id" />
        )}
        {editable && !selecting && (
          <button
            onClick={() => {
              setDraft(body);
              setEditing(true);
            }}
            className="turn-action"
            title="Edit this turn and re-send on a new branch"
            style={{ fontSize: 10.5, color: c.muted2, border: `1px solid ${c.border2}`, borderRadius: 5, padding: "1px 7px", letterSpacing: 0, fontWeight: 400 }}
          >
            ✎ edit → fork
          </button>
        )}
      </div>
      {isUser
        ? body && (
          <div
            style={{
              fontSize: 14.5,
              lineHeight: 1.6,
              color: c.text,
              maxWidth: 640,
              // User text is verbatim; agent text is markdown (below).
              whiteSpace: "pre-wrap",
            }}
          >
            {body}
          </div>
        )
        : (
          <div style={{ display: "flex", gap: 18, alignItems: "flex-start" }}>
          <div style={{ display: "flex", flexDirection: "column", gap: 10, maxWidth: 640, flex: "0 1 640px", minWidth: 0 }}>
            {segments.map((seg, i) => {
              const body = seg.kind === "reasoning"
                ? (
                  <div
                    style={{
                      fontSize: 13,
                      lineHeight: 1.55,
                      color: c.muted2,
                      fontStyle: "italic",
                      paddingLeft: 12,
                      borderLeft: `2px solid ${c.border2}`,
                    }}
                  >
                    {seg.text}
                  </div>
                )
                : seg.kind === "tools"
                ? (
                  <ToolGroup
                    parts={seg.parts}
                    pending={msg.pending}
                    // Per-call ⑂ inside the expanded group: cut after that call's result.
                    onBranch={branchable
                      ? (pos) => {
                        setBranchAt({ seg: i, cut: seg.idxs[pos] });
                        setBranchDraft("");
                      }
                      : undefined}
                  />
                )
                : (
                  <div style={{ fontSize: 14.5, lineHeight: 1.65, color: c.text2 }}>
                    <Markdown text={seg.text} />
                  </div>
                );
              if (!selecting) {
                return (
                  <div key={i} className="seg-wrap" style={{ position: "relative" }}>
                    {body}
                    {branchable && (
                      // Hover affordance: cut after this whole section.
                      <button
                        className="seg-branch"
                        onClick={() => {
                          setBranchAt({ seg: i, cut: seg.idxs[seg.idxs.length - 1] });
                          setBranchDraft("");
                        }}
                        title="Branch here: keep history up to this section, then explain what to do differently"
                        style={{
                          position: "absolute",
                          top: 0,
                          right: 0,
                          fontSize: 10.5,
                          color: c.muted2,
                          background: c.panel,
                          border: `1px solid ${c.border2}`,
                          borderRadius: 5,
                          padding: "1px 7px",
                        }}
                      >
                        ⑂ branch
                      </button>
                    )}
                    {branchAt?.seg === i && (
                      <div style={{ marginTop: 10, maxWidth: 640 }}>
                        <div
                          style={{
                            fontSize: 10.5,
                            letterSpacing: ".14em",
                            color: c.amber,
                            fontWeight: 600,
                            marginBottom: 6,
                          }}
                        >
                          ⑂ BRANCH HERE — history keeps everything above this point
                        </div>
                        <textarea
                          autoFocus
                          value={branchDraft}
                          onChange={(e) => setBranchDraft(e.target.value)}
                          onKeyDown={(e) => {
                            if (e.key === "Enter" && !e.shiftKey) {
                              e.preventDefault();
                              confirmBranch();
                            } else if (e.key === "Escape") setBranchAt(null);
                          }}
                          placeholder="Don't try it that way — explain what to do instead…"
                          style={{
                            width: "100%",
                            minHeight: 54,
                            resize: "vertical",
                            background: c.panel3,
                            border: `1px solid ${c.amber}`,
                            borderRadius: 9,
                            padding: "10px 12px",
                            color: c.text,
                            fontFamily: sans,
                            fontSize: 14,
                            lineHeight: 1.6,
                            outline: "none",
                          }}
                        />
                        <div style={{ display: "flex", gap: 8, marginTop: 6 }}>
                          <button
                            onClick={confirmBranch}
                            style={{
                              fontSize: 12,
                              fontWeight: 600,
                              color: c.bg,
                              background: c.green,
                              borderRadius: 7,
                              padding: "6px 12px",
                            }}
                          >
                            ⑂ Branch &amp; send
                          </button>
                          <button
                            onClick={() => setBranchAt(null)}
                            style={{
                              fontSize: 12,
                              color: c.muted,
                              border: `1px solid ${c.border}`,
                              borderRadius: 7,
                              padding: "6px 12px",
                            }}
                          >
                            Cancel
                          </button>
                        </div>
                      </div>
                    )}
                  </div>
                );
              }
              // Selection mode: each section is its own toggle, so a pick can be
              // "this turn minus its tool calls". Clicking a section of an unpicked
              // turn starts a partial pick; unpicked sections of a picked turn dim.
              const on = selectedParts !== undefined && seg.idxs.every((x) => selectedParts.has(x));
              return (
                <div
                  key={i}
                  onClick={(e) => {
                    e.stopPropagation();
                    onPickParts(msg.id, seg.idxs);
                  }}
                  title={on ? "Click to exclude this section" : "Click to include this section"}
                  style={{
                    cursor: "pointer",
                    borderRadius: 7,
                    padding: "4px 6px",
                    margin: "-4px -6px",
                    border: `1px dashed ${on ? c.amber : c.border2}`,
                    background: on ? alpha(c.amber, 10) : undefined,
                    opacity: selected && !on ? 0.45 : 1,
                  }}
                >
                  {body}
                </div>
              );
            })}
            {(!!live || showCursor) && (
              <div style={{ fontSize: 14.5, lineHeight: 1.65, color: c.text2 }}>
                {live && <Markdown text={live} streaming />}
                {showCursor && (
                  <span
                    style={{
                      display: "inline-block",
                      width: 7,
                      height: 15,
                      marginLeft: 2,
                      verticalAlign: "text-bottom",
                      background: c.green,
                      borderRadius: 1,
                      animation: "boughPulse 1s steps(2) infinite",
                    }}
                  />
                )}
              </div>
            )}
          </div>
          {chipsAside && subagents.length > 0 && (
            // Sticky within the conversation scroll: the cards ride alongside a long
            // streaming turn instead of sinking to its bottom edge.
            <div style={{ flex: "0 0 178px", minWidth: 0, position: "sticky", top: 6 }}>
              <SubagentChips column subs={subagents} onOpen={onOpenSession} />
            </div>
          )}
          </div>
        )}
      {!chipsAside && subagents.length > 0 && <SubagentChips subs={subagents} onOpen={onOpenSession} />}
      {activity && <ActivityView group={activity} />}
    </div>
  );
}

// Staged messages waiting for the current turn to finish. Each row is editable
// inline (click ✎) and removable (✕) before it's sent.
function QueuedList(
  { queued, onRemove, onEdit }: {
    queued: string[];
    onRemove?: (i: number) => void;
    onEdit?: (i: number, text: string) => void;
  },
) {
  const [editing, setEditing] = useState<number | null>(null);
  const [draft, setDraft] = useState("");
  return (
    <div
      style={{
        flex: "none",
        borderTop: `1px solid ${c.border}`,
        background: c.panel,
        padding: "10px 24px 0",
        display: "flex",
        flexDirection: "column",
        gap: 6,
      }}
    >
      <div style={{ fontSize: 10, letterSpacing: ".12em", color: c.amber, fontWeight: 600 }}>
        QUEUED · {queued.length} — sends when this turn finishes
      </div>
      {queued.map((text, i) => (
        <div
          key={i}
          style={{
            display: "flex",
            alignItems: "flex-start",
            gap: 8,
            background: c.panel3,
            border: `1px solid ${c.border2}`,
            borderRadius: 8,
            padding: "7px 10px",
          }}
        >
          {editing === i
            ? (
              <>
                <textarea
                  autoFocus
                  value={draft}
                  onChange={(e) => setDraft(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && !e.shiftKey) {
                      e.preventDefault();
                      const t = draft.trim();
                      if (t) onEdit?.(i, t);
                      setEditing(null);
                    } else if (e.key === "Escape") {
                      setEditing(null);
                    }
                  }}
                  rows={1}
                  style={{
                    flex: 1,
                    resize: "none",
                    border: "none",
                    outline: "none",
                    background: "transparent",
                    color: c.text,
                    fontFamily: sans,
                    fontSize: 13.5,
                    lineHeight: 1.5,
                  }}
                />
                <button
                  onClick={() => {
                    const t = draft.trim();
                    if (t) onEdit?.(i, t);
                    setEditing(null);
                  }}
                  style={{ fontSize: 11, color: c.green }}
                >
                  save
                </button>
              </>
            )
            : (
              <>
                <span style={{ flex: 1, fontSize: 13.5, color: c.text2, whiteSpace: "pre-wrap", lineHeight: 1.5 }}>
                  {text}
                </span>
                {onEdit && (
                  <button
                    onClick={() => {
                      setDraft(text);
                      setEditing(i);
                    }}
                    title="Edit this queued message"
                    style={{ fontSize: 12, color: c.muted2, flex: "none" }}
                  >
                    ✎
                  </button>
                )}
                {onRemove && (
                  <button
                    onClick={() => onRemove(i)}
                    title="Remove from queue"
                    style={{ fontSize: 12, color: c.muted2, flex: "none" }}
                  >
                    ✕
                  </button>
                )}
              </>
            )}
        </div>
      ))}
    </div>
  );
}

export function Conversation({
  thread,
  streaming,
  activity = [],
  subagents = [],
  onOpenSession,
  subagentThread = false,
  dimmed = false,
  canBranch = false,
  busy = false,
  focusKey,
  onSend,
  onInterrupt,
  onSearchFiles,
  onForkEdit,
  onBranchAt,
  onCompact,
  onExtract,
  onHandoff,
  draft: sessionDraft,
  sessionId,
  skills = [],
  queued = [],
  onRemoveQueued,
  onEditQueued,
  disabled,
}: {
  thread: Message[];
  streaming: Record<string, string>;
  // Changes when the open session changes; the composer refocuses so you can type
  // immediately after creating/switching a session.
  focusKey?: string | null;
  activity?: ActivityGroup[];
  // Subagent sessions (kind "subagent") for chip rendering under their spawning turn.
  subagents?: Session[];
  onOpenSession?: (id: string) => void;
  // The open session IS a subagent — its replies label as "◆ subagent", not supervisor.
  subagentThread?: boolean;
  dimmed?: boolean;
  // Live mode exposes fork/compact; mock leaves them off (the affordances are inert there).
  canBranch?: boolean;
  // A turn is streaming — the send button becomes a stop button and esc interrupts.
  busy?: boolean;
  onSend: (text: string, branch: boolean) => void;
  onInterrupt?: () => void;
  onForkEdit?: (messageId: string, text: string) => void;
  // Branch from inside a turn (⑂ on a section / tool call): keep parts[0..partIdx],
  // then send `text` as the correction on the new branch.
  onBranchAt?: (messageId: string, partIdx: number, text: string) => void;
  // Compact the picked turns/sections (session's OWN turns only) onto a summary branch.
  onCompact?: (picks: TurnPick[]) => void;
  // Copy the picked turns/sections (ancestors allowed) into a fresh conversation.
  onExtract?: (picks: TurnPick[]) => void;
  // Hand off to a fresh conversation focused on a goal: the server drafts its
  // opening prompt from this thread. Resolves when the new session is open.
  onHandoff?: (goal: string) => Promise<void>;
  // The open session's unsent handoff draft — prefills an empty composer.
  draft?: string | null;
  // The open session's id — marks which thread messages are its own (vs inherited),
  // gating the compact action on selections the server would reject.
  sessionId?: string | null;
  // Fuzzy workspace file search for @ references; absent → no autocomplete.
  onSearchFiles?: (q: string) => Promise<string[]>;
  // Installed skills for / references; absent/empty → no autocomplete.
  skills?: { name: string; description: string }[];
  // Messages staged while a turn runs — shown above the composer, editable/removable.
  queued?: string[];
  onRemoveQueued?: (i: number) => void;
  onEditQueued?: (i: number, text: string) => void;
  disabled: boolean;
}) {
  const activityFor = (id: string) => activity.find((a) => a.messageId === id);
  // Waiting on the model with nothing on screen for it — before the first token,
  // and between tool rounds. Shown at the bottom of the thread, not inside the
  // turn body; a running tool already shows its own elapsed row.
  const lastMsg = thread[thread.length - 1];
  const waiting = !!lastMsg?.pending && streaming[lastMsg.id] === undefined &&
    !lastMsg.parts.some((p) =>
      p.type === "tool_call" &&
      !lastMsg.parts.some((q) => q.type === "tool_result" && q.callId === p.id)
    );
  // Wide screens park subagent cards in a sticky side column; phones flow them inline.
  const chipsAside = !useIsMobile();
  const [text, setText] = useState("");
  const scrollRef = useRef<HTMLDivElement>(null);
  const taRef = useRef<HTMLTextAreaElement>(null);

  // New/switched session → put the caret in the composer.
  useEffect(() => {
    taRef.current?.focus();
  }, [focusKey]);

  // A handoff draft prefills the composer when its session opens (review, edit,
  // send); the server clears the draft on first post. Typed text deliberately
  // survives session switches (existing behavior), but an UNEDITED prefill must
  // not follow the user to another session — so on switch, text that still equals
  // the last prefill is replaced by the new session's draft (or cleared).
  const prefillRef = useRef<string | null>(null);
  useEffect(() => {
    if (text.trim() && text !== prefillRef.current) return; // user-typed — never clobber
    const next = sessionDraft ?? "";
    setText(next);
    prefillRef.current = next || null;
    // deliberately not depending on `text`: prefill happens on open/draft change only
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionDraft, sessionId]);

  // Large pastes become a "[Pasted content #N]" marker + chip instead of flooding
  // the textarea; markers expand back to the full text on send.
  const [pastes, setPastes] = useState<{ n: number; text: string }[]>([]);
  const pasteSeq = useRef(1);
  const pasteMarker = (n: number) => `[Pasted content #${n}]`;

  function onPaste(e: React.ClipboardEvent<HTMLTextAreaElement>) {
    const pasted = e.clipboardData.getData("text/plain");
    if (pasted.length <= 1000 && pasted.split("\n").length <= 10) return;
    e.preventDefault();
    const n = pasteSeq.current++;
    setPastes((p) => [...p, { n, text: pasted }]);
    const ta = e.currentTarget;
    const start = ta.selectionStart;
    const marker = pasteMarker(n);
    const next = text.slice(0, start) + marker + text.slice(ta.selectionEnd);
    setText(next);
    requestAnimationFrame(() => {
      ta.selectionStart = ta.selectionEnd = start + marker.length;
    });
  }

  function removePaste(n: number) {
    setPastes((p) => p.filter((x) => x.n !== n));
    setText((t) => t.split(pasteMarker(n)).join(""));
    taRef.current?.focus();
  }

  // @ file autocomplete: the trailing `@token` before the cursor drives a search.
  const [fileMatches, setFileMatches] = useState<string[]>([]);
  const [fileActive, setFileActive] = useState(0);
  const atMenuOpen = fileMatches.length > 0;

  // Parse a trailing `@token` (no spaces) at the cursor and fetch matches.
  function refreshAtMenu(value: string, caret: number) {
    if (!onSearchFiles) return;
    const upto = value.slice(0, caret);
    const m = /(^|\s)@([\w./-]*)$/.exec(upto);
    if (!m) {
      setFileMatches([]);
      return;
    }
    const q = m[2];
    onSearchFiles(q).then((files) => {
      setFileMatches(files);
      setFileActive(0);
    }).catch(() => setFileMatches([]));
  }

  function pickFile(path: string) {
    const ta = taRef.current;
    const caret = ta ? ta.selectionStart : text.length;
    const before = text.slice(0, caret).replace(/@([\w./-]*)$/, `@${path} `);
    const next = before + text.slice(caret);
    setText(next);
    setFileMatches([]);
    // Restore focus + caret after the inserted path.
    requestAnimationFrame(() => {
      if (ta) {
        ta.focus();
        ta.selectionStart = ta.selectionEnd = before.length;
      }
    });
  }

  // / skill autocomplete: same UX as @. The prefetched prop seeds the list; opening
  // the menu refetches /skills so a skill installed mid-session (by the human or by
  // a turn) autocompletes without a page reload or server restart.
  const [skillMatches, setSkillMatches] = useState<{ name: string; description: string }[]>([]);
  const [skillActive, setSkillActive] = useState(0);
  const [liveSkills, setLiveSkills] = useState(skills);
  useEffect(() => setLiveSkills(skills), [skills]);
  const slashMenuOpen = skillMatches.length > 0;

  // Parse a trailing `/token` (no spaces) at the cursor; null = no token (menu closed).
  function matchSkills(
    list: { name: string; description: string }[],
    value: string,
    caret: number,
  ): { name: string; description: string }[] | null {
    const m = /(^|\s)\/([\w-]*)$/.exec(value.slice(0, caret));
    if (!m) return null;
    const q = m[2].toLowerCase();
    return list.filter((s) => s.name.toLowerCase().includes(q));
  }

  function refreshSlashMenu(value: string, caret: number) {
    if (skills.length === 0) return;
    const matches = matchSkills(liveSkills, value, caret);
    if (!matches) {
      setSkillMatches([]);
      return;
    }
    if (!slashMenuOpen) {
      // The menu is opening — refresh the list behind it and re-filter at the
      // caret's current position once the fresh list lands.
      api.skills().then((fresh) => {
        setLiveSkills(fresh);
        const ta = taRef.current;
        if (!ta) return;
        const again = matchSkills(fresh, ta.value, ta.selectionStart ?? ta.value.length);
        if (again) {
          setSkillMatches(again);
          setSkillActive((a) => Math.min(a, Math.max(again.length - 1, 0)));
        }
      }).catch(() => {});
    }
    setSkillMatches(matches);
    setSkillActive(0);
  }

  function pickSkill(name: string) {
    const ta = taRef.current;
    const caret = ta ? ta.selectionStart : text.length;
    const before = text.slice(0, caret).replace(/\/([\w-]*)$/, `/${name} `);
    const next = before + text.slice(caret);
    setText(next);
    setSkillMatches([]);
    refreshThemeMenu(next, before.length); // "/theme " hands off to the preset picker
    requestAnimationFrame(() => {
      if (ta) {
        ta.focus();
        ta.selectionStart = ta.selectionEnd = before.length;
      }
    });
  }

  // /theme picker: once the trailing token is exactly "/theme " (the skill pick or a
  // typed space), the slash menu hands off to a preset picker. Selecting applies the
  // palette immediately (PUT /theme + live CSS-variable swap) — no model turn. Free
  // text after "/theme" closes the picker and goes to the model (the /theme skill).
  const [themeMenu, setThemeMenu] = useState<
    { active: number; current: string | null; defaults?: Record<string, string> } | null
  >(null);
  const themeEntries: (ThemePreset | { name: "Default"; colors: null })[] = [
    { name: "Default", colors: null },
    ...THEME_PRESETS,
  ];

  function refreshThemeMenu(value: string, caret: number) {
    const open = /(^|\s)\/theme\s+$/.test(value.slice(0, caret));
    if (!open) {
      setThemeMenu(null);
      return;
    }
    if (themeMenu) return; // already open — keep the active row
    setThemeMenu({ active: 0, current: null });
    // Mark the saved theme's row + get true default swatches (best-effort;
    // the picker works without either).
    api.theme()
      .then((r) =>
        setThemeMenu((m) => (m ? { ...m, current: r.theme?.name ?? null, defaults: r.defaults } : m))
      )
      .catch(() => {});
  }

  function pickTheme(entry: ThemePreset | { name: "Default"; colors: null }) {
    (entry.colors === null ? api.clearTheme() : api.setTheme(entry as ThemePreset)).catch(() => {});
    applyTheme(entry.colors);
    const ta = taRef.current;
    const caret = ta ? ta.selectionStart : text.length;
    const before = text.slice(0, caret).replace(/\/theme\s+$/, "");
    setText(before + text.slice(caret));
    setThemeMenu(null);
    requestAnimationFrame(() => {
      if (ta) {
        ta.focus();
        ta.selectionStart = ta.selectionEnd = before.length;
      }
    });
  }

  // Turn multi-selection: click toggles a whole turn; shift-click extends from the
  // last clicked turn; clicking a SECTION inside a turn (a prose block or tool group)
  // toggles just that section, so a pick can be "this turn minus its tool calls".
  // Feeds both actions — compact-to-branch (summarize the picked content in place)
  // and extract-to-conversation (copy it into a fresh conversation). State maps
  // messageId → the set of picked part indexes (a full set = the whole turn).
  const [selecting, setSelecting] = useState(false);
  const [picked, setPicked] = useState<Map<string, Set<number>>>(new Map());
  const lastPick = useRef<string | null>(null);

  const idxOf = (id: string | null) => (id ? thread.findIndex((m) => m.id === id) : -1);
  const allIdxs = (m: Message) => new Set(m.parts.map((_, i) => i));
  const pickedCount = picked.size;
  const partialCount = thread.filter((m) => {
    const set = picked.get(m.id);
    return set !== undefined && set.size < m.parts.length;
  }).length;
  // Compact is limited to the session's OWN turns (the server 400s a selection that
  // reaches into ancestor history); extract has no such constraint.
  const pickedOwn = !sessionId ||
    thread.every((m) => !picked.has(m.id) || m.sessionId === sessionId);

  function pick(id: string, shift: boolean) {
    setPicked((prev) => {
      const next = new Map(prev);
      const a = idxOf(lastPick.current);
      const b = idxOf(id);
      if (shift && a >= 0 && b >= 0) {
        for (let i = Math.min(a, b); i <= Math.max(a, b); i++) {
          if (thread[i].parts.length) next.set(thread[i].id, allIdxs(thread[i]));
        }
      } else if (next.has(id)) next.delete(id);
      else {
        const m = thread[idxOf(id)];
        if (m?.parts.length) next.set(id, allIdxs(m));
      }
      return next;
    });
    lastPick.current = id;
  }
  function pickSection(id: string, idxs: number[]) {
    setPicked((prev) => {
      const next = new Map(prev);
      const set = new Set(next.get(id) ?? []);
      const on = idxs.every((i) => set.has(i));
      for (const i of idxs) {
        if (on) set.delete(i);
        else set.add(i);
      }
      if (set.size === 0) next.delete(id);
      else next.set(id, set);
      return next;
    });
    lastPick.current = id;
  }
  function resetSelect() {
    setSelecting(false);
    setPicked(new Map());
    lastPick.current = null;
  }
  /** The selection as API picks, in thread order (click order shouldn't matter). */
  function buildPicks(): TurnPick[] {
    return thread
      .filter((m) => picked.has(m.id))
      .map((m) => {
        const set = picked.get(m.id)!;
        return set.size >= m.parts.length
          ? { messageId: m.id }
          : { messageId: m.id, parts: [...set].sort((x, y) => x - y) };
      });
  }
  // Handoff: a one-line goal input in the toolbar; submit drafts the opening prompt
  // for a fresh conversation (slow: one LLM call — the button shows drafting…).
  const [handingOff, setHandingOff] = useState(false);
  const [handoffGoal, setHandoffGoal] = useState("");
  const [handoffBusy, setHandoffBusy] = useState(false);

  function submitHandoff() {
    const goal = handoffGoal.trim();
    if (!goal || !onHandoff || handoffBusy) return;
    setHandoffBusy(true);
    // The store opens the new session (or surfaces the error as a notice) — either
    // way this composer is done with the goal input.
    onHandoff(goal).finally(() => {
      setHandoffBusy(false);
      setHandingOff(false);
      setHandoffGoal("");
    });
  }

  function confirmCompact() {
    if (pickedCount === 0 || !pickedOwn || !onCompact) return;
    onCompact(buildPicks());
    resetSelect();
  }
  function confirmExtract() {
    if (pickedCount === 0 || !onExtract) return;
    onExtract(buildPicks());
    resetSelect();
  }

  // Stick to the newest turn as it streams — but release the instant the reader
  // scrolls UP, so mid-stream reading isn't yanked back down. Direction-based, not a
  // distance threshold: any upward move disengages follow (a small trackpad nudge
  // during a fast stream was getting overridden by the next delta); returning near
  // the bottom re-engages. Programmatic pins only move DOWN, so they never disengage.
  const follow = useRef(true);
  const lastTop = useRef(0);
  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    const dist = el.scrollHeight - el.scrollTop - el.clientHeight;
    if (el.scrollTop < lastTop.current - 1) follow.current = false; // user scrolled up
    else if (dist < 40) follow.current = true; // back at the bottom
    lastTop.current = el.scrollTop;
  };
  useEffect(() => {
    const el = scrollRef.current;
    if (el && follow.current) {
      el.scrollTop = el.scrollHeight;
      lastTop.current = el.scrollTop;
    }
  }, [thread, streaming]);

  function submit(branch: boolean) {
    let t = text.trim();
    if (!t || disabled) return;
    // Expand paste markers to their full text; a hand-deleted marker drops its paste.
    for (const p of pastes) t = t.split(pasteMarker(p.n)).join(p.text);
    onSend(t, branch);
    setText("");
    setPastes([]);
  }

  function onKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    // The /theme picker, when open, owns arrows / enter / esc.
    if (themeMenu) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        return setThemeMenu((m) => m && { ...m, active: Math.min(m.active + 1, themeEntries.length - 1) });
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        return setThemeMenu((m) => m && { ...m, active: Math.max(m.active - 1, 0) });
      }
      if (e.key === "Enter" || e.key === "Tab") {
        e.preventDefault();
        return pickTheme(themeEntries[themeMenu.active]);
      }
      if (e.key === "Escape") {
        e.preventDefault();
        return setThemeMenu(null);
      }
    }
    // The @ file menu, when open, owns arrows / enter / esc.
    if (atMenuOpen) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        return setFileActive((a) => Math.min(a + 1, fileMatches.length - 1));
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        return setFileActive((a) => Math.max(a - 1, 0));
      }
      if (e.key === "Enter" || e.key === "Tab") {
        e.preventDefault();
        return pickFile(fileMatches[fileActive]);
      }
      if (e.key === "Escape") {
        e.preventDefault();
        return setFileMatches([]);
      }
    }
    // The / skill menu mirrors it exactly (the two are mutually exclusive by token).
    if (slashMenuOpen) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        return setSkillActive((a) => Math.min(a + 1, skillMatches.length - 1));
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        return setSkillActive((a) => Math.max(a - 1, 0));
      }
      if (e.key === "Enter" || e.key === "Tab") {
        e.preventDefault();
        return pickSkill(skillMatches[skillActive].name);
      }
      if (e.key === "Escape") {
        e.preventDefault();
        return setSkillMatches([]);
      }
    }
    // Esc stops a running turn (matches the leading TUI agents).
    if (e.key === "Escape" && busy && onInterrupt) {
      e.preventDefault();
      onInterrupt();
      return;
    }
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      submit(e.altKey);
    }
  }

  return (
    <div
      style={{
        flex: 1,
        display: "flex",
        flexDirection: "column",
        minWidth: 0,
        // Without minHeight:0 this flex column grows to its content height, pushing
        // the composer off-screen and defeating the inner scroll area (root is
        // overflow:hidden, so the page can't scroll to compensate).
        minHeight: 0,
        background: c.panel,
      }}
    >
      {canBranch && (onCompact || onExtract || onHandoff) && thread.length > 0 && (
        <div
          style={{
            flex: "none",
            display: "flex",
            alignItems: "center",
            gap: 12,
            padding: "8px 34px",
            borderBottom: `1px solid ${c.border}`,
            background: selecting ? alpha(c.amber, 6) : c.panel,
            fontSize: 12,
          }}
        >
          {selecting ? (
            <>
              <span style={{ color: c.amber }}>
                {pickedCount > 0
                  ? `${pickedCount} turn${pickedCount === 1 ? "" : "s"} selected` +
                    (partialCount > 0 ? ` · ${partialCount} partial` : "")
                  : "Click turns to select · shift-click for a range · click a section to include/exclude it"}
              </span>
              <div style={{ flex: 1 }} />
              {onCompact && (
                <button
                  onClick={confirmCompact}
                  disabled={pickedCount === 0 || !pickedOwn}
                  title={!pickedOwn
                    ? "Selection includes inherited turns — compact only works on this session's own turns"
                    : "Summarize the selected turns in place on a new branch"}
                  style={{
                    fontSize: 12,
                    fontWeight: 600,
                    color: pickedCount && pickedOwn ? c.bg : c.muted2,
                    background: pickedCount && pickedOwn ? c.amber : c.border2,
                    borderRadius: 7,
                    padding: "5px 11px",
                  }}
                >
                  ⊟ Compact → branch
                </button>
              )}
              {onExtract && (
                <button
                  onClick={confirmExtract}
                  disabled={pickedCount === 0}
                  title="Copy the selected turns into a fresh conversation"
                  style={{
                    fontSize: 12,
                    fontWeight: 600,
                    color: pickedCount ? c.bg : c.muted2,
                    background: pickedCount ? c.green : c.border2,
                    borderRadius: 7,
                    padding: "5px 11px",
                  }}
                >
                  ⧉ New conversation
                </button>
              )}
              <button onClick={resetSelect} style={{ fontSize: 12, color: c.muted, border: `1px solid ${c.border}`, borderRadius: 7, padding: "5px 11px" }}>
                Cancel
              </button>
            </>
          ) : handingOff ? (
            <>
              <span style={{ color: c.green, flex: "none" }}>⤳ Handoff</span>
              <input
                autoFocus
                value={handoffGoal}
                onChange={(e) => setHandoffGoal(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") submitHandoff();
                  if (e.key === "Escape") {
                    setHandingOff(false);
                    setHandoffGoal("");
                  }
                }}
                disabled={handoffBusy}
                placeholder="Goal for the new conversation — what should it pick up and do?"
                style={{
                  flex: 1,
                  fontSize: 12.5,
                  color: c.text,
                  background: c.bg,
                  border: `1px solid ${c.border2}`,
                  borderRadius: 7,
                  padding: "5px 10px",
                  outline: "none",
                }}
              />
              <button
                onClick={submitHandoff}
                disabled={!handoffGoal.trim() || handoffBusy}
                title="Draft a self-contained opening prompt from this thread and open it in a fresh conversation"
                style={{
                  fontSize: 12,
                  fontWeight: 600,
                  color: handoffGoal.trim() && !handoffBusy ? c.bg : c.muted2,
                  background: handoffGoal.trim() && !handoffBusy ? c.green : c.border2,
                  borderRadius: 7,
                  padding: "5px 11px",
                }}
              >
                {handoffBusy ? "drafting…" : "Hand off"}
              </button>
              <button
                onClick={() => {
                  setHandingOff(false);
                  setHandoffGoal("");
                }}
                disabled={handoffBusy}
                style={{ fontSize: 12, color: c.muted, border: `1px solid ${c.border}`, borderRadius: 7, padding: "5px 11px" }}
              >
                Cancel
              </button>
            </>
          ) : (
            <>
              <span style={{ color: c.muted2, fontFamily: mono, fontSize: 11 }}>current thread</span>
              <div style={{ flex: 1 }} />
              {onHandoff && (
                <button
                  onClick={() => setHandingOff(true)}
                  title="Hand off to a fresh conversation: state a goal, get an editable opening prompt drafted from this thread"
                  style={{ fontSize: 12, color: c.muted, border: `1px solid ${c.border}`, borderRadius: 7, padding: "5px 11px" }}
                >
                  ⤳ Handoff
                </button>
              )}
              <button
                onClick={() => setSelecting(true)}
                title="Select turns to compact into a summary or extract into a new conversation"
                style={{ fontSize: 12, color: c.muted, border: `1px solid ${c.border}`, borderRadius: 7, padding: "5px 11px" }}
              >
                ⊟ Select turns
              </button>
            </>
          )}
        </div>
      )}
      <div
        ref={scrollRef}
        onScroll={onScroll}
        className="conv-scroll"
        style={{
          flex: 1,
          overflowY: "auto",
          padding: "26px 34px",
          minHeight: 0,
          opacity: dimmed ? 0.32 : 1,
          pointerEvents: dimmed ? "none" : undefined,
          transition: "opacity .2s",
        }}
      >
        {thread.length === 0 && (
          <div style={{ color: c.muted2, fontSize: 14, marginTop: 40, maxWidth: 520 }}>
            No turns yet. Describe a task below — bough plans, spawns workers, gates the
            network, and stages every change for your review.
          </div>
        )}
        {thread.map((m) => (
          <TurnView
            key={m.id}
            msg={m}
            live={streaming[m.id]}
            activity={activityFor(m.id)}
            subagents={subagents.filter((s) => s.originMessageId === m.id)}
            onOpenSession={onOpenSession}
            subagentThread={subagentThread}
            chipsAside={chipsAside}
            editable={canBranch && !!onForkEdit && m.role === "user"}
            onEdit={(id, t) => onForkEdit?.(id, t)}
            selecting={selecting}
            selectedParts={picked.get(m.id)}
            onPick={pick}
            onPickParts={pickSection}
            onBranchAt={canBranch && onBranchAt
              ? (partIdx, text) => onBranchAt(m.id, partIdx, text)
              : undefined}
          />
        ))}
        {waiting && <TumblingLogo />}
      </div>

      {queued.length > 0 && (
        <QueuedList queued={queued} onRemove={onRemoveQueued} onEdit={onEditQueued} />
      )}
      <div
        className="conv-composer"
        style={{
          flex: "none",
          padding: "16px 24px 18px",
          borderTop: `1px solid ${c.border}`,
          background: c.panel,
          position: "relative",
        }}
      >
        {atMenuOpen && (
          <div
            style={{
              position: "absolute",
              left: 24,
              right: 24,
              bottom: "100%",
              marginBottom: 6,
              maxHeight: 220,
              overflowY: "auto",
              background: c.panel2,
              border: `1px solid ${c.border}`,
              borderRadius: 10,
              boxShadow: "0 16px 40px rgba(0,0,0,.4)",
              padding: 5,
              zIndex: 20,
            }}
          >
            {fileMatches.map((f, i) => (
              <div
                key={f}
                onMouseEnter={() => setFileActive(i)}
                onMouseDown={(e) => {
                  e.preventDefault();
                  pickFile(f);
                }}
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: 8,
                  padding: "7px 10px",
                  borderRadius: 6,
                  background: i === fileActive ? c.panelInset : "transparent",
                  fontFamily: mono,
                  fontSize: 12,
                  color: i === fileActive ? c.text : c.text2,
                  cursor: "pointer",
                }}
              >
                <span style={{ color: c.green }}>@</span>
                {f}
              </div>
            ))}
          </div>
        )}
        {slashMenuOpen && (
          <div
            style={{
              position: "absolute",
              left: 24,
              right: 24,
              bottom: "100%",
              marginBottom: 6,
              maxHeight: 220,
              overflowY: "auto",
              background: c.panel2,
              border: `1px solid ${c.border}`,
              borderRadius: 10,
              boxShadow: "0 16px 40px rgba(0,0,0,.4)",
              padding: 5,
              zIndex: 20,
            }}
          >
            {skillMatches.map((s, i) => (
              <div
                key={s.name}
                onMouseEnter={() => setSkillActive(i)}
                onMouseDown={(e) => {
                  e.preventDefault();
                  pickSkill(s.name);
                }}
                style={{
                  display: "flex",
                  alignItems: "baseline",
                  gap: 10,
                  padding: "7px 10px",
                  borderRadius: 6,
                  background: i === skillActive ? c.panelInset : "transparent",
                  fontSize: 12,
                  cursor: "pointer",
                }}
              >
                <span style={{ fontFamily: mono, color: i === skillActive ? c.text : c.text2, flexShrink: 0 }}>
                  <span style={{ color: c.green }}>/</span>
                  {s.name}
                </span>
                <span
                  style={{
                    color: c.muted2,
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                    whiteSpace: "nowrap",
                  }}
                >
                  {s.description}
                </span>
              </div>
            ))}
          </div>
        )}
        {themeMenu && (
          <div
            style={{
              position: "absolute",
              left: 24,
              right: 24,
              bottom: "100%",
              marginBottom: 6,
              maxHeight: 220,
              overflowY: "auto",
              background: c.panel2,
              border: `1px solid ${c.border}`,
              borderRadius: 10,
              boxShadow: "0 16px 40px rgba(0,0,0,.4)",
              padding: 5,
              zIndex: 20,
            }}
          >
            {themeEntries.map((t, i) => {
              const active = i === themeMenu.active;
              const current = themeMenu.current === null ? t.colors === null : themeMenu.current === t.name;
              // Default's swatches come from the server's true defaults — the live
              // var() values would show the ACTIVE theme, not the default palette.
              const swatch = (key: string) =>
                t.colors?.[key] ?? themeMenu.defaults?.[key] ?? (c as Record<string, string>)[key];
              return (
                <div
                  key={t.name}
                  onMouseEnter={() => setThemeMenu((m) => m && { ...m, active: i })}
                  onMouseDown={(e) => {
                    e.preventDefault();
                    pickTheme(t);
                  }}
                  style={{
                    display: "flex",
                    alignItems: "center",
                    gap: 10,
                    padding: "7px 10px",
                    borderRadius: 6,
                    background: active ? c.panelInset : "transparent",
                    fontSize: 12,
                    cursor: "pointer",
                  }}
                >
                  <span style={{ display: "flex", gap: 3, flexShrink: 0 }}>
                    {["bg", "panel3", "text", "green", "amber", "red"].map((key) => (
                      <span
                        key={key}
                        style={{
                          width: 11,
                          height: 11,
                          borderRadius: "50%",
                          background: swatch(key),
                          border: `1px solid ${c.hairline}`,
                        }}
                      />
                    ))}
                  </span>
                  <span style={{ color: active ? c.text : c.text2 }}>{t.name}</span>
                  {current && <span style={{ color: c.green }}>✓ current</span>}
                  <span style={{ flex: 1 }} />
                  {t.colors === null && <span style={{ color: c.muted2 }}>bough's own palette</span>}
                </div>
              );
            })}
            <div style={{ padding: "6px 10px", fontSize: 11, color: c.muted2 }}>
              ↵ apply · esc dismiss · or keep typing to describe a custom theme for the model
            </div>
          </div>
        )}
        <div
          style={{
            border: `1px solid ${c.border}`,
            borderRadius: 11,
            background: c.panel3,
            padding: "13px 15px",
          }}
        >
          <textarea
            ref={taRef}
            value={text}
            onChange={(e) => {
              setText(e.target.value);
              refreshAtMenu(e.target.value, e.target.selectionStart);
              refreshSlashMenu(e.target.value, e.target.selectionStart);
              refreshThemeMenu(e.target.value, e.target.selectionStart);
            }}
            onKeyDown={onKeyDown}
            onPaste={onPaste}
            placeholder="Message bough…  @ to reference a file"
            rows={1}
            disabled={disabled}
            style={{
              width: "100%",
              resize: "none",
              border: "none",
              outline: "none",
              background: "transparent",
              color: c.text,
              fontFamily: sans,
              fontSize: 14,
              lineHeight: 1.5,
              minHeight: 22,
              maxHeight: 160,
            }}
          />
          {pastes.length > 0 && (
            <div style={{ display: "flex", flexWrap: "wrap", gap: 6, marginTop: 8 }}>
              {pastes.map((p) => (
                <span
                  key={p.n}
                  style={{
                    display: "inline-flex",
                    alignItems: "center",
                    gap: 7,
                    fontFamily: mono,
                    fontSize: 11,
                    color: c.muted,
                    background: c.panelInset,
                    border: `1px solid ${c.border3}`,
                    borderRadius: 6,
                    padding: "3px 8px",
                  }}
                >
                  ⎘ Pasted content #{p.n} ·{" "}
                  {p.text.includes("\n") ? `${p.text.split("\n").length} lines` : `${(p.text.length / 1000).toFixed(1)}k chars`}
                  <button
                    onClick={() => removePaste(p.n)}
                    title="Remove pasted content"
                    style={{ color: c.muted2, fontSize: 13, padding: 0, lineHeight: 1 }}
                  >
                    ×
                  </button>
                </span>
              ))}
            </div>
          )}
          <div
            style={{
              display: "flex",
              alignItems: "center",
              justifyContent: "space-between",
              marginTop: 16,
            }}
          >
            <div
              className="conv-hints"
              style={{
                display: "flex",
                alignItems: "center",
                gap: 14,
                fontFamily: mono,
                fontSize: 11,
                color: c.muted2,
              }}
            >
              {busy
                ? (
                  <>
                    <span style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
                      <Kbd>↵</Kbd> steer now
                    </span>
                    <span style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
                      <Kbd>⌥↵</Kbd> queue
                    </span>
                    <span style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
                      <Kbd>esc</Kbd> stop
                    </span>
                  </>
                )
                : (
                  <>
                    <span style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
                      <Kbd>⌥↵</Kbd> branch here
                    </span>
                    <span style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
                      <Kbd>↵</Kbd> send
                    </span>
                    <span>edit any past turn to fork</span>
                  </>
                )}
            </div>
            {busy && onInterrupt
              ? (
                <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                  {text.trim() && (
                    <button
                      onClick={() => submit(false)}
                      className="conv-send"
                      title="Steer: send now, the running turn picks it up (↵)"
                      style={{
                        width: 30,
                        height: 30,
                        borderRadius: 8,
                        background: c.green,
                        color: c.bg,
                        display: "flex",
                        alignItems: "center",
                        justifyContent: "center",
                        fontSize: 15,
                      }}
                    >
                      ↑
                    </button>
                  )}
                  <button
                    onClick={onInterrupt}
                    className="conv-send"
                    title="Stop the running turn (esc)"
                    style={{
                      width: 30,
                      height: 30,
                      borderRadius: 8,
                      background: c.panelInset,
                      border: `1px solid ${c.border}`,
                      color: c.red,
                      display: "flex",
                      alignItems: "center",
                      justifyContent: "center",
                      fontSize: 12,
                    }}
                  >
                    ■
                  </button>
                </div>
              )
              : (
                <button
                  onClick={() => submit(false)}
                  className="conv-send"
                  disabled={disabled || !text.trim()}
                  style={{
                    width: 30,
                    height: 30,
                    borderRadius: 8,
                    background: text.trim() ? c.green : c.border2,
                    color: text.trim() ? c.bg : c.muted,
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "center",
                    fontSize: 15,
                    transition: "background .15s",
                  }}
                >
                  ↑
                </button>
              )}
          </div>
        </div>
      </div>
    </div>
  );
}
