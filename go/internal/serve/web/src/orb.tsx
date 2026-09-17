import { useEffect, useRef, useState, type ReactNode } from "react";
import type { Job, OrbDetail, PreflightCheck, OrbRemovePlan, OrbFile, OrbStatus, Project, Status } from "./types";
import { askChoice, askConfirm } from "./dialog";
import { sessionTitle } from "./render";
import { CopyButton, Pending } from "./loading";
import { STATUS, StatusMark } from "./status";
import { idTail } from "./palette";
import { OrbAddress } from "./mode";

const FILES: OrbFile[] = ["project.yml", "Dockerfile", "setup.sh", "resume.sh"];

/** Server text with `backticked` commands, the commands set as code. */
function Prose({ text }: { text: string }) {
  return <>{text.split(/`([^`]+)`/).map((part, i) => (i % 2 ? <code key={i} className="mono">{part}</code> : part))}</>;
}

/** A state mark with its word, and the reason after it when there is one. */
function Mark({ status, word, detail }: { status: Status; word: ReactNode; detail?: string }) {
  return (
    <span className="orb-mark" style={{ color: STATUS[status].tone }}>
      <svg className="state-mark" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5"
           strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">{STATUS[status].glyph}</svg>
      {word}{detail && <span className="orb-why"> · <Prose text={detail} /></span>}
    </span>
  );
}

const CHECK_TONE: Record<PreflightCheck["status"], Status> = { ok: "done", warn: "needs-you", fail: "error" };
const CHECK_KIND: Record<PreflightCheck["kind"], string> = { runtime: "Runtime", clone: "Clone", gh: "", secret: "Secret" };

/** The preflight: runtime up, each repo clones, the gh token, each secret resolves. */
function Preflight({ checks }: { checks: PreflightCheck[] }) {
  const failing = checks.filter((c) => c.status === "fail").length;
  return (
    <>
      <dt>Preflight</dt>
      <dd>
        <ul className="orb-preflight">
          {checks.map((c) => (
            <li key={c.kind + c.name}>
              <Mark status={CHECK_TONE[c.status]} detail={c.detail}
                    word={<>{CHECK_KIND[c.kind] ? <>{CHECK_KIND[c.kind]} <span className="mono">{c.name}</span></> : c.name}</>} />
            </li>
          ))}
        </ul>
        {failing > 0 && <span className="orb-hint">{failing} failing · a new session will likely fail to start</span>}
      </dd>
    </>
  );
}

/** Stop is offered while the container runs; a failed setup can leave it up. */
export const orbUp = (o?: { status: OrbStatus; up?: boolean }) => !!o && (o.up || o.status === "running");

/** The confirm Stop orb needs, or null: only running jobs make it more than a pause. */
export function stopOrbQuestion(jobs?: Job[]): { title: string; body: string } | null {
  if (!jobs?.length) return null;
  const n = jobs.length;
  const names = jobs.map((j) => j.cmd.split("\n")[0].trim()).join(", ");
  return { title: "Stop the orb?", body: `Its ${n} running ${n === 1 ? "job stops" : "jobs stop"} too: ${names}. The container and worktrees stay; the next command starts it again.` };
}

/** Asks only when jobs would die; resolves true to go ahead. */
export async function confirmStopOrb(jobs?: Job[]): Promise<boolean> {
  const q = stopOrbQuestion(jobs);
  return !q || askConfirm(q.title, q.body, { action: "Stop orb", danger: true });
}

/** The project's last image build failed and no image stands in for it: a session started now rebuilds and likely fails again. */
export const failedBuild = (p?: Project) => !!p?.orb && p.orb.build === "failed" && !p.orb.built;

/**
 * Before starting a session on a failed image: start anyway, or open the
 * project's orb to fix it. Resolves true to start; opening the orb navigates.
 */
export async function confirmFailedBuild(p?: Project): Promise<boolean> {
  if (!p || !failedBuild(p)) return true;
  const c = await askChoice(`${p.name}’s image build failed`,
    "A new session rebuilds the image first and will likely fail the same way. Fix the recipe in Projects → Orb, or start anyway.",
    ["Open orb", "Start anyway"]);
  if (c === "Open orb") location.hash = `#/projects/${p.id}/orb`;
  return c === "Start anyway";
}

export interface OrbFailureLog { phase?: string; log?: string; error: string; text: string }

const FAILED: Record<string, string> = { build: "Image build failed", setup: "Setup failed", start: "Orb failed to start" };

/**
 * Why a session's orb failed: the phase, the log lines that say why, and
 * only the fix that applies (Rebuild for a build, Edit resume.sh and Retry
 * for setup). The fix lives on the project's orb page, linked, not as CLI text.
 */
export function OrbFailureBody({ log, projectId, name, onRebuild, onRetry }: {
  log: OrbFailureLog; projectId?: string; name: string; onRebuild?: () => void; onRetry?: () => void;
}) {
  const phase = log.phase || "start";
  const [first, ...rest] = (log.error || "The orb failed without an error message.").split("\n");
  const lines = rest.filter((l) => !l.startsWith("full log: ")).map((l) => l.trim());
  const orbHref = projectId ? `#/projects/${projectId}/orb` : undefined;
  const tail = log.text ? log.text.split("\n").slice(-120).join("\n") : "";
  return <>
    <p className="orb-failure-title"><strong>{FAILED[phase] ?? FAILED.start}</strong> <span className="meta-line">{first}</span></p>
    {lines.length > 0 && <pre className="mono orb-failure-lines">{lines.join("\n")}</pre>}
    {tail && (
      <details className="orb-failure-log">
        <summary>Full {log.log || "log"}</summary>
        <pre className="mono">{tail}</pre>
      </details>
    )}
    <span className="orb-actions">
      {phase === "build" && onRebuild && <button className="btn" onClick={onRebuild}>Rebuild image</button>}
      {phase === "setup" && orbHref && <a className="btn" href={orbHref}>Edit resume.sh</a>}
      {phase === "setup" && onRetry && <button className="btn" onClick={onRetry}>Retry</button>}
      {orbHref && <a className="link" href={orbHref}>Projects → {name} → Orb</a>}
    </span>
  </>;
}

/** An orb's state in the session vocabulary, so its rows read like every other list. */
const STATE: Record<OrbStatus, Status> = {
  "": "queued", running: "running", building: "running", starting: "running",
  failed: "error", stopped: "stopped",
};

/**
 * One project's orb: the definition files, the snapshot image and the
 * containers sessions run in. Presentational; ProjectsView fetches.
 */
export function ProjectOrb({ project, detail, log, error, onAttach, onDetach, onSave, onBuild, onStopOrb, onRemoveOrb, onOpen, titles = {} }: {
  project: Project; detail?: OrbDetail; log: string;
  /** Session id to title, so a container row names the work. */
  titles?: Record<string, string>;
  /** Why the detail could not be read, when it could not. */
  error?: string;
  onAttach: () => void; onDetach: () => void;
  /** Rejects with the server's parse error, which stays beside the editor. */
  onSave: (name: OrbFile, text: string) => Promise<void>;
  onBuild: () => void; onStopOrb: (session: string) => void; onRemoveOrb?: (session: string) => void;
  onOpen?: (session: string) => void;
}) {
  const [tab, setTab] = useState<OrbFile>("project.yml");
  // Edits per file survive switching tabs; a saved file drops its draft.
  const [drafts, setDrafts] = useState<Partial<Record<OrbFile, string>>>({});
  const [saving, setSaving] = useState(false);
  const [saveErr, setSaveErr] = useState("");
  const logRef = useRef<HTMLPreElement>(null);
  useEffect(() => { if (logRef.current) logRef.current.scrollTop = logRef.current.scrollHeight; }, [log]);

  const about = <p className="proj-none orb-about">An orb is a container image for this project: its sessions run inside it, with the project's repos checked out on a branch of their own.</p>;
  if (!project.slug) {
    return (
      <div className="proj-orb">
        {about}
        <p className="proj-none">No orb yet for “{project.name}”.</p>
        <div className="orb-tabs"><button className="btn btn-primary" onClick={onAttach}>Add orb</button></div>
      </div>
    );
  }
  if (!detail) {
    return <div className="proj-orb">{error
      ? <p className="rp-state err" role="alert">Orb unavailable · {error}</p>
      : <Pending what="Orb" inline />}</div>;
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
      {about}
      <dl className="orb-kv">
        {!detail.preflight?.length && <>
          <dt>Runtime</dt>
          <dd>
            <span className="mono">{detail.runtime.name || "unknown"}</span>
            {detail.runtime.available
              ? <Mark status="done" word="Available" />
              : <Mark status="error" word="Unavailable" detail={detail.runtime.error} />}
          </dd>
        </>}
        <dt>Image</dt>
        <dd>
          <span className="mono">{detail.orb.image || "none"}</span>
          {detail.orb.error
            ? <Mark status="error" word="Failed" detail={detail.orb.error} />
            : building ? <Mark status="running" word="Building" />
            : detail.orb.built ? <Mark status="done" word="Built" />
            : detail.build.state === "failed" ? <Mark status="error" word="Build failed" detail={detail.build.error} />
            : <Mark status="idle" word="Not built" />}
        </dd>
        {!!detail.preflight?.length && <Preflight checks={detail.preflight} />}
      </dl>

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
      {saveErr && <p className="err orb-save-err" role="alert">{saveErr}</p>}
      <div className="orb-tabs">
        <button className="btn btn-primary" disabled={building || !detail.runtime.available} onClick={onBuild}
                title={detail.runtime.available ? undefined : "Start the container runtime first"}>{building ? "Building…" : "Build image"}</button>
        <button className="btn" disabled={!dirty || saving} onClick={() => { void save(); }}>{saving ? "Saving…" : "Save"}</button>
        {!detail.runtime.available && <span className="orb-hint">Start the container runtime first</span>}
        <button className="btn btn-danger-quiet orb-detach" onClick={onDetach}>Detach orb…</button>
      </div>

      {(log || detail.build.state) && (
        <details className="block" open={building || detail.build.state === "failed"}>
          <summary>
            <span className="block-label">Build log</span>
            <span className="block-detail mono">{detail.build.tag}</span>
            <span className="block-lines">{!building && detail.build.tag && detail.orb.image && detail.build.tag !== detail.orb.image
              ? `older image · ${detail.build.state}` : detail.build.state || "never built"}</span>
          </summary>
          <pre ref={logRef} className="orb-log">{log || "Nothing logged yet."}</pre>
        </details>
      )}

      {detail.orbs.length === 0
        ? <p className="proj-none">No session has run in this orb yet.</p>
        : <div className="orb-sessions">
          <div className="sel-cols orb-cols" aria-hidden="true"><span>Session</span><span className="orb-ctr">Container</span><span className="orb-st">Status</span><span className="orb-act" /></div>
          {detail.orbs.map((o) => (
          <div key={o.session} className="proj-row orb-row">
            <button className="proj-open" onClick={() => onOpen?.(o.session)}>
              <span className="proj-title">{sessionTitle({ id: o.session, title: titles[o.session] || o.title })}</span>
              {/* A fallback name is the same for every untitled session: the id tail tells them apart. */}
              {!(titles[o.session] || o.title) && <span className="mono row-id">{idTail(o.session)}</span>}
            </button>
            {/* The container name is for pasting into a terminal, not for reading. */}
            <span className="orb-ctr" title={o.container}>
              {o.container && <CopyButton text={o.container} label={idTail(o.container)} className="link proj-act mono" />}
            </span>
            <span className="orb-st" title={o.error}><StatusMark status={STATE[o.status]} /></span>
            {/* Every row keeps the action slot, so the columns line up whether or not it can stop. */}
            <span className="orb-act">{orbUp(o) ? <button className="btn btn-sm" onClick={() => onStopOrb(o.session)}>Stop orb</button>
              : onRemoveOrb && <button className="btn btn-sm btn-danger-quiet" onClick={() => onRemoveOrb(o.session)}>Remove…</button>}</span>
            {o.status === "running" && (o.ip || o.ports?.length) ? <div className="orb-note"><OrbAddress orb={o} /></div> : null}
            {o.proxyAuth === "legacy" && (
              <p className="orb-note"><Mark status="needs-you" word="Proxy unauthenticated"
                detail="this orb predates proxy tokens, so any VM on the bridge can use its host proxy and relay; remove the orb and start a session to recreate it with a token" /></p>
            )}
          </div>
          ))}
        </div>}
    </div>
  );
}

function bytes(n: number): string {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) { n /= 1024; i++; }
  return i === 0 ? `${n} B` : `${n.toFixed(1)} ${units[i]}`;
}

/** Remove orb's confirm: exactly what goes and what stays. Branches are kept here; `bough project prune --branches` deletes merged ones. */
export function removeOrbQuestion(plan: OrbRemovePlan): { title: string; body: string } {
  if (plan.dirty?.length) {
    return { title: "Uncommitted changes", body: `Commit or discard the changes first; removing would lose them:\n${plan.dirty.map((d) => `• ${d}`).join("\n")}` };
  }
  const del = [
    ...(plan.container ? [`container ${plan.container}`] : []),
    ...plan.worktrees.map((w) => `worktree ${w}`),
    `${plan.dir} (${bytes(plan.bytes)} on disk)`,
  ];
  const keep = [
    ...plan.branches.map((b) => `branch ${b.branch} in ${b.repo} (${b.reason})`),
    "the session's history",
  ];
  return { title: "Remove this orb?", body: `Deletes:\n${del.map((l) => `• ${l}`).join("\n")}\n\nKeeps:\n${keep.map((l) => `• ${l}`).join("\n")}` };
}
