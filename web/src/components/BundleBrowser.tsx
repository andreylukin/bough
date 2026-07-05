// Screen 5 — network bundle browser + config. Community Claw Patrol policy bundles are
// browsed like packages (publisher, version, verified signature, install count).
// Selecting one renders its typed parameters as a safe form — never hand-edited HCL.
import { useState } from "react";
import { c, alpha, mono } from "../theme";
import { bundles as mockBundles, type Bundle, type BundleParam } from "../mock";
import { TitleBar } from "./TitleBar";

function Field({ p }: { p: BundleParam }) {
  switch (p.kind) {
    case "text":
      return (
        <div>
          <label style={{ display: "block", fontSize: 12, color: c.text2, marginBottom: 6 }}>{p.label}</label>
          <div style={{ padding: "8px 11px", border: `1px solid ${c.border}`, borderRadius: 8, background: c.panel, fontFamily: mono, fontSize: 12, color: c.text }}>
            {p.value}
          </div>
        </div>
      );
    case "select":
      return (
        <div>
          <label style={{ display: "block", fontSize: 12, color: c.text2, marginBottom: 6 }}>{p.label}</label>
          <div style={{ padding: "8px 11px", border: `1px solid ${c.border}`, borderRadius: 8, background: c.panel, display: "flex", alignItems: "center" }}>
            <span style={{ fontFamily: mono, fontSize: 12, color: c.text, flex: 1 }}>{p.value}</span>
            <span style={{ color: c.muted2 }}>▾</span>
          </div>
        </div>
      );
    case "multiselect":
      return (
        <div>
          <label style={{ display: "block", fontSize: 12, color: c.text2, marginBottom: 7 }}>{p.label}</label>
          <div style={{ display: "flex", flexWrap: "wrap", gap: 6, fontFamily: mono, fontSize: 11 }}>
            {p.selected?.map((v) => (
              <span key={v} style={{ padding: "4px 10px", borderRadius: 6, background: alpha(c.green, 14), color: c.green, border: `1px solid ${alpha(c.green, 40)}` }}>
                {v} ✕
              </span>
            ))}
            {p.available?.map((v) => (
              <span key={v} style={{ padding: "4px 10px", borderRadius: 6, color: c.muted2, border: `1px dashed ${c.hairline}` }}>
                + {v}
              </span>
            ))}
          </div>
        </div>
      );
    case "toggle":
      return (
        <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", padding: "11px 12px", border: `1px solid ${c.border2}`, borderRadius: 9, background: c.panel3 }}>
          <div>
            <div style={{ fontSize: 12, color: c.text }}>{p.label}</div>
            <div style={{ fontSize: 10.5, color: c.muted2 }}>{p.hint}</div>
          </div>
          <div style={{ width: 38, height: 22, borderRadius: 11, background: p.on ? c.green : c.hairline, position: "relative", flex: "none" }}>
            <span style={{ position: "absolute", top: 2, [p.on ? "right" : "left"]: 2, width: 18, height: 18, borderRadius: "50%", background: c.bg } as React.CSSProperties} />
          </div>
        </div>
      );
    case "number":
      return (
        <div>
          <label style={{ display: "block", fontSize: 12, color: c.text2, marginBottom: 6 }}>{p.label}</label>
          <div style={{ display: "flex", alignItems: "center", width: 120, border: `1px solid ${c.border}`, borderRadius: 8, background: c.panel, overflow: "hidden" }}>
            <span style={{ flex: 1, padding: "8px 11px", fontFamily: mono, fontSize: 12, color: c.text }}>{p.value}</span>
            <span style={{ width: 26, textAlign: "center", color: c.muted2, borderLeft: `1px solid ${c.border}`, padding: "8px 0" }}>−</span>
            <span style={{ width: 26, textAlign: "center", color: c.muted2, borderLeft: `1px solid ${c.border}`, padding: "8px 0" }}>+</span>
          </div>
        </div>
      );
  }
}

function BundleCard({ b, selected, onSelect }: { b: Bundle; selected: boolean; onSelect: () => void }) {
  return (
    <button
      onClick={onSelect}
      style={{
        textAlign: "left",
        border: selected ? `1px solid ${c.green}` : `1px solid ${c.border2}`,
        borderRadius: 11,
        background: selected ? alpha(c.green, 5) : c.panel3,
        padding: 14,
      }}
    >
      <div style={{ display: "flex", alignItems: "center", gap: 9, marginBottom: 9 }}>
        <div style={{ width: 28, height: 28, borderRadius: 7, background: c.panelInset, border: `1px solid ${c.border}`, display: "flex", alignItems: "center", justifyContent: "center", color: c.muted, fontSize: 13 }}>
          {b.glyph}
        </div>
        <div style={{ minWidth: 0 }}>
          <div style={{ fontFamily: mono, fontSize: 12.5, color: c.text }}>{b.name}</div>
          <div style={{ fontSize: 10.5, color: c.muted2 }}>
            {b.publisher} · {b.version}
          </div>
        </div>
      </div>
      <p style={{ margin: "0 0 11px", fontSize: 11.5, color: c.muted, lineHeight: 1.5 }}>{b.desc}</p>
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between" }}>
        <span style={{ fontFamily: mono, fontSize: 10.5, color: c.muted2 }}>↓ {b.installs}</span>
        {b.state === "configuring" ? (
          <span style={{ fontSize: 11.5, color: c.green, fontWeight: 600 }}>Configuring →</span>
        ) : b.state === "installed" ? (
          <span style={{ fontSize: 11.5, color: c.green }}>✓ Installed</span>
        ) : (
          <span style={{ fontSize: 11.5, color: c.muted, border: `1px solid ${c.border}`, borderRadius: 6, padding: "3px 11px" }}>Install</span>
        )}
      </div>
    </button>
  );
}

export function BundleBrowser({
  onClose,
  bundles = mockBundles,
  onInstall,
}: {
  onClose: () => void;
  bundles?: Bundle[];
  onInstall?: (id: string) => void;
}) {
  const allBundles = bundles.length ? bundles : mockBundles;
  const [selectedId, setSelectedId] = useState(
    allBundles.find((b) => b.state === "configuring")?.id ?? allBundles[0].id
  );
  const selected = allBundles.find((b) => b.id === selectedId) ?? allBundles[0];

  return (
    <div style={{ height: "100%", display: "flex", flexDirection: "column", background: c.panel }}>
      <TitleBar
        branch="bundles"
        right={
          <button onClick={onClose} style={{ fontSize: 11.5, color: c.muted, padding: "4px 10px", border: `1px solid ${c.border}`, borderRadius: 6 }}>
            ✕ Close
          </button>
        }
      />

      <div className="bundles-body" style={{ flex: 1, display: "flex", minHeight: 0 }}>
        {/* left categories */}
        <div className="bundles-side" style={{ width: 196, flex: "none", background: c.panel2, borderRight: `1px solid ${c.border}`, padding: "16px 12px", fontSize: 12.5 }}>
          <div style={{ position: "relative", marginBottom: 16 }}>
            <div style={{ padding: "7px 10px 7px 28px", border: `1px solid ${c.border}`, borderRadius: 7, background: c.panel, color: c.muted2, fontSize: 12 }}>
              Search bundles
            </div>
            <span style={{ position: "absolute", left: 9, top: 7, color: c.muted2 }}>⌕</span>
          </div>
          <div style={{ display: "flex", flexDirection: "column", gap: 2 }}>
            {[
              // Real counts — a sidebar claiming "48 bundles" over a one-bundle
              // list reads as broken and poisons trust in every other number.
              ["All bundles", String(allBundles.length), true],
              ["Installed", String(allBundles.filter((b) => b.state === "installed").length), false],
              ["Cloud & infra", "", false],
              ["Source control", "", false],
              ["Package registries", "", false],
              ["AI & APIs", "", false],
            ].map(([label, count, active]) => (
              <div key={label as string} style={{ padding: "7px 10px", borderRadius: 6, background: active ? c.panelInset : "transparent", color: active ? c.text : c.muted }}>
                {label}
                {count ? <span style={{ float: "right", color: c.muted2, fontFamily: mono, fontSize: 11 }}>{count as string}</span> : null}
              </div>
            ))}
          </div>
          <div style={{ marginTop: 20, fontSize: 11, letterSpacing: ".12em", color: c.muted2, fontWeight: 600, marginBottom: 8 }}>SIGNED BY</div>
          <div style={{ display: "flex", flexDirection: "column", gap: 6, fontFamily: mono, fontSize: 11, color: c.muted }}>
            <span>
              <span style={{ color: c.green }}>✓</span> bough-verified
            </span>
            <span style={{ color: c.muted2 }}>community</span>
          </div>
        </div>

        {/* center grid */}
        <div style={{ flex: 1, minWidth: 0, overflowY: "auto", padding: "20px 22px", background: c.canvas }}>
          <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: 16 }}>
            <span style={{ fontSize: 13, color: c.muted }}>All bundles</span>
            <span style={{ display: "inline-flex", alignItems: "center", gap: 7, fontSize: 12, color: c.muted2 }}>Sort · popular ▾</span>
          </div>
          <div className="bundles-grid" style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 12 }}>
            {allBundles.map((b) => (
              <BundleCard key={b.id} b={b} selected={b.id === selectedId} onSelect={() => setSelectedId(b.id)} />
            ))}
          </div>
        </div>

        {/* right config form */}
        <div className="bundles-detail" style={{ width: 378, flex: "none", background: c.panel2, borderLeft: `1px solid ${c.border}`, display: "flex", flexDirection: "column", minHeight: 0 }}>
          <div style={{ flex: "none", padding: "16px 18px 14px", borderBottom: `1px solid ${c.border}` }}>
            <div style={{ display: "flex", alignItems: "center", gap: 10, marginBottom: 6 }}>
              <div style={{ width: 30, height: 30, borderRadius: 8, background: c.panelInset, border: `1px solid ${c.border}`, display: "flex", alignItems: "center", justifyContent: "center", color: c.muted }}>
                {selected.glyph}
              </div>
              <div>
                <div style={{ fontFamily: mono, fontSize: 13.5, color: c.text }}>{selected.name}</div>
                <div style={{ fontSize: 10.5, color: c.green }}>
                  {selected.verified ? "✓ " : ""}
                  {selected.publisher} · {selected.version}
                </div>
              </div>
            </div>
            <p style={{ margin: 0, fontSize: 11.5, color: c.muted2, lineHeight: 1.5 }}>
              {selected.params
                ? `This bundle exposes ${selected.params.length} parameters. bough renders them below — you never touch raw policy.`
                : "This bundle installs with sane defaults. No parameters to configure."}
            </p>
          </div>

          <div style={{ flex: 1, overflowY: "auto", padding: "16px 18px", display: "flex", flexDirection: "column", gap: 16 }}>
            {selected.params?.map((p) => <Field key={p.label} p={p} />) ?? (
              <div style={{ color: c.muted2, fontSize: 12 }}>No parameters.</div>
            )}
          </div>

          <div style={{ flex: "none", padding: "14px 18px", borderTop: `1px solid ${c.border}`, display: "flex", gap: 9, alignItems: "center" }}>
            <span style={{ fontSize: 11, color: c.muted2, flex: 1 }}>Applies to the live gate on install</span>
            <button onClick={onClose} style={{ fontSize: 12.5, color: c.muted, padding: "8px 13px", border: `1px solid ${c.border}`, borderRadius: 8 }}>Cancel</button>
            <button
              onClick={() => onInstall?.(selected.id)}
              style={{ fontSize: 12.5, fontWeight: 600, color: c.bg, padding: "8px 15px", borderRadius: 8, background: c.green }}
            >
              {selected.state === "installed" ? "Reconfigure" : "Install bundle"}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
