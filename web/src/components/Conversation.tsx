// Center pane: the current head read top-to-bottom, plus the composer. User and
// supervisor turns render as prose; worker sub-agents and tool calls fold into quiet
// collapsed groups. The live turn streams in from the delta buffer.
import { useEffect, useRef, useState } from "react";
import { c, mono, sans } from "../theme";
import type { Message, Part } from "../types";
import type { ActivityGroup, WorkerActivity } from "../mock";
import { CopyId, Kbd } from "./ui";
import { Markdown } from "./Markdown";

const roleLabel: Record<string, { text: string; color: string }> = {
  user: { text: "YOU", color: c.muted2 },
  supervisor: { text: "BOUGH · supervisor", color: c.green },
  worker: { text: "◇ worker", color: c.muted },
};

function clip(s: string, max: number): string {
  return s.length > max ? s.slice(0, max) + `\n… (${s.length - max} more chars)` : s;
}

function ToolGroup({ parts }: { parts: Part[] }) {
  const [open, setOpen] = useState(false);
  const calls = parts.filter((p) => p.type === "tool_call") as Extract<Part, { type: "tool_call" }>[];
  const results = new Map(
    (parts.filter((p) => p.type === "tool_result") as Extract<Part, { type: "tool_result" }>[]).map(
      (r) => [r.callId, r]
    )
  );
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
              <div style={{ color: c.muted }}>
                <span style={{ color: c.green }}>◇</span> {call.name}
                {res && (
                  <span style={{ color: res.isError ? c.red : c.green, marginLeft: 8 }}>
                    {res.isError ? "✗ error" : "✓ done"}
                  </span>
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
  | { kind: "text"; text: string }
  | { kind: "reasoning"; text: string }
  | { kind: "tools"; parts: Part[] };

// Group a turn's parts into renderable segments, preserving their order. Consecutive
// tool_call/tool_result parts fold into one collapsible ToolGroup between prose blocks.
function segmentParts(parts: Part[]): Segment[] {
  const segs: Segment[] = [];
  for (const p of parts) {
    if (p.type === "text") segs.push({ kind: "text", text: p.text });
    else if (p.type === "reasoning") segs.push({ kind: "reasoning", text: p.text });
    else {
      const last = segs[segs.length - 1];
      if (last?.kind === "tools") last.parts.push(p);
      else segs.push({ kind: "tools", parts: [p] });
    }
  }
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
          background: "#181b20",
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

function TurnView({
  msg,
  live,
  activity,
  editable,
  onEdit,
  compacting,
  inSpan,
  onPick,
}: {
  msg: Message;
  live?: string;
  activity?: ActivityGroup;
  editable: boolean;
  onEdit: (id: string, text: string) => void;
  compacting: boolean;
  inSpan: boolean;
  onPick: (id: string) => void;
}) {
  const label = roleLabel[msg.role] ?? roleLabel.worker;
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

  return (
    <div
      onClick={compacting ? () => onPick(msg.id) : undefined}
      style={{
        marginBottom: 24,
        position: "relative",
        cursor: compacting ? "pointer" : undefined,
        borderRadius: 9,
        padding: compacting ? "8px 10px" : undefined,
        margin: compacting ? "0 -10px 16px" : undefined,
        border: compacting ? `1px solid ${inSpan ? c.amber : "transparent"}` : undefined,
        background: inSpan ? "rgba(217,180,95,.08)" : undefined,
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
        {!compacting && (
          // session/turn address — each turn carries its HOME session id, so on a fork
          // this points at the ancestor head an inherited turn actually lives in.
          <CopyId value={`${msg.sessionId}/${msg.id}`} title="Copy session/turn id" />
        )}
        {editable && !compacting && (
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
          <div style={{ display: "flex", flexDirection: "column", gap: 10, maxWidth: 640 }}>
            {segments.map((seg, i) => {
              if (seg.kind === "reasoning") {
                return (
                  <div
                    key={i}
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
                );
              }
              if (seg.kind === "tools") return <ToolGroup key={i} parts={seg.parts} />;
              return (
                <div key={i} style={{ fontSize: 14.5, lineHeight: 1.65, color: c.text2 }}>
                  <Markdown text={seg.text} />
                </div>
              );
            })}
            {(!!live || showCursor) && (
              <div style={{ fontSize: 14.5, lineHeight: 1.65, color: c.text2 }}>
                {live && <Markdown text={live} />}
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
        )}
      {activity && <ActivityView group={activity} />}
    </div>
  );
}

export function Conversation({
  thread,
  streaming,
  activity = [],
  dimmed = false,
  canBranch = false,
  busy = false,
  focusKey,
  onSend,
  onInterrupt,
  onSearchFiles,
  onForkEdit,
  onCompact,
  disabled,
}: {
  thread: Message[];
  streaming: Record<string, string>;
  // Changes when the open session changes; the composer refocuses so you can type
  // immediately after creating/switching a session.
  focusKey?: string | null;
  activity?: ActivityGroup[];
  dimmed?: boolean;
  // Live mode exposes fork/compact; mock leaves them off (the affordances are inert there).
  canBranch?: boolean;
  // A turn is streaming — the send button becomes a stop button and esc interrupts.
  busy?: boolean;
  onSend: (text: string, branch: boolean) => void;
  onInterrupt?: () => void;
  onForkEdit?: (messageId: string, text: string) => void;
  onCompact?: (fromId: string, toId: string) => void;
  // Fuzzy workspace file search for @ references; absent → no autocomplete.
  onSearchFiles?: (q: string) => Promise<string[]>;
  disabled: boolean;
}) {
  const activityFor = (id: string) => activity.find((a) => a.messageId === id);
  const [text, setText] = useState("");
  const scrollRef = useRef<HTMLDivElement>(null);
  const taRef = useRef<HTMLTextAreaElement>(null);

  // New/switched session → put the caret in the composer.
  useEffect(() => {
    taRef.current?.focus();
  }, [focusKey]);

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

  // Compact span selection: two clicked turns define an inclusive range to summarise.
  const [compacting, setCompacting] = useState(false);
  const [pickA, setPickA] = useState<string | null>(null);
  const [pickB, setPickB] = useState<string | null>(null);

  const idxOf = (id: string | null) => (id ? thread.findIndex((m) => m.id === id) : -1);
  const a = idxOf(pickA);
  const b = idxOf(pickB);
  const lo = a >= 0 && b >= 0 ? Math.min(a, b) : a;
  const hi = a >= 0 && b >= 0 ? Math.max(a, b) : a;
  const spanCount = lo >= 0 && hi >= 0 ? hi - lo + 1 : 0;

  const inSpan = (i: number) => lo >= 0 && i >= lo && i <= hi;

  function pick(id: string) {
    if (!pickA) return setPickA(id);
    if (!pickB) return setPickB(id);
    // Third click restarts the selection.
    setPickA(id);
    setPickB(null);
  }
  function resetCompact() {
    setCompacting(false);
    setPickA(null);
    setPickB(null);
  }
  function confirmCompact() {
    if (lo < 0 || hi < 0 || !onCompact) return;
    onCompact(thread[lo].id, thread[hi].id);
    resetCompact();
  }

  // Stick to the newest turn as it streams — but ONLY if the reader is already at
  // the bottom. Otherwise scrolling up to re-read gets yanked back down by every
  // streamed delta, which reads as "I can't scroll". `atBottom` is sampled before
  // the DOM paints so a just-arrived delta doesn't itself count as "scrolled up".
  const atBottom = useRef(true);
  const onScroll = () => {
    const el = scrollRef.current;
    if (el) atBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
  };
  useEffect(() => {
    const el = scrollRef.current;
    if (el && atBottom.current) el.scrollTop = el.scrollHeight;
  }, [thread, streaming]);

  function submit(branch: boolean) {
    const t = text.trim();
    if (!t || disabled) return;
    onSend(t, branch);
    setText("");
  }

  function onKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
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
      {canBranch && onCompact && thread.length > 0 && (
        <div
          style={{
            flex: "none",
            display: "flex",
            alignItems: "center",
            gap: 12,
            padding: "8px 34px",
            borderBottom: `1px solid ${c.border}`,
            background: compacting ? "rgba(217,180,95,.06)" : c.panel,
            fontSize: 12,
          }}
        >
          {compacting ? (
            <>
              <span style={{ color: c.amber }}>
                {spanCount > 0 ? `${spanCount} turn${spanCount === 1 ? "" : "s"} selected` : "Click the first and last turn to compact"}
              </span>
              <div style={{ flex: 1 }} />
              <button
                onClick={confirmCompact}
                disabled={spanCount === 0}
                style={{ fontSize: 12, fontWeight: 600, color: spanCount ? c.bg : c.muted2, background: spanCount ? c.amber : "#262b32", borderRadius: 7, padding: "5px 11px" }}
              >
                ⊟ Compact → branch
              </button>
              <button onClick={resetCompact} style={{ fontSize: 12, color: c.muted, border: `1px solid ${c.border}`, borderRadius: 7, padding: "5px 11px" }}>
                Cancel
              </button>
            </>
          ) : (
            <>
              <span style={{ color: c.muted2, fontFamily: mono, fontSize: 11 }}>current thread</span>
              <div style={{ flex: 1 }} />
              <button
                onClick={() => setCompacting(true)}
                style={{ fontSize: 12, color: c.muted, border: `1px solid ${c.border}`, borderRadius: 7, padding: "5px 11px" }}
              >
                ⊟ Compact a span
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
        {thread.map((m, i) => (
          <TurnView
            key={m.id}
            msg={m}
            live={streaming[m.id]}
            activity={activityFor(m.id)}
            editable={canBranch && !!onForkEdit && m.role === "user"}
            onEdit={(id, t) => onForkEdit?.(id, t)}
            compacting={compacting}
            inSpan={inSpan(i)}
            onPick={pick}
          />
        ))}
      </div>

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
            }}
            onKeyDown={onKeyDown}
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
                  <span style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
                    <Kbd>esc</Kbd> stop
                  </span>
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
                    background: text.trim() ? c.green : "#262b32",
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
