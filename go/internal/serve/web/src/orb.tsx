import { useEffect, useRef, useState } from "react";
import type { OrbDetail, OrbFile, OrbStatus, Project } from "./types";

const FILES: OrbFile[] = ["project.yml", "Dockerfile", "setup.sh", "resume.sh"];

const TONE: Record<OrbStatus, string> = {
  "": "mode-stopped", running: "mode-running", building: "mode-busy", starting: "mode-busy",
  failed: "mode-failed", stopped: "mode-stopped",
};

/**
 * One project's orb: the definition files, the snapshot image and the
 * containers sessions run in. Presentational; ProjectsView fetches.
 */
export function ProjectOrb({ project, detail, log, error, onAttach, onDetach, onSave, onBuild, onStopOrb, onOpen }: {
  project: Project; detail?: OrbDetail; log: string;
  /** Why the detail could not be read, when it could not. */
  error?: string;
  onAttach: () => void; onDetach: () => void;
  /** Rejects with the server's parse error, which stays beside the editor. */
  onSave: (name: OrbFile, text: string) => Promise<void>;
  onBuild: () => void; onStopOrb: (session: string) => void;
  onOpen?: (session: string) => void;
}) {
  const [tab, setTab] = useState<OrbFile>("project.yml");
  // Edits per file survive switching tabs; a saved file drops its draft.
  const [drafts, setDrafts] = useState<Partial<Record<OrbFile, string>>>({});
  const [saving, setSaving] = useState(false);
  const [saveErr, setSaveErr] = useState("");
  const logRef = useRef<HTMLPreElement>(null);
  useEffect(() => { if (logRef.current) logRef.current.scrollTop = logRef.current.scrollHeight; }, [log]);

  if (!project.slug) {
    return (
      <div className="proj-orb">
        <p className="proj-none">No orb. Add one to run project sessions for “{project.name}” in a container that can change files.</p>
        <div className="orb-tabs"><button className="btn btn-primary" onClick={onAttach}>Add orb</button></div>
      </div>
    );
  }
  if (!detail) {
    return <div className="proj-orb"><p className="rp-state" role="status">{error ? `Orb unavailable · ${error}` : "Reading orb…"}</p></div>;
  }

  const saved = detail.files[tab] ?? "";
  const text = drafts[tab] ?? saved;
  const dirty = text !== saved;
  const building = detail.build.state === "building";
  const save = async () => {
    setSaving(true); setSaveErr("");
    try {
      await onSave(tab, text);
      setDrafts((d) => { const n = { ...d }; delete n[tab]; return n; });
    } catch (e) { setSaveErr(e instanceof Error ? e.message : String(e)); }
    finally { setSaving(false); }
  };

  return (
    <div className="proj-orb">
      <p className="orb-line">
        <span className="mono">{detail.runtime.name || "runtime"}</span>{" "}
        {detail.runtime.available
          ? <span className="mode-running">available</span>
          : <span className="mode-failed">unavailable{detail.runtime.error ? ` · ${detail.runtime.error}` : ""}</span>}
      </p>
      <p className="orb-line">
        <span className="mono">{detail.orb.image || "no image"}</span>{" "}
        {detail.orb.error
          ? <span className="mode-failed">{detail.orb.error}</span>
          : building ? <span className="mode-busy">building</span>
          : detail.orb.built ? <span className="mode-running">built</span>
          : detail.build.state === "failed" ? <span className="mode-failed">build failed{detail.build.error ? ` · ${detail.build.error}` : ""}</span>
          : <span className="mode-stopped">not built</span>}
      </p>

      <div className="orb-tabs" role="tablist" aria-label="Definition files">
        {FILES.map((f) => (
          <button key={f} role="tab" aria-selected={tab === f} className={"btn mono" + (tab === f ? " btn-primary" : "")}
                  onClick={() => { setTab(f); setSaveErr(""); }}>
            {f}{drafts[f] !== undefined && drafts[f] !== (detail.files[f] ?? "") ? " •" : ""}
          </button>
        ))}
      </div>
      <textarea className="field mono orb-editor" aria-label={tab} spellCheck={false} value={text}
                placeholder={tab === "project.yml" ? "" : "Empty: saving removes the file"}
                onChange={(e) => setDrafts((d) => ({ ...d, [tab]: e.target.value }))} />
      {saveErr && <p className="err" role="alert">{saveErr}</p>}
      <div className="orb-tabs">
        <button className="btn btn-primary" disabled={!dirty || saving} onClick={() => { void save(); }}>{saving ? "Saving…" : "Save"}</button>
        <button className="btn" disabled={building} onClick={onBuild}>{building ? "Building…" : "Build image"}</button>
        <button className="link orb-detach" onClick={onDetach}>Detach orb</button>
      </div>

      {(log || detail.build.state) && (
        <details className="block" open={building || detail.build.state === "failed"}>
          <summary>
            <span className="block-label">Build log</span>
            <span className="block-detail mono">{detail.build.tag}</span>
            <span className="block-lines">{detail.build.state || "never built"}</span>
          </summary>
          <pre ref={logRef} className="orb-log">{log || "Nothing logged yet."}</pre>
        </details>
      )}

      {detail.orbs.length === 0
        ? <p className="proj-none">No session has run in this orb yet.</p>
        : detail.orbs.map((o) => (
          <div key={o.session} className="proj-row">
            <button className="proj-open" onClick={() => onOpen?.(o.session)}>
              <span className="proj-title mono">{o.session.slice(-6)}</span>
              <span className="num proj-when">{o.container ?? ""}</span>
            </button>
            <span className={"status mono mode-chip " + TONE[o.status]} title={o.error}>{o.status || "pending"}</span>
            {o.status === "running" && <button className="btn" onClick={() => onStopOrb(o.session)}>Stop orb</button>}
          </div>
        ))}
    </div>
  );
}
