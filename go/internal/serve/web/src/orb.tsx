import { useEffect, useRef, useState, type ReactNode, type RefObject } from "react";
import type { Job, OrbDetail, PreflightCheck, OrbRemovePlan, OrbFile, OrbState, OrbStatus, Project, Status } from "./types";
import { askChoice, askConfirm } from "./dialog";
import { sessionTitle } from "./render";
import { CopyButton, Elapsed, ErrorNote, Pending, ago, duration } from "./loading";
import { MARKED, STATUS, StatusMark } from "./status";
import { idTail } from "./palette";
import { OrbAddress } from "./mode";

// Same order as projectdef.EditableFiles, which is what the detail's
// files map is built from.
const FILES: OrbFile[] = ["project.yml", "Dockerfile", "setup.sh", "resume.sh", "MEMORY.md"];

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
function Preflight({ checks, onRecheck, onEdit }: { checks: PreflightCheck[]; onRecheck: () => void; onEdit: () => void }) {
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
        {/* A failing check is the reason the next session will not start, so it
            carries the two things that fix it rather than a grey aside. Re-check
            re-reads the orb, which is what recomputes preflight on the server. */}
        {failing > 0 && (
          <div className="callout err orb-pf-fix">
            <p className="orb-pf-line">{failing === 1 ? "One check fails" : `${failing} checks fail`}. A session started now will fail the same way.</p>
            <span className="orb-pf-acts">
              <button className="btn" onClick={onEdit}>Edit project.yml</button>
              <button className="btn" onClick={onRecheck}>Re-check</button>
            </span>
          </div>
        )}
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
  if (c === "Open orb") location.hash = `#/projects/${p.slug}/orb`;
  return c === "Start anyway";
}

export interface OrbFailureLog { phase?: string; log?: string; error: string; text: string }

const FAILED: Record<string, string> = { build: "Image build failed", setup: "Setup failed", start: "Orb failed to start" };

/**
 * Why a session's orb failed: the phase, the log lines that say why, and
 * only the fix that applies (Rebuild for a build, Edit resume.sh and Retry
 * for setup). The fix lives on the project's orb page, linked, not as CLI text.
 */
export function OrbFailureBody({ log, projectSlug, name, onRebuild, onRetry }: {
  log: OrbFailureLog; projectSlug?: string; name: string; onRebuild?: () => void; onRetry?: () => void;
}) {
  const phase = log.phase || "start";
  const [first, ...rest] = (log.error || "The orb failed without an error message.").split("\n");
  const lines = rest.filter((l) => !l.startsWith("full log: ")).map((l) => l.trim());
  const orbHref = projectSlug ? `#/projects/${projectSlug}/orb` : undefined;
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
export const ORB_AS_STATUS: Record<OrbStatus, Status> = {
  "": "queued", running: "running", building: "running", starting: "running",
  failed: "error", stopped: "stopped",
};

/** The one-line answer to "is this orb usable right now?", in the shared status vocabulary. */
export function orbVerdict(d: OrbDetail): { status: Status; word: string; why?: string } {
  if (!d.runtime.available) return { status: "error", word: "Runtime not running", why: d.runtime.error };
  if (d.orb.error) return { status: "error", word: "Orb failed", why: d.orb.error };
  if (d.build.state === "building") return { status: "running", word: "Building image" };
  if (d.build.state === "failed") return d.orb.built
    ? { status: "needs-you", word: "Ready · last build failed", why: d.build.error }
    : { status: "error", word: "Build failed", why: d.build.error };
  if (!d.orb.built) return { status: "idle", word: "No image yet" };
  return { status: "done", word: "Ready" };
}

/**
 * The verdict's word. `Mark` always draws its glyph, but a resting state does
 * not wear one: a grey tick on "Ready" and a grey dot on "No image yet" mark
 * nothing. The fix stays here rather than in `Mark`, whose other callers —
 * the preflight list and the Runtime row — still want their marks.
 */
function VerdictMark({ status, word }: { status: Status; word: string }) {
  return MARKED.has(status) ? <Mark status={status} word={word} /> : <span className="orb-mark">{word}</span>;
}

function Fact({ label, children }: { label: string; children: ReactNode }) {
  return <div className="orb-fact"><span className="eyebrow">{label}</span><span className="orb-fact-v">{children}</span></div>;
}

/**
 * The fact strip: the verdict, when the image it would use was built, and who
 * is in it. Three facts, not a dashboard — the image tag is plumbing nobody
 * types, so it lives in the Built tile's title.
 */
function OrbVerdict({ detail }: { detail: OrbDetail }) {
  const v = orbVerdict(detail);
  const b = detail.build;
  // "Up" is one predicate across the UI; this is the same one the roll-up counts with.
  const running = detail.orbs.filter((o) => orbUp(o)).length;
  const total = detail.orbs.length;
  const start = Date.parse(b.startedAt || "");
  const end = Date.parse(b.endedAt || "");
  return (
    <div className="orb-verdict">
      <Fact label="Orb"><VerdictMark status={v.status} word={v.word} /></Fact>
      <Fact label="Built">
        <span title={detail.orb.image || undefined}>
          {b.state === "building" && b.startedAt && Number.isFinite(start)
            ? <Elapsed since={b.startedAt} />
            : Number.isFinite(start) && Number.isFinite(end)
              ? <span className="num">{duration(end - start)} <span className="orb-fact-sub">· {ago(b.endedAt!)} ago</span></span>
              : <span className="orb-fact-sub">Never</span>}
        </span>
      </Fact>
      <Fact label="Sessions">
        {total === 0 ? <span className="orb-fact-sub">None yet</span>
          : running > 0 ? <span className="num">{running} up <span className="orb-fact-sub">· {total} total</span></span>
          : <span className="num orb-fact-sub">{total} total, none up</span>}
      </Fact>
    </div>
  );
}

/**
 * What an empty pane says. project.yml is required, MEMORY.md is a file
 * that may legitimately be empty (an empty save writes an empty file),
 * and the scripts are removed by saving nothing.
 */
export function filePlaceholder(f: OrbFile): string {
  if (f === "project.yml") return "";
  if (f === "MEMORY.md") return "Empty. Write what every session in this project should know: what it is, where things live, decisions already made.";
  return "Empty: saving removes the file";
}

/**
 * The definition files, a tab per file over one pane, with the drafts and
 * the Save that both pages share.
 *
 * `order` is a prop because the two callers lead with different files:
 * the orb panel keeps projectdef.EditableFiles' order, the project page
 * puts MEMORY.md first. The tab is controlled so a caller can point at
 * the file it wants fixed (the preflight's "Edit project.yml").
 */
export function FileEditor({ order, files, tab, onTab, onSave, editorRef, meta, note, actions, announce }: {
  order: readonly OrbFile[];
  files: Partial<Record<OrbFile, string>>;
  tab: OrbFile; onTab: (f: OrbFile) => void;
  /** Rejects with the server's parse error, which stays beside the editor. */
  onSave: (name: OrbFile, text: string) => Promise<void>;
  editorRef?: RefObject<HTMLTextAreaElement | null>;
  /** The pane's header line, e.g. "MEMORY.md · 84 lines". */
  meta?: (f: OrbFile, text: string) => ReactNode;
  /** One line under the tabs that stays whatever is open. */
  note?: ReactNode;
  /** Buttons that sit before Save. */
  actions?: ReactNode;
  /** Say so when a save lands; the orb panel's Build log already does. */
  announce?: boolean;
}) {
  // Edits per file survive switching tabs; a saved file drops its draft.
  const [drafts, setDrafts] = useState<Partial<Record<OrbFile, string>>>({});
  const [saving, setSaving] = useState(false);
  const [saveErr, setSaveErr] = useState("");
  const [said, setSaid] = useState("");
  const saved = files[tab] ?? "";
  const text = drafts[tab] ?? saved;
  const dirty = text !== saved;
  useEffect(() => {
    if (!said) return;
    const t = window.setTimeout(() => setSaid(""), 2600);
    return () => clearTimeout(t);
  }, [said]);
  const save = async () => {
    setSaving(true); setSaveErr("");
    try {
      await onSave(tab, text);
      setDrafts((d) => { const n = { ...d }; delete n[tab]; return n; });
      if (announce) setSaid(`${tab} saved.`);
    } catch (e) { setSaveErr(e instanceof Error ? e.message : String(e)); }
    finally { setSaving(false); }
  };
  return (
    <>
      <div className="orb-tabs" role="tablist" aria-label="Definition files">
        {order.map((f) => (
          <button key={f} role="tab" aria-selected={tab === f} className={"btn mono" + (tab === f ? " btn-primary" : "")}
                  onClick={() => { onTab(f); setSaveErr(""); }}>
            {f}{drafts[f] !== undefined && drafts[f] !== (files[f] ?? "") ? " \u2022" : ""}
          </button>
        ))}
      </div>
      {note && <p className="file-note">{note}</p>}
      {meta?.(tab, text)}
      <textarea ref={editorRef} className="field mono orb-editor" aria-label={tab} spellCheck={false} value={text}
                placeholder={filePlaceholder(tab)}
                onChange={(e) => setDrafts((d) => ({ ...d, [tab]: e.target.value }))} />
      {saveErr && <p className="err orb-save-err" role="alert">{saveErr}</p>}
      <div className="orb-tabs">
        {actions}
        <button className="btn" disabled={!dirty || saving} onClick={() => { void save(); }}>{saving ? "Saving\u2026" : "Save"}</button>
      </div>
      {said && <p className="file-said" role="status">{said}</p>}
    </>
  );
}

/**
 * Every container recorded for a project: one row per session, with the
 * one action that row can take. Shared by the orb panel and the project
 * page's thread orbs, so a container reads the same wherever it is seen.
 */
export function OrbSessions({ orbs, titles = {}, onOpen, onStopOrb, onRemoveOrb, none = "No session has run in this orb yet." }: {
  orbs: OrbState[];
  /** Session id to title, so a container row names the work. */
  titles?: Record<string, string>;
  onOpen?: (session: string) => void;
  onStopOrb: (session: string) => void;
  onRemoveOrb?: (session: string) => void;
  /** What stands in for an empty table. */
  none?: ReactNode;
}) {
  if (orbs.length === 0) return <p className="proj-none">{none}</p>;
  return (
    <div className="orb-sessions">
      <div className="sel-cols orb-cols" aria-hidden="true"><span>Session</span><span className="orb-ctr">Container</span><span className="orb-st">Status</span><span className="orb-act" /></div>
      {orbs.map((o) => (
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
        <span className="orb-st" title={o.error}><StatusMark status={ORB_AS_STATUS[o.status]} /></span>
        {/* Every row keeps the action slot, so the columns line up whether or not it can stop. */}
        <span className="orb-act">{orbUp(o) ? <button className="btn btn-sm" onClick={() => onStopOrb(o.session)}>Stop orb</button>
          : onRemoveOrb && <button className="btn btn-sm btn-danger-quiet" onClick={() => onRemoveOrb(o.session)}>Remove…</button>}</span>
        {o.status === "running" && (o.ip || o.ports?.length) ? <div className="orb-note"><OrbAddress orb={o} /></div> : null}
        {/* A long warning is a tinted callout, not coloured prose. The fix is
            the row's own Remove… beside it, so this does not repeat the button. */}
        {o.proxyAuth === "legacy" && (
          <div className="orb-note callout warn orb-legacy">
            <p className="orb-legacy-line"><b>Proxy unauthenticated.</b> Any VM on the bridge can use this orb’s host proxy. Removing it and starting a session recreates it with a token.</p>
          </div>
        )}
      </div>
      ))}
    </div>
  );
}

/**
 * One project's orb: the definition files, the snapshot image and the
 * containers sessions run in. Presentational; ProjectsView fetches.
 */
export function ProjectOrb({ project, detail, log, error, onSave, onBuild, onStopOrb, onRemoveOrb, onOpen, onRetry, titles = {} }: {
  project: Project; detail?: OrbDetail; log: string;
  /** Session id to title, so a container row names the work. */
  titles?: Record<string, string>;
  /** Why the detail could not be read, when it could not. */
  error?: string;
  /** Rejects with the server's parse error, which stays beside the editor. */
  onSave: (name: OrbFile, text: string) => Promise<void>;
  onBuild: () => void; onStopOrb: (session: string) => void; onRemoveOrb?: (session: string) => void;
  onOpen?: (session: string) => void;
  /** Re-read the orb: the Retry of a failed read, and what recomputes preflight. */
  onRetry: () => void;
}) {
  const [tab, setTab] = useState<OrbFile>("project.yml");
  const logRef = useRef<HTMLPreElement>(null);
  const editorRef = useRef<HTMLTextAreaElement>(null);
  useEffect(() => { if (logRef.current) logRef.current.scrollTop = logRef.current.scrollHeight; }, [log]);

  const about = <p className="proj-none orb-about">An orb is a container image for this project: its sessions run inside it, with the project's repos checked out on a branch of their own.</p>;
  if (!detail) {
    return <div className="proj-orb">{error
      ? <ErrorNote title="Couldn’t read this orb" err={error}
          action={{ label: "Retry", onClick: onRetry }} className="orb-err" />
      : <Pending what="Orb" inline />}</div>;
  }

  const building = detail.build.state === "building";
  // The build log opens itself while a build runs and when one fails,
  // but `open` alone fought the user: this page re-renders on every poll,
  // and each render forced a closed log back open. It opens on the
  // change of state, and a close sticks until the state changes again.
  const [logOpen, setLogOpen] = useState(building || detail.build.state === "failed");
  const lastBuild = useRef(detail.build.state);
  useEffect(() => {
    if (detail.build.state === lastBuild.current) return;
    lastBuild.current = detail.build.state;
    if (detail.build.state === "building" || detail.build.state === "failed") setLogOpen(true);
  }, [detail.build.state]);
  const v = orbVerdict(detail);
  // Only a red or amber verdict has a reason worth the room; Ready says it all.
  const verdictFail = v.status === "error" || v.status === "needs-you";

  return (
    <div className="proj-orb">
      {/* The verdict first: whether a session started now would work, when the
          image it would use was built, and who is already inside. */}
      <OrbVerdict detail={detail} />
      {verdictFail && <p className="callout err orb-verdict-why" role="alert">{v.word}{v.why ? <> · <Prose text={v.why} /></> : null}</p>}

      {/* Sessions before the recipe: the containers are what the page is
          usually opened to check, and the recipe is what it is opened to edit. */}
      <section className="orb-sec">
        <h3 className="orb-sec-h">Sessions</h3>
        <OrbSessions orbs={detail.orbs} titles={titles} onOpen={onOpen} onStopOrb={onStopOrb} onRemoveOrb={onRemoveOrb} />
      </section>

      <section className="orb-sec">
        <h3 className="orb-sec-h">Recipe</h3>
        {about}
        <dl className="orb-kv">
          <dt>Runtime</dt>
          <dd>
            <span className="mono">{detail.runtime.name || "unknown"}</span>
            {detail.runtime.available
              ? <Mark status="done" word="Available" />
              : <Mark status="error" word="Unavailable" detail={detail.runtime.error} />}
          </dd>
          {!!detail.preflight?.length && <Preflight checks={detail.preflight} onRecheck={onRetry} onEdit={() => {
            // Setting the tab alone was a no-op whenever project.yml was
            // already the open one, which is every fresh render: the
            // button promised a fix and visibly did nothing. Put the
            // cursor where the fix gets typed.
            setTab("project.yml");
            requestAnimationFrame(() => {
              const el = editorRef.current;
              if (!el) return;
              el.scrollIntoView({ block: "nearest" });
              el.focus();
            });
          }} />}
        </dl>

        <FileEditor order={FILES} files={detail.files} tab={tab} onTab={setTab} onSave={onSave} editorRef={editorRef}
                    actions={<>
                      <button className="btn btn-primary" disabled={building || !detail.runtime.available} onClick={onBuild}
                              title={detail.runtime.available ? undefined : "Start the container runtime first"}>{building ? "Building…" : "Build image"}</button>
                    </>} />
        {!detail.runtime.available && <div className="orb-tabs"><span className="orb-hint">Start the container runtime first</span></div>}

        {(log || detail.build.state) && (
          <details className="block" open={logOpen} onToggle={(e) => setLogOpen(e.currentTarget.open)}>
            <summary>
              <span className="block-label">Build log</span>
              <span className="block-detail mono">{detail.build.tag}</span>
              <span className="block-lines">{!building && detail.build.tag && detail.orb.image && detail.build.tag !== detail.orb.image
                ? `older image · ${detail.build.state}` : detail.build.state || "never built"}</span>
            </summary>
            <pre ref={logRef} className="orb-log">{log || "Nothing logged yet."}</pre>
          </details>
        )}
      </section>
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
