import { useCallback, useEffect, useRef, useState } from "react";
import type { OrbPortal, Row } from "./types";
import { api } from "./api";
import { InlineFail } from "./loading";

/**
 * The Portal pane: a web server running inside this session's orb, shown
 * beside the thread so the page and the agent changing it are on screen
 * together.
 *
 * The orb is on this machine, so a portal is a loopback port and the
 * frame loads it directly — there is no tunnel and nothing to rewrite.
 * The frame is another origin, so this pane can point it somewhere and
 * remount it, but never read inside it: after a link is clicked in the
 * page, the address bar still shows where we sent it, not where it went.
 *
 * A portal is a route to a process, not a deployment. The listener
 * belongs to the session that opened it and the record outlives it, so
 * `live` is what says which of the two you are looking at.
 */
export function PortalPane({ row, onClose }: { row: Row; onClose: () => void }) {
  const [portals, setPortals] = useState<OrbPortal[] | null>(null);
  const [fail, setFail] = useState<string>();
  const [at, setAt] = useState<number>();
  // Where the frame is pointed, and what is in the bar. They differ
  // while the user is typing.
  const [url, setUrl] = useState("");
  const [typed, setTyped] = useState("");
  // Reloading means remounting: a cross-origin frame's history is not
  // ours to touch.
  const [nonce, setNonce] = useState(0);
  const barRef = useRef<HTMLInputElement>(null);

  const read = useCallback(async () => {
    try {
      const orb = await api.sessionOrb(row.id);
      setPortals(orb?.portals ?? []);
      setFail(undefined);
    } catch (e) {
      setFail(e instanceof Error ? e.message : String(e));
    }
  }, [row.id]);

  useEffect(() => { void read(); }, [read]);
  // A dev server the agent has not started yet turns up on its own.
  useEffect(() => {
    const t = setInterval(() => void read(), 4000);
    return () => clearInterval(t);
  }, [read]);

  // Follow the session's own portals: the one it opened last is the one
  // it is most likely talking about.
  useEffect(() => {
    if (!portals || portals.length === 0) { setAt(undefined); return; }
    setAt((cur) => (cur !== undefined && portals.some((p) => p.host === cur) ? cur : portals[portals.length - 1].host));
  }, [portals]);

  const shown = portals?.find((p) => p.host === at);
  // Switching portal moves the address with it. A path the user typed
  // under the same portal survives the poll that follows it.
  const shownURL = shown?.url;
  useEffect(() => {
    if (!shownURL) return;
    setUrl((cur) => (cur.startsWith(shownURL) ? cur : shownURL));
    setTyped((cur) => (cur.startsWith(shownURL) ? cur : shownURL));
  }, [shownURL]);

  // A bare path is relative to the portal, so "/admin" is enough.
  const go = (next: string) => {
    if (!next) return;
    const full = /^https?:\/\//.test(next) ? next
      : shownURL ? shownURL.replace(/\/$/, "") + (next.startsWith("/") ? next : "/" + next)
      : next;
    setTyped(full);
    setUrl(full);
    setNonce((n) => n + 1);
  };

  return (
    <aside className="portal-pane" aria-label="Portal">
      <header className="portal-head">
        <span className="portal-title">Portal</span>
        {portals && portals.length > 1 && (
          <div className="seg portal-pick" role="group" aria-label="Which portal">
            {portals.map((p) => (
              <button key={p.host} type="button" className="seg-item" aria-pressed={p.host === at}
                      onClick={() => setAt(p.host)}>{p.name || p.guest}</button>
            ))}
          </div>
        )}
        <button className="portal-x" onClick={onClose} aria-label="Close portal">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" aria-hidden="true"><path d="M6 6l12 12M18 6L6 18" /></svg>
        </button>
      </header>

      {shown && shown.live && (
        <form className="portal-bar" onSubmit={(e) => { e.preventDefault(); go(typed.trim()); barRef.current?.blur(); }}>
          <input ref={barRef} className="portal-url mono" value={typed} spellCheck={false}
                 aria-label="Portal address" placeholder={shown.url}
                 onChange={(e) => setTyped(e.target.value)}
                 onKeyDown={(e) => { if (e.key === "Escape") { setTyped(url); e.currentTarget.blur(); } }} />
          <button type="submit" className="portal-act" title="Go" aria-label="Go to this address">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M5 12h13M12 5l7 7-7 7" /></svg>
          </button>
          <button type="button" className="portal-act" onClick={() => setNonce((n) => n + 1)} title="Reload" aria-label="Reload the page">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M20 11a8 8 0 10-.7 4.3M20 5v6h-6" /></svg>
          </button>
          <a className="portal-act" href={url || shown.url} target="_blank" rel="noreferrer" title="Open in a tab" aria-label="Open in a browser tab">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M14 5h5v5M19 5l-8 8M18 14v5H5V6h5" /></svg>
          </a>
        </form>
      )}

      <div className="portal-body">
        {fail ? <div className="portal-note"><InlineFail what="Couldn’t read this session’s portals" onRetry={() => void read()} /></div>
          : portals === null ? <p className="portal-note">Loading…</p>
          : portals.length === 0 ? <PortalEmpty project={row.mode === "project"} />
          : !shown ? null
          : !shown.live ? (
            <p className="portal-note">
              Nothing answers on {shown.url} any more. A portal belongs to the session that
              opened it and closes when that session ends — ask the agent to open it again.
            </p>
          ) : (
            <iframe key={`${shown.host}:${nonce}`} className="portal-frame" src={url || shown.url}
                    title={shown.name || `Port ${shown.guest} in the orb`} />
          )}
      </div>
    </aside>
  );
}

function PortalEmpty({ project }: { project: boolean }) {
  return (
    <p className="portal-note">
      {project
        ? <>No portal is open. Ask the agent to start the dev server and open one — it calls <code>tools.portal.open(3000)</code> and the page appears here.</>
        : <>This is a local session, so there is no orb to open a portal into. Portals show a server running inside a project orb.</>}
    </p>
  );
}
