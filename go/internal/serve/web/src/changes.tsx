import { useEffect, useState } from "react";
import { api, type Change, type Scope } from "./api";
import { Back } from "./app";
import { sessionTitle } from "./render";
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

const sum = (files: Change[] | null) => ({
  add: (files ?? []).reduce((n, f) => n + Math.max(0, f.add), 0),
  del: (files ?? []).reduce((n, f) => n + Math.max(0, f.del), 0),
});

/** "3 files · +12 −4", or the state instead of a count. */
export function countOf(r: Read): { text: string; add?: number; del?: number; quiet: boolean } {
  if (r.files === null) return { text: r.failed ? "Unavailable" : "Reading…", quiet: true };
  if (!r.repo) return { text: "—", quiet: true };
  if (!r.files.length) return { text: "None", quiet: true };
  return { text: `${r.files.length} ${r.files.length === 1 ? "file" : "files"}`, ...sum(r.files), quiet: false };
}

export const scopeName = (s: Scope) => (s === "session" ? "Session edits" : "Working tree");

/**
 * The scope switch, the file list and one file's patch: in the header's
 * popover on a desktop and as a full page (#/s/<id>/changes) anywhere.
 * With a single file the patch is shown at once.
 */
export function ChangesBody({ row, data, scope, onScope }: {
  row: Row; data: ReturnType<typeof useChanges>; scope: Scope; onScope: (s: Scope) => void;
}) {
  const r = scope === "session" ? data.session : data.tree;
  const [pick, setPick] = useState<string | null>(null);
  const [diff, setDiff] = useState<{ path: string; text: string | null; failed?: boolean } | null>(null);
  const files = r.files ?? [];
  const only = files.length === 1 && files[0].patch !== false ? files[0].path : null;
  const path = pick ?? only;
  const file = files.find((f) => f.path === path);
  useEffect(() => { setPick(null); }, [scope]);
  useEffect(() => {
    if (!path || file?.patch === false) { setDiff(null); return; }
    let live = true;
    setDiff({ path, text: null });
    api.diff(row.id, path, scope).then((text) => { if (live) setDiff({ path, text }); },
      () => { if (live) setDiff({ path, text: null, failed: true }); });
    return () => { live = false; };
    // A re-read of the same file set re-fetches its patch too.
  }, [row.id, path, scope, r.at]);

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
        {" · "}{row.branch ? <>Current checkout: <span className="mono">{row.branch}</span></> : "Branch unknown"}
        {" · "}{scope === "session" ? "files this session’s turns changed, from its first checkpoint" : "everything uncommitted, including edits made before or outside this session"}
        {r.at ? ` · read ${clock(r.at)}` : ""}
      </p>
      {r.failed && (
        <p className="rt-label">{r.files === null ? "Couldn’t read the changes" : "Stale: the last refresh failed"}{" "}
          <button className="btn rt-stop" onClick={data.retry}>Retry</button></p>
      )}
      {r.files !== null && !r.repo && <p className="rt-label">This folder is not a Git repository, so there are no changes to show.</p>}
      {r.files !== null && r.repo && !files.length && (
        <p className="rt-label">{scope === "session" ? "This session has not changed any files" : "No uncommitted changes"}</p>
      )}
      {files.length > (only ? 1 : 0) && (
        <ul className="chg-files">
          {files.map((f) => (
            <li key={f.path}>
              <button className={"rt-link rt-file" + (f.path === path ? " chg-on" : "")} title={f.path} aria-current={f.path === path || undefined}
                      onClick={() => setPick(f.path === pick ? null : f.path)}>
                <span className="mono rt-job-cmd">{f.path}</span>
                <span className="num">
                  {f.patch === false ? <span className="rt-label">Patch not recorded</span>
                    : f.new ? <span className="rt-add">new</span>
                    : f.add < 0 ? <span className="rt-label">binary</span>
                    : <><span className="rt-add">+{f.add}</span> <span className="rt-del">−{f.del}</span></>}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
      {path && file && (
        <div className="chg-diff">
          <div className="chg-diff-head">
            <span className="mono rt-job-cmd" title={path}>{path}</span>
            <button className="btn rt-stop" onClick={() => void navigator.clipboard?.writeText(path)}>Copy path</button>
          </div>
          {file.patch === false ? <p className="rt-label">Patch not recorded: no checkpoint was taken for this session</p>
            : diff?.text != null ? (
              <pre className="mono rt-diff-body">{diff.text.split("\n").map((l, i) => (
                <span key={i} className={l.startsWith("+") && !l.startsWith("+++") ? "rt-add" : l.startsWith("-") && !l.startsWith("---") ? "rt-del" : l.startsWith("@@") ? "rt-label" : undefined}>{l + "\n"}</span>
              ))}</pre>
            )
            : diff?.failed ? <button className="btn rt-stop" onClick={data.retry}>Couldn’t read the diff · Retry</button>
            : <p className="rt-label">Reading diff…</p>}
        </div>
      )}
    </div>
  );
}

/** The full-width review: #/s/<id>/changes. */
export function ChangesPage({ row, tick, onBack }: { row: Row; tick: number; onBack: () => void }) {
  const data = useChanges(row.id, tick);
  const [scope, setScope] = useState<Scope>("session");
  return (
    <div className="thread">
      <header className="thread-head page-head">
        <Back onBack={onBack} />
        <div className="head-main chg-head"><h1>Changes</h1><span className="chg-session">{sessionTitle(row)}</span></div>
      </header>
      <div className="scroll proj-body chg-page">
        <ChangesBody row={row} data={data} scope={scope} onScope={setScope} />
      </div>
    </div>
  );
}
