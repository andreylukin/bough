// Screen 2, live. The heads map derived from real data: one lane per session (all
// heads) with its own turns as dots, parentId links as edges, kind glyphs (↩ fork,
// ⊟ compaction), the current head highlighted. Click a turn → jump to that session;
// select a span of one head's turns → Compact → branch. Zoom whole-tree → single-turn
// via a CSS-scaled canvas (native scroll pans). New sessions (session.created) arrive
// through the live `sessions` prop, so the map updates without reopening.
//
// The mock map (MapView) stays for design review; this renders in live mode.
import { useEffect, useMemo, useState } from "react";
import { c, mono } from "../theme";
import { api } from "../api";
import type { Message, Session } from "../types";
import { TitleBar } from "./TitleBar";

const kindGlyph: Record<Session["kind"], string> = {
  root: "⎇",
  fork: "↩",
  worker: "◇",
  compaction: "⊟",
};
const kindColor: Record<Session["kind"], string> = {
  root: c.green,
  fork: c.muted2,
  worker: c.muted2,
  compaction: c.amber,
};

const LANE_H = 78;
const LABEL_W = 190;
const TURN_W = 40;
const TOP = 30;
const LEFT = 28;
const INDENT = 46; // per lineage-depth, so branched heads step right of their origin

const ZOOMS = { Tree: 0.55, Head: 0.9, Turn: 1.5 } as const;
type ZoomKey = keyof typeof ZOOMS;

type Lane = { session: Session; turns: Message[]; y: number; depth: number };

function dotColor(m: Message): string {
  if (m.pending) return c.amber;
  if (m.role === "supervisor") return c.green;
  return c.hairline;
}

export function LiveMapView({
  sessions,
  currentId,
  onJump,
  onCompact,
  onClose,
}: {
  sessions: Session[];
  currentId: string | null;
  onJump: (sessionId: string) => void;
  onCompact: (sessionId: string, fromId: string, toId: string) => void;
  onClose: () => void;
}) {
  const [threads, setThreads] = useState<Record<string, Message[]>>({});
  const [zoomKey, setZoomKey] = useState<ZoomKey>("Head");
  const [nudge, setNudge] = useState(1); // −/+ multiplier on the preset
  const scale = ZOOMS[zoomKey] * nudge;

  const [compacting, setCompacting] = useState(false);
  const [sel, setSel] = useState<{ sessionId: string; from: string; to?: string } | null>(null);

  // Fetch each head's OWN turns (thread filtered to messages the session owns). Lazy on
  // the session set; a small cache keyed by id avoids refetching unchanged heads.
  useEffect(() => {
    let alive = true;
    (async () => {
      const missing = sessions.filter((s) => !(s.id in threads));
      if (missing.length === 0) return;
      const fetched = await Promise.all(
        missing.map(async (s) => {
          try {
            const { thread } = await api.getSession(s.id);
            return [s.id, thread.filter((m) => m.sessionId === s.id)] as const;
          } catch {
            return [s.id, [] as Message[]] as const;
          }
        })
      );
      if (alive) setThreads((prev) => ({ ...prev, ...Object.fromEntries(fetched) }));
    })();
    return () => {
      alive = false;
    };
  }, [sessions, threads]);

  // Depth via parentId chain (roots = 0). Current head first, then newest → oldest.
  const lanes: Lane[] = useMemo(() => {
    const byId = new Map(sessions.map((s) => [s.id, s]));
    // Depth follows the lineage origin (task #18) so a fork steps right of the head it
    // branched from; falls back to parentId. Bounded against cycles.
    const depthOf = (s: Session): number => {
      let d = 0;
      let cur: Session | undefined = s;
      const seen = new Set<string>();
      for (;;) {
        const link = cur?.originId ?? cur?.parentId;
        if (!link || !byId.has(link) || seen.has(cur!.id)) break;
        seen.add(cur!.id);
        cur = byId.get(link);
        d++;
      }
      return d;
    };
    // Lineage-tree order: roots newest-first, each branched head placed right after the
    // head it came from, so its connector is a short hop rather than a long lane-crossing.
    const childrenOf = new Map<string, Session[]>();
    const roots: Session[] = [];
    for (const s of sessions) {
      const link = s.originId ?? s.parentId;
      if (link && byId.has(link) && link !== s.id) {
        const arr = childrenOf.get(link) ?? [];
        arr.push(s);
        childrenOf.set(link, arr);
      } else {
        roots.push(s);
      }
    }
    const ordered: Session[] = [];
    const seen = new Set<string>();
    const visit = (s: Session) => {
      if (seen.has(s.id)) return;
      seen.add(s.id);
      ordered.push(s);
      for (const k of (childrenOf.get(s.id) ?? []).sort((a, b) => a.createdAt - b.createdAt)) visit(k);
    };
    roots.sort((a, b) => b.createdAt - a.createdAt).forEach(visit);
    // Any stragglers (cycles / missing links) appended so nothing vanishes.
    for (const s of sessions) if (!seen.has(s.id)) ordered.push(s);

    return ordered.map((session, i) => ({
      session,
      turns: threads[session.id] ?? [],
      y: TOP + i * LANE_H,
      depth: depthOf(session),
    }));
  }, [sessions, threads]);

  const laneById = useMemo(() => new Map(lanes.map((l) => [l.session.id, l])), [lanes]);

  const maxDepth = Math.max(0, ...lanes.map((l) => l.depth));
  const canvasW = LEFT + maxDepth * INDENT + LABEL_W + Math.max(4, ...lanes.map((l) => l.turns.length)) * TURN_W + 120;
  const canvasH = TOP + lanes.length * LANE_H + 40;

  // Deterministic geometry so lineage edges can anchor on a specific turn dot. Mirrors
  // the flex layout below: label (LABEL_W) + gap, then dots stepped by TURN_W.
  const laneX = (l: Lane) => LEFT + l.depth * INDENT;
  const dotCX = (l: Lane, i: number) => laneX(l) + LABEL_W + 16 + i * TURN_W;
  const DOT_CY = 15;

  const currentTitle = sessions.find((s) => s.id === currentId)?.title ?? "map";

  function clickTurn(sessionId: string, msgId: string) {
    if (!compacting) return onJump(sessionId);
    if (!sel || sel.sessionId !== sessionId) return setSel({ sessionId, from: msgId });
    setSel({ ...sel, to: msgId });
  }

  const selCount = (() => {
    if (!sel) return 0;
    const turns = threads[sel.sessionId] ?? [];
    const a = turns.findIndex((m) => m.id === sel.from);
    const b = sel.to ? turns.findIndex((m) => m.id === sel.to) : a;
    if (a < 0 || b < 0) return 0;
    return Math.abs(b - a) + 1;
  })();

  function inSel(sessionId: string, i: number): boolean {
    if (!sel || sel.sessionId !== sessionId) return false;
    const turns = threads[sessionId] ?? [];
    const a = turns.findIndex((m) => m.id === sel.from);
    const b = sel.to ? turns.findIndex((m) => m.id === sel.to) : a;
    const lo = Math.min(a, b);
    const hi = Math.max(a, b);
    return i >= lo && i <= hi;
  }

  function confirmCompact() {
    if (!sel || selCount === 0) return;
    const turns = threads[sel.sessionId] ?? [];
    const a = turns.findIndex((m) => m.id === sel.from);
    const b = sel.to ? turns.findIndex((m) => m.id === sel.to) : a;
    const from = turns[Math.min(a, b)].id;
    const to = turns[Math.max(a, b)].id;
    onCompact(sel.sessionId, from, to);
    setCompacting(false);
    setSel(null);
  }

  return (
    <div style={{ height: "100%", display: "flex", flexDirection: "column", background: c.panel }}>
      <TitleBar
        live
        connected
        branch={currentTitle}
        right={
          <span style={{ fontFamily: mono, fontSize: 11.5, color: c.muted2 }}>
            {sessions.length} heads · {sessions.filter((s) => s.kind === "compaction").length} compactions
          </span>
        }
      />

      <div style={{ flex: 1, display: "flex", minHeight: 0 }}>
        <div style={{ flex: 1, display: "flex", flexDirection: "column", minWidth: 0, background: c.canvas, position: "relative" }}>
          {/* header + zoom controls */}
          <div
            style={{
              height: 44,
              flex: "none",
              display: "flex",
              alignItems: "center",
              justifyContent: "space-between",
              padding: "0 18px",
              borderBottom: `1px solid ${c.border2}`,
            }}
          >
            <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
              <span style={{ fontSize: 12.5, fontWeight: 600, color: c.text }}>MAP</span>
              <span style={{ fontFamily: mono, fontSize: 11, color: c.muted2 }}>all heads · live</span>
            </div>
            <div style={{ display: "flex", alignItems: "center", gap: 14 }}>
              {compacting ? (
                <>
                  <span style={{ fontSize: 12, color: c.amber }}>
                    {selCount > 0 ? `${selCount} turn${selCount === 1 ? "" : "s"} selected` : "Click first & last turn of one head"}
                  </span>
                  <button
                    onClick={confirmCompact}
                    disabled={selCount === 0}
                    style={{ fontSize: 12, fontWeight: 600, color: selCount ? c.panel : c.muted2, background: selCount ? c.amber : "#262b32", borderRadius: 7, padding: "5px 11px" }}
                  >
                    ⊟ Compact → branch
                  </button>
                  <button onClick={() => { setCompacting(false); setSel(null); }} style={{ fontSize: 12, color: c.muted, border: `1px solid ${c.border}`, borderRadius: 7, padding: "5px 11px" }}>
                    Cancel
                  </button>
                </>
              ) : (
                <button onClick={() => setCompacting(true)} style={{ fontSize: 12, color: c.muted, border: `1px solid ${c.border}`, borderRadius: 7, padding: "5px 11px" }}>
                  ⊟ Compact a span
                </button>
              )}
              <div style={{ display: "flex", background: c.panelInset, border: `1px solid ${c.border}`, borderRadius: 7, overflow: "hidden", fontSize: 11.5 }}>
                {(Object.keys(ZOOMS) as ZoomKey[]).map((z, i) => (
                  <button
                    key={z}
                    onClick={() => { setZoomKey(z); setNudge(1); }}
                    style={{ padding: "5px 11px", color: z === zoomKey ? c.text : c.muted2, background: z === zoomKey ? "#262b32" : "transparent", borderLeft: i ? `1px solid ${c.border}` : "none" }}
                  >
                    {z}
                  </button>
                ))}
              </div>
              <div style={{ display: "flex", alignItems: "center", gap: 6, fontFamily: mono, fontSize: 12, color: c.muted2 }}>
                <button onClick={() => setNudge((n) => Math.max(0.4, n - 0.15))} style={{ width: 22, height: 22, border: `1px solid ${c.border}`, borderRadius: 6, color: c.muted }}>−</button>
                <span style={{ width: 40, textAlign: "center" }}>{Math.round(scale * 100)}%</span>
                <button onClick={() => setNudge((n) => Math.min(2.5, n + 0.15))} style={{ width: 22, height: 22, border: `1px solid ${c.border}`, borderRadius: 6, color: c.muted }}>+</button>
              </div>
              <button onClick={onClose} style={{ fontSize: 11.5, color: c.muted, padding: "4px 10px", border: `1px solid ${c.border}`, borderRadius: 6 }}>✕ Close</button>
            </div>
          </div>

          {/* scroll = pan; the inner canvas is CSS-scaled = zoom */}
          <div style={{ flex: 1, overflow: "auto", minHeight: 0, backgroundImage: "radial-gradient(circle,#1c2026 1px,transparent 1px)", backgroundSize: "26px 26px" }}>
            <div style={{ width: canvasW * scale, height: canvasH * scale }}>
              <div style={{ width: canvasW, height: canvasH, transform: `scale(${scale})`, transformOrigin: "top left", position: "relative" }}>
                {/* Lineage edges: from the origin turn's dot to the branched head. Uses
                    origin (task #18) when present; falls back to parentId. The connector
                    anchors on originMessageId's dot — the design's branch-off-a-turn look. */}
                <svg width={canvasW} height={canvasH} style={{ position: "absolute", inset: 0, overflow: "visible", pointerEvents: "none" }}>
                  {lanes.map((l) => {
                    const originId = l.session.originId ?? l.session.parentId;
                    if (!originId) return null;
                    const origin = laneById.get(originId);
                    if (!origin) return null;
                    // Anchor at the divergence turn's dot, else the origin's tip.
                    const oTurns = origin.turns;
                    let idx = l.session.originMessageId ? oTurns.findIndex((m) => m.id === l.session.originMessageId) : -1;
                    if (idx < 0) idx = oTurns.length - 1;
                    const x1 = idx >= 0 ? dotCX(origin, idx) : laneX(origin) + LABEL_W / 2;
                    const y1 = origin.y + DOT_CY;
                    const x2 = l.turns.length ? dotCX(l, 0) : laneX(l) + 8;
                    const y2 = l.y + DOT_CY;
                    const compaction = l.session.kind === "compaction";
                    const midY = (y1 + y2) / 2;
                    return (
                      <path
                        key={l.session.id}
                        d={`M${x1},${y1} C${x1},${midY} ${x2},${midY} ${x2},${y2}`}
                        stroke={compaction ? c.amber : c.hairline}
                        strokeWidth={1.5}
                        strokeDasharray={compaction ? "5 4" : undefined}
                        opacity={0.75}
                        fill="none"
                      />
                    );
                  })}
                </svg>

                {lanes.map((l) => {
                  const s = l.session;
                  const current = s.id === currentId;
                  const x = laneX(l);
                  return (
                    <div key={s.id} style={{ position: "absolute", left: x, top: l.y, display: "flex", alignItems: "center", gap: 10, whiteSpace: "nowrap" }}>
                      {/* lane label */}
                      <button
                        onClick={() => !compacting && onJump(s.id)}
                        title={s.title}
                        style={{
                          width: LABEL_W,
                          textAlign: "left",
                          display: "flex",
                          alignItems: "center",
                          gap: 7,
                          padding: "6px 9px",
                          borderRadius: 7,
                          background: current ? c.panelInset : "transparent",
                          border: current ? `1px solid ${c.border}` : "1px solid transparent",
                          borderLeft: current ? `2px solid ${c.green}` : undefined,
                          overflow: "hidden",
                        }}
                      >
                        <span style={{ color: kindColor[s.kind], flex: "none" }}>{kindGlyph[s.kind]}</span>
                        <span style={{ fontSize: 12, color: current ? c.text : c.muted, overflow: "hidden", textOverflow: "ellipsis" }}>{s.title}</span>
                      </button>

                      {/* turn dots */}
                      <div style={{ display: "flex", alignItems: "center" }}>
                        {l.turns.length === 0 && (
                          <span style={{ fontFamily: mono, fontSize: 10.5, color: c.muted2 }}>· no turns</span>
                        )}
                        {l.turns.map((m, i) => {
                          const selected = inSel(s.id, i);
                          const isLast = i === l.turns.length - 1;
                          return (
                            <div key={m.id} style={{ display: "flex", alignItems: "center" }}>
                              {i > 0 && <div style={{ width: TURN_W - 14, height: 2, background: current ? c.green : c.border }} />}
                              <button
                                onClick={(e) => { e.stopPropagation(); clickTurn(s.id, m.id); }}
                                title={m.role + ": " + m.parts.map((p) => ("text" in p ? p.text : "")).join(" ").slice(0, 60)}
                                className={m.pending ? "pulse-amber" : undefined}
                                style={{
                                  width: current && isLast ? 15 : 12,
                                  height: current && isLast ? 15 : 12,
                                  borderRadius: "50%",
                                  flex: "none",
                                  background: selected ? c.amber : current && isLast ? c.green : c.panelInset,
                                  border: selected ? `2px solid ${c.amber}` : `2px solid ${dotColor(m)}`,
                                  boxShadow: selected ? `0 0 0 3px rgba(217,180,95,.2)` : undefined,
                                  cursor: "pointer",
                                }}
                              />
                            </div>
                          );
                        })}
                        {current && l.turns.length > 0 && (
                          <span style={{ marginLeft: 8, fontFamily: mono, fontSize: 10.5, color: c.green }}>head</span>
                        )}
                      </div>
                    </div>
                  );
                })}
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
