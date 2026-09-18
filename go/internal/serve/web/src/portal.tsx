import { useCallback, useEffect, useState } from "react";
import type { OrbPortal, Row } from "./types";
import { api } from "./api";
import { Back } from "./app";
import { sessionTitle } from "./render";
import { InlineFail } from "./loading";

/**
 * The Portal tab: a web server running inside this session's orb, shown
 * in place. The orb is on this machine, so a portal is a loopback port
 * and the frame loads it directly — there is no tunnel and nothing to
 * rewrite. The frame is another origin, so this page can point at it and
 * reload it but never read inside it.
 *
 * A portal is a route to a process, not a deployment: the listener
 * belongs to the session that opened it, and the record outlives the
 * process. `live` is what says which of the two you are looking at.
 */
export function PortalPage({ row, onBack }: { row: Row; onBack: () => void }) {
  const [portals, setPortals] = useState<OrbPortal[] | null>(null);
  const [fail, setFail] = useState<string>();
  const [at, setAt] = useState<number>();
  // Reloading the frame means remounting it: a cross-origin frame's
  // history is not ours to touch.
  const [nonce, setNonce] = useState(0);

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
  // A dev server the agent has not started yet appears on its own.
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
  return (
    <div className="thread">
      <header className="thread-head page-head">
        <Back onBack={onBack} />
        <div className="head-main portal-head">
          <h1>Portal</h1><span className="chg-session">{sessionTitle(row)}</span>
        </div>
        {portals && portals.length > 1 && (
          <div className="seg portal-pick" role="group" aria-label="Which portal">
            {portals.map((p) => (
              <button key={p.host} type="button" className="seg-item" aria-pressed={p.host === at}
                      onClick={() => setAt(p.host)}>{p.name || `Port ${p.guest}`}</button>
            ))}
          </div>
        )}
      </header>
      {/* The address bar sits above the frame, not in the header: the
          header is a grid of named areas, and an address belongs with
          the thing it addresses. */}
      {shown && shown.live && (
        <div className="portal-bar">
          <a className="portal-url mono" href={shown.url} target="_blank" rel="noreferrer"
             title="Open in a browser tab">{shown.url}</a>
          <button className="btn" onClick={() => setNonce((n) => n + 1)}>Reload</button>
        </div>
      )}
      <div className="scroll portal-body">
        {fail ? <InlineFail what="Couldn’t read this session’s portals" onRetry={() => void read()} />
          : portals === null ? <p className="portal-note">Loading…</p>
          : portals.length === 0 ? <PortalEmpty project={row.mode === "project"} />
          : !shown ? null
          : !shown.live ? (
            <p className="portal-note">
              Nothing answers on {shown.url} any more. A portal belongs to the session that
              opened it and closes when that session ends — ask the agent to open it again.
            </p>
          ) : (
            <iframe key={`${shown.host}:${nonce}`} className="portal-frame" src={shown.url}
                    title={shown.name || `Port ${shown.guest} in the orb`} />
          )}
      </div>
    </div>
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
