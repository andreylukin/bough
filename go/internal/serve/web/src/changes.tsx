import { useEffect, useRef, useState } from "react";
import { api, type Change, type Scope } from "./api";
import { Back } from "./app";
import { changedPath, sessionTitle } from "./render";
import { CopyCommand } from "./work-ui";
import { Pending } from "./loading";
import type { Row } from "./types";

// Two scopes, always named: what this session's turns changed (the
// default, from its checkpoints) and the working tree (everything
// uncommitted, whoever did it). Pre-existing dirt is never the agent's.

const clock = (ms: number) => new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });

interface Read { files: (Change & { patch?: boolean })[] | null; repo: boolean; failed: boolean; at?: number }
const empty: Read = { files: null, repo: true, failed: false };

/** Both scopes, re-read when the transcript grows; the tree on a timer too, since a hand edit leaves no entry. */
export function useChanges(id: string, tick: number) {
  const [session, setSession] = useState<Read>(empty);
  const [tree, setTree] = useState<Read>(empty);
  const [nonce, setNonce] = useState(0);
  useEffect(() => {
    let live = true;
    api.edits(id).then((r) => { if (live) setSession({ files: r.files, repo: r.repo, failed: false, at: Date.now() }); })
      .catch(() => { if (live) setSession((s) => ({ ...s, failed: true })); });
    const read = () => api.changes(id)
      .then((r) => { if (live) setTree({ files: r.files, repo: r.repo, failed: false, at: Date.now() }); })
      .catch(() => { if (live) setTree((s) => ({ ...s, failed: true })); });
    read();
    const t = setInterval(read, 10_000);
    return () => { live = false; clearInterval(t); };
  }, [id, tick, nonce]);
  return { session, tree, retry: () => setNonce((n) => n + 1) };
}

// A count below zero is unknown (binary, or no patch): it adds nothing.
const sum = (files: Change[] | null) => ({
  add: (files ?? []).reduce((n, f) => n + Math.max(0, f.add), 0),
  del: (files ?? []).reduce((n, f) => n + Math.max(0, f.del), 0),
});

/** "2 min ago", for when the list was read. */
const ago = (ms: number) => {
  const m = Math.round((Date.now() - ms) / 60_000);
  return m < 1 ? "just now" : m < 60 ? `${m} min ago` : clock(ms);
};

/** "3 files · +12 −4", or the state instead of a count. */
export function countOf(r: Read): { text: string; add?: number; del?: number; quiet: boolean } {
  if (r.files === null) return { text: r.failed ? "Unavailable" : "Reading…", quiet: true };
  if (!r.repo) return { text: "—", quiet: true };
  if (!r.files.length) return { text: "None", quiet: true };
  const s = sum(r.files);
  const text = `${r.files.length} ${r.files.length === 1 ? "file" : "files"}`;
  // No line counts known (every patch unrecorded or binary): no "+0 −0".
  return s.add || s.del ? { text, ...s, quiet: false } : { text, quiet: false };
}

export const scopeName = (s: Scope) => (s === "session" ? "Session edits" : "Working tree");

/** The ?file=<path> of a #/s/<id>/changes?file=… link, if any. */
export function hashFile(): string | null {
  const m = /[?&]file=([^&]*)/.exec(typeof window === "undefined" ? "" : window.location.hash);
  if (!m) return null;
  try { return decodeURIComponent(m[1]); } catch { return m[1]; }
}

export interface DiffLine { kind: "add" | "del" | "ctx" | "hunk" | "meta"; text: string; old?: number; new?: number }

/** A unified patch, line by line, numbered from its @@ headers. */
export function parseDiff(text: string): DiffLine[] {
  let o = 0, n = 0, inHunk = false;
  const lines = text.endsWith("\n") ? text.slice(0, -1).split("\n") : text.split("\n");
  return lines.map((l): DiffLine => {
    const h = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(l);
    if (h) { o = +h[1]; n = +h[2]; inHunk = true; return { kind: "hunk", text: l }; }
    if (!inHunk) return { kind: "meta", text: "" };
    if (l.startsWith("+")) return { kind: "add", text: l, new: n++ };
    if (l.startsWith("-")) return { kind: "del", text: l, old: o++ };
    if (l.startsWith("\\")) return { kind: "meta", text: l };
    return { kind: "ctx", text: l, old: o++, new: n++ };
  }).filter((l) => l.kind !== "meta" || l.text !== "");
}

const lineClass = { add: " dl-add rt-add", del: " dl-del rt-del", ctx: " dl-ctx", hunk: " dl-hunk rt-label", meta: " dl-meta rt-label" };

/** One patch: old/new gutters, tinted added and removed lines, hunk bars. */
export function DiffBody({ text }: { text: string }) {
  return (
    <pre className="mono rt-diff-body dl-body">{parseDiff(text).map((l, i) => (
      <span key={i} className={"dl" + lineClass[l.kind]}>
        <span className="num dl-n" aria-hidden>{l.old ?? ""}</span>
        <span className="num dl-n" aria-hidden>{l.new ?? ""}</span>
        <span className="dl-t">{l.text + "\n"}</span>
      </span>
    ))}</pre>
  );
}

/** Why a session edit has no patch; the Working tree tab still diffs it against HEAD. */
const NO_DIFF = "Diff unavailable: this session has no checkpoint.";

const countBadge = (f: Change & { patch?: boolean }) =>
  f.patch === false ? <span className="rt-label">No diff</span>
    : f.new ? <><span className="chg-badge">new</span>{f.add > 0 && <span className="rt-add">+{f.add}</span>}</>
    : f.add < 0 ? <span className="chg-badge">binary</span>
    : <><span className="rt-add">+{f.add}</span> <span className="rt-del">−{f.del}</span></>;

/** Every file as a card; a card fetches its patch the first time it opens. */
function FileCards({ row, files, scope, at, open }: {
  row: Row; files: (Change & { patch?: boolean })[]; scope: Scope; at?: number; open: string | null;
}) {
  const [q, setQ] = useState("");
  const s = sum(files);
  const shown = q ? files.filter((f) => f.path.toLowerCase().includes(q.toLowerCase())) : files;
  return (
    <>
      <div className="chg-summary">
        <p className="rt-label">Showing <span className="num">{files.length}</span> changed {files.length === 1 ? "file" : "files"} with{" "}
          <span className="rt-add num">+{s.add}</span> additions and <span className="rt-del num">−{s.del}</span> deletions</p>
        {files.length > 8 && (
          <input className="chg-filter" type="search" placeholder="Filter paths" aria-label="Filter changed files"
                 value={q} onChange={(e) => setQ(e.target.value)} />
        )}
      </div>
      {q && !shown.length && <p className="rt-label">No changed file matches “{q}”</p>}
      <div className="chg-cards">
        {shown.map((f) => <FileCard key={scope + f.path} row={row} file={f} scope={scope} at={at}
                                    first={open === f.path || (files.length === 1 && !open)} />)}
      </div>
    </>
  );
}

function FileCard({ row, file, scope, at, first }: { row: Row; file: Change & { patch?: boolean }; scope: Scope; at?: number; first: boolean }) {
  const [open, setOpen] = useState(first);
  const [diff, setDiff] = useState<{ text: string | null; failed?: boolean }>({ text: null });
  const [nonce, setNonce] = useState(0);
  const ref = useRef<HTMLDetailsElement>(null);
  const shown = changedPath(file.path, row.cwd);
  const cut = shown.lastIndexOf("/");
  const canDiff = file.patch !== false && file.add >= 0;
  useEffect(() => { if (first) requestAnimationFrame(() => ref.current?.scrollIntoView({ block: "start" })); }, []);
  useEffect(() => {
    if (!open || !canDiff) return;
    let live = true;
    setDiff((d) => (d.text === null ? { text: null } : d));
    api.diff(row.id, file.path, scope).then((text) => { if (live) setDiff({ text }); },
      () => { if (live) setDiff({ text: null, failed: true }); });
    return () => { live = false; };
  }, [open, row.id, file.path, scope, at, nonce, canDiff]);
  return (
    <details ref={ref} className="chg-card" open={open} onToggle={(e) => setOpen(e.currentTarget.open)}>
      <summary className="chg-card-head" title={file.path}>
        <span className="chg-card-path mono">
          {cut >= 0 && <span className="chg-card-dir">{shown.slice(0, cut + 1)}</span>}
          <b>{shown.slice(cut + 1)}</b>
        </span>
        <span className="num chg-card-count">{countBadge(file)}</span>
        <span onClick={(e) => e.stopPropagation()}><CopyCommand text={file.path} label="Copy path" /></span>
      </summary>
      {open && (file.patch === false ? <p className="rt-label chg-card-note">{NO_DIFF} See Working tree.</p>
        : !canDiff ? <p className="rt-label chg-card-note">Binary file: no text diff to show</p>
        : diff.text != null ? <DiffBody text={diff.text} />
        : diff.failed ? <p className="chg-card-note"><button className="btn rt-stop" onClick={() => setNonce((n) => n + 1)}>Couldn’t read the diff · Retry</button></p>
        : <div className="chg-card-note"><Pending what="Diff" inline onRetry={() => setNonce((n) => n + 1)} /></div>)}
    </details>
  );
}

/**
 * The scope switch, the file list and one file's patch: in the header's
 * popover on a desktop and as a full page (#/s/<id>/changes) anywhere.
 * With a single file the patch is shown at once.
 */
export function ChangesBody({ row, data, scope, onScope, cards }: {
  row: Row; data: ReturnType<typeof useChanges>; scope: Scope; onScope: (s: Scope) => void;
  /** The full page: every file as a card with its own lazy diff. */
  cards?: boolean;
}) {
  const r = scope === "session" ? data.session : data.tree;
  const [pick, setPick] = useState<string | null>(hashFile);
  const [diff, setDiff] = useState<{ path: string; text: string | null; failed?: boolean } | null>(null);
  const files = r.files ?? [];
  const only = files.length === 1 && files[0].patch !== false ? files[0].path : null;
  const path = pick ?? only;
  const file = files.find((f) => f.path === path);
  const first = useRef(true);
  useEffect(() => { if (first.current) { first.current = false; return; } setPick(null); }, [scope]);
  useEffect(() => {
    if (cards || !path || file?.patch === false) { setDiff(null); return; }
    let live = true;
    setDiff({ path, text: null });
    api.diff(row.id, path, scope).then((text) => { if (live) setDiff({ path, text }); },
      () => { if (live) setDiff({ path, text: null, failed: true }); });
    return () => { live = false; };
    // A re-read of the same file set re-fetches its patch too.
  }, [row.id, path, scope, r.at, cards]);

  return (
    <div className="chg">
      <div className="chg-scopes" role="tablist" aria-label="Changes scope">
        {(["session", "tree"] as Scope[]).map((s) => {
          const c = countOf(s === "session" ? data.session : data.tree);
          return (
            <button key={s} type="button" role="tab" aria-selected={scope === s} className="chg-scope" onClick={() => onScope(s)}>
              {scopeName(s)}: <span className="num">{c.text}{c.add !== undefined && <> · <span className="rt-add">+{c.add}</span> <span className="rt-del">−{c.del}</span></>}</span>
            </button>
          );
        })}
      </div>
      <p className="rt-label chg-where">
        <span className="mono" title={row.cwd}>{row.cwd}</span>
        {r.repo && row.branch && <>{" · "}<span className="mono">{row.branch}</span></>}
        {scope === "tree" && " · everything uncommitted, including edits made outside this session"}
        {r.at ? ` · updated ${ago(r.at)}` : ""}
      </p>
      {r.failed && (
        <p className="rt-label">{r.files === null ? "Couldn’t read the changes" : "Stale: the last refresh failed"}{" "}
          <button className="btn rt-stop" onClick={data.retry}>Retry</button></p>
      )}
      {r.files !== null && !r.repo && <p className="rt-label">This folder is not a Git repository, so there are no changes to show.</p>}
      {r.files !== null && r.repo && !files.length && (
        <p className="rt-label">{scope === "session" ? "This session has not changed any files" : "No uncommitted changes"}</p>
      )}
      {scope === "session" && files.some((f) => f.patch === false) && (
        <p className="rt-label chg-nodiff">{NO_DIFF}{" "}
          <button type="button" className="rt-link" onClick={() => onScope("tree")}>See Working tree</button></p>
      )}
      {cards && files.length > 0 && <FileCards row={row} files={files} scope={scope} at={r.at} open={pick} />}
      {!cards && files.length > (only ? 1 : 0) && (
        <ul className="chg-files">
          {files.map((f) => (
            <li key={f.path}>
              <button className={"rt-link rt-file" + (f.path === path ? " chg-on" : "")} title={f.patch === false ? `${f.path} · ${NO_DIFF} See Working tree.` : f.path}
                      aria-current={f.path === path || undefined} disabled={f.patch === false}
                      onClick={() => {
                        setPick(f.path === pick ? null : f.path);
                        // The diff sits under the list: bring it into view.
                        requestAnimationFrame(() => document.querySelector(".chg-diff")?.scrollIntoView({ block: "nearest", behavior: "smooth" }));
                      }}>
                <span className="mono rt-job-cmd">{f.path}</span>
                <span className="num">
                  {f.patch === false ? <span className="rt-label">No diff</span>
                    : f.new ? <span className="rt-add">new{f.add > 0 ? ` · +${f.add}` : ""}</span>
                    : f.add < 0 ? <span className="rt-label">binary</span>
                    : <><span className="rt-add">+{f.add}</span> <span className="rt-del">−{f.del}</span></>}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
      {!cards && path && file && (
        <div className="chg-diff">
          <div className="chg-diff-head">
            <span className="mono rt-job-cmd" title={path}>{path}</span>
            <CopyCommand text={path} label="Copy path" />
          </div>
          {file.patch === false ? <p className="rt-label">{NO_DIFF} See Working tree.</p>
            : diff?.text != null ? (
              <DiffBody text={diff.text} />
            )
            : diff?.failed ? <button className="btn rt-stop" onClick={data.retry}>Couldn’t read the diff · Retry</button>
            : <Pending what="Diff" inline onRetry={data.retry} />}
        </div>
      )}
    </div>
  );
}

/** The full-width review: #/s/<id>/changes. */
export function ChangesPage({ row, tick, onBack }: { row: Row; tick: number; onBack: () => void }) {
  const data = useChanges(row.id, tick);
  const [scope, setScope] = useState<Scope>("session");
  // Another session opens on its own edits, not the last one's tab.
  useEffect(() => { setScope("session"); }, [row.id]);
  return (
    <div className="thread">
      <header className="thread-head page-head">
        <Back onBack={onBack} />
        <div className="head-main chg-head"><h1>Changes</h1><span className="chg-session">{sessionTitle(row)}</span></div>
      </header>
      <div className="scroll proj-body chg-page">
        <ChangesBody row={row} data={data} scope={scope} onScope={setScope} cards />
      </div>
    </div>
  );
}
