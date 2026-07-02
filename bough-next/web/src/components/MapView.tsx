// Screen 2 — the heads map. Slides out from the left (not a modal). Every head is a
// lane; the active head's spine is green, forks branch off the turn they diverged from,
// a dead worker attempt keeps its red tip. Zoom whole-tree → head → turn. Highlight a
// span (amber band) → Compact → branch. Built on @xyflow/react.
import { useMemo, useState } from "react";
import {
  Background,
  BackgroundVariant,
  Handle,
  Position,
  ReactFlow,
  type Edge,
  type Node,
  type NodeProps,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { c, mono } from "../theme";
import { mapEdges, mapNodes, type MapNode } from "../mock";
import { TitleBar } from "./TitleBar";

const toneStroke: Record<MapNode["tone"], string> = {
  green: c.green,
  muted: c.hairline,
  dead: c.border,
  compacted: c.amber,
};

type TurnData = MapNode & Record<string, unknown>;

function TurnNode({ data }: NodeProps) {
  const d = data as TurnData;
  const big = d.head;
  const size = big ? 19 : d.kind === "head" ? 15 : 15;
  const compacted = d.tone === "compacted";
  return (
    <div style={{ position: "relative", display: "flex", flexDirection: "column", alignItems: "center" }}>
      <Handle type="target" position={Position.Left} style={{ opacity: 0, left: 0 }} />
      {compacted ? (
        <div
          style={{
            width: 34,
            height: 22,
            borderRadius: 6,
            background: "rgba(217,180,95,.10)",
            border: `1.5px dashed ${c.amber}`,
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            color: c.amber,
            fontSize: 12,
          }}
        >
          ⊟
        </div>
      ) : (
        <div
          className={d.head ? "pulse-green" : undefined}
          style={{
            width: size,
            height: size,
            borderRadius: "50%",
            background: d.head ? c.green : c.panelInset,
            border: d.head ? "none" : `2px solid ${toneStroke[d.tone]}`,
          }}
        />
      )}
      {d.label && (
        <div
          style={{
            position: "absolute",
            top: compacted ? -16 : d.kind === "head" || d.tone !== "green" ? -18 : size + 4,
            whiteSpace: "nowrap",
            fontFamily: mono,
            fontSize: 10.5,
            color: d.tone === "green" ? (d.head ? c.green : c.muted) : d.tone === "compacted" ? c.amber : c.muted2,
            fontWeight: d.head ? 500 : 400,
          }}
        >
          {d.label}
        </div>
      )}
      {d.tip && (
        <div
          style={{
            position: "absolute",
            left: size + 6,
            top: -2,
            fontFamily: mono,
            fontSize: 10.5,
            whiteSpace: "nowrap",
            color: d.tip.tone === "green" ? c.green : d.tip.tone === "red" ? c.red : c.muted,
          }}
        >
          {d.tip.text}
        </div>
      )}
      <Handle type="source" position={Position.Right} style={{ opacity: 0, right: 0 }} />
    </div>
  );
}

const nodeTypes = { turn: TurnNode };

const zoomLevels = ["Tree", "Head", "Turn"] as const;

function MapCanvas() {
  const nodes: Node[] = useMemo(
    () =>
      mapNodes.map((n) => ({
        id: n.id,
        type: "turn",
        position: { x: n.x, y: n.y },
        data: n as unknown as Record<string, unknown>,
        draggable: false,
        selectable: false,
      })),
    []
  );
  const edges: Edge[] = useMemo(
    () =>
      mapEdges.map((e, i) => ({
        id: `e${i}`,
        source: e.from,
        target: e.to,
        type: "smoothstep",
        style: {
          stroke: toneStroke[e.tone],
          strokeWidth: e.tone === "green" ? 2.5 : 2,
          strokeDasharray: e.tone === "compacted" ? "5 4" : undefined,
          opacity: e.tone === "dead" ? 0.7 : 1,
        },
      })),
    []
  );

  return (
    <ReactFlow
      nodes={nodes}
      edges={edges}
      nodeTypes={nodeTypes}
      fitView
      fitViewOptions={{ padding: 0.26 }}
      minZoom={0.3}
      maxZoom={2.5}
      proOptions={{ hideAttribution: true }}
      nodesDraggable={false}
      nodesConnectable={false}
      panOnScroll
      zoomOnScroll
    >
      <Background variant={BackgroundVariant.Dots} gap={26} size={1} color="#1c2026" />
    </ReactFlow>
  );
}

export function MapView({ onClose }: { onClose: () => void }) {
  const [zoom, setZoom] = useState<(typeof zoomLevels)[number]>("Head");
  return (
    <div style={{ height: "100%", display: "flex", flexDirection: "column", background: c.panel }}>
      <TitleBar right={<span style={{ fontFamily: mono, fontSize: 11.5, color: c.muted2 }}>7 heads · 4 compactions</span>} />

      <div style={{ flex: 1, display: "flex", minHeight: 0 }}>
        {/* narrow heads list */}
        <div style={{ width: 232, flex: "none", background: c.panel2, borderRight: `1px solid ${c.border}`, padding: "14px 12px", overflowY: "auto" }}>
          <div style={{ fontSize: 11, letterSpacing: ".14em", color: c.muted2, fontWeight: 600, marginBottom: 12 }}>HEADS · 7</div>
          <div style={{ display: "flex", flexDirection: "column", gap: 6, fontSize: 12 }}>
            {[
              { g: "⎇", t: "main · migrate-auth", active: true, gc: c.green },
              { g: "↩", t: "edit · v3 format", gc: c.muted2 },
              { g: "◇", t: "worker · m'ware A", gc: c.muted2 },
              { g: "◇", t: "worker · m'ware B", gc: c.muted2, dead: true },
              { g: "⊟", t: "compacted · research", gc: c.amber },
              { g: "↩", t: "edit · patch only", gc: c.muted2 },
              { g: "◷", t: "main · pre-migration", gc: c.muted2 },
            ].map((h, i) => (
              <div
                key={i}
                style={{
                  padding: "7px 9px",
                  borderRadius: 6,
                  background: h.active ? c.panelInset : "transparent",
                  borderLeft: h.active ? `2px solid ${c.green}` : undefined,
                  color: h.dead ? c.muted2 : h.active ? c.text : c.muted,
                  textDecoration: h.dead ? "line-through" : undefined,
                }}
              >
                <span style={{ color: h.gc }}>{h.g}</span> {h.t}
              </div>
            ))}
          </div>
        </div>

        {/* map panel */}
        <div style={{ flex: 1, display: "flex", flexDirection: "column", minWidth: 0, background: c.canvas, position: "relative" }}>
          <div
            style={{
              position: "absolute",
              left: 0,
              top: 0,
              bottom: 0,
              width: 3,
              background: "linear-gradient(180deg,rgba(78,201,143,0),rgba(78,201,143,.35),rgba(78,201,143,0))",
              zIndex: 5,
            }}
          />
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
              <span style={{ fontFamily: mono, fontSize: 11, color: c.muted2 }}>all heads</span>
            </div>
            <div style={{ display: "flex", alignItems: "center", gap: 14 }}>
              <div style={{ display: "flex", background: c.panelInset, border: `1px solid ${c.border}`, borderRadius: 7, overflow: "hidden", fontSize: 11.5 }}>
                {zoomLevels.map((z) => (
                  <button
                    key={z}
                    onClick={() => setZoom(z)}
                    style={{
                      padding: "5px 11px",
                      color: z === zoom ? c.text : c.muted2,
                      background: z === zoom ? "#262b32" : "transparent",
                      borderLeft: z !== "Tree" ? `1px solid ${c.border}` : "none",
                    }}
                  >
                    {z}
                  </button>
                ))}
              </div>
              <div style={{ display: "flex", alignItems: "center", gap: 6, fontFamily: mono, fontSize: 12, color: c.muted2 }}>
                <span style={{ width: 22, height: 22, border: `1px solid ${c.border}`, borderRadius: 6, display: "inline-flex", alignItems: "center", justifyContent: "center" }}>−</span>
                <span>60%</span>
                <span style={{ width: 22, height: 22, border: `1px solid ${c.border}`, borderRadius: 6, display: "inline-flex", alignItems: "center", justifyContent: "center" }}>+</span>
              </div>
              <button onClick={onClose} style={{ fontSize: 11.5, color: c.muted, padding: "4px 10px", border: `1px solid ${c.border}`, borderRadius: 6 }}>
                ✕ Close
              </button>
            </div>
          </div>

          <div style={{ flex: 1, position: "relative", minHeight: 0 }}>
            <MapCanvas />
            {/* floating compact-to-branch action card */}
            <div
              style={{
                position: "absolute",
                left: 260,
                bottom: 90,
                width: 250,
                background: c.panelInset,
                border: `1px solid ${c.amber}`,
                borderRadius: 11,
                padding: "13px 14px",
                boxShadow: "0 16px 40px -12px rgba(0,0,0,.7)",
                zIndex: 10,
              }}
            >
              <div style={{ fontSize: 12.5, color: c.text, marginBottom: 3 }}>
                <span style={{ color: c.amber, fontWeight: 600 }}>3 turns</span> highlighted
              </div>
              <div style={{ fontSize: 11.5, color: c.muted, lineHeight: 1.5, marginBottom: 11 }}>
                Plan → Encoder → M'ware. Collapse into a summary on a new branch.
              </div>
              <div style={{ display: "flex", gap: 8 }}>
                <button style={{ flex: 1, textAlign: "center", padding: 7, borderRadius: 7, background: c.amber, color: c.panel, fontSize: 12, fontWeight: 600 }}>
                  ⊟ Compact → branch
                </button>
                <button style={{ textAlign: "center", padding: "7px 10px", border: `1px solid ${c.hairline}`, borderRadius: 7, fontSize: 12, color: c.muted }}>
                  Clear
                </button>
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
