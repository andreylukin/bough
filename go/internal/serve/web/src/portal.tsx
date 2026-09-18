import { useCallback, useEffect, useRef, useState } from "react";
import type { OrbPortal, Row } from "./types";
import { api } from "./api";
import { Elapsed, EmptyState, InlineFail, Spinner } from "./loading";

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
export function PortalPane({ row, onClose, onAsk }: { row: Row; onClose: () => void; onAsk: (text: string) => Promise<unknown> }) {
  const [portals, setPortals] = useState<OrbPortal[] | null>(null);
  const [fail, setFail] = useState<string>();
  const [at, setAt] = useState<number>();
  // Where the frame is pointed, and what is in the bar. They differ
  // while the user is typing. `typed` holds the path, not the whole URL:
  // the port is a fixed prefix the user never has to retype.
  const [url, setUrl] = useState("");
  const [typed, setTyped] = useState("");
  // Reloading means remounting: a cross-origin frame's history is not
  // ours to touch.
  const [nonce, setNonce] = useState(0);
  const barRef = useRef<HTMLInputElement>(null);
  // Asking the agent for a portal is a turn, not a request we can wait
  // on: "sent" is all we can honestly claim until the poll sees one.
  const [ask, setAsk] = useState<"" | "busy" | "sent">("");
  const [askErr, setAskErr] = useState<string>();

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

  const send = async (text: string) => {
    setAsk("busy"); setAskErr(undefined);
    try { await onAsk(text); setAsk("sent"); }
    catch (e) { setAsk(""); setAskErr(e instanceof Error ? e.message : String(e)); }
  };
  // Once the portal being looked at is live the ask has been answered:
  // the pane says so by showing the page, not by keeping a note about the
  // question. Asking "is any portal live" was wrong — the dead arm shows
  // for the SELECTED portal, which is exactly the case where another one
  // is live and the picker is on screen.
  const shownLive = portals?.find((p) => p.host === at)?.live;
  useEffect(() => { if (shownLive) { setAsk(""); setAskErr(undefined); } }, [shownLive]);

  // Follow the session's own portals: the one it opened last is the one
  // it is most likely talking about.
  useEffect(() => {
    if (!portals || portals.length === 0) { setAt(undefined); return; }
    setAt((cur) => (cur !== undefined && portals.some((p) => p.host === cur) ? cur : portals[portals.length - 1].host));
  }, [portals]);

  const shown = portals?.find((p) => p.host === at);
  const shownURL = shown?.url;
  // Switching portal starts again at its front door. Derived from the
  // portal alone: reading `url` here would pin a stale closure.
  useEffect(() => {
    if (!shownURL) return;
    setUrl(shownURL);
    setTyped("/");
  }, [shownURL]);

  const label = shown ? (shown.name || "Portal") : "Portal";
  const origin = shownURL ? shownURL.replace(/\/$/, "") : "";
  const path = url.startsWith(origin) ? url.slice(origin.length) || "/" : url;

  // A bare path is relative to the portal; a full URL still works.
  // An empty Enter now goes to "/" rather than doing nothing.
  const go = (next: string) => {
    const abs = /^https?:\/\//.test(next);
    const p = next.startsWith("/") ? next : "/" + next;
    setTyped(abs ? next : p || "/");
    setUrl(abs ? next : origin + (p || "/"));
    setNonce((n) => n + 1);
  };

  // Width is the user's, not ours. Kept per browser, clamped on read.
  const [wide, setWide] = useState(() => {
    try { const n = Number(localStorage.getItem("bough:portal-w")); return n >= 360 && n <= 1400 ? n : 0; } catch { return 0; }
  });
  const drag = useRef<{ x: number; w: number } | null>(null);
  const onGrip = (e: React.PointerEvent<HTMLDivElement>) => {
    const pane = e.currentTarget.parentElement as HTMLElement;
    drag.current = { x: e.clientX, w: pane.getBoundingClientRect().width };
    e.currentTarget.setPointerCapture(e.pointerId);
  };
  const onGripMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!drag.current) return;
    setWide(Math.min(1400, Math.max(360, drag.current.w + (drag.current.x - e.clientX))));
  };
  const onGripUp = () => { drag.current = null; try { if (wide) localStorage.setItem("bough:portal-w", String(wide)); } catch { /* private window */ } };
  const onGripReset = () => { setWide(0); try { localStorage.removeItem("bough:portal-w"); } catch { /* private window */ } };

  return (
    <aside className="portal-pane" aria-label="Portal" style={wide ? { flexBasis: `${wide}px` } : undefined}>
      {/* A separator with no tabIndex cannot be reached, which left the
          pane's width a pointer-only setting and its focus ring dead.
          Arrows resize, Home restores the default. */}
      <div className="portal-grip" role="separator" aria-label="Resize the portal pane" aria-orientation="vertical"
           tabIndex={0} aria-valuenow={wide || undefined} aria-valuemin={360} aria-valuemax={1400}
           onPointerDown={onGrip} onPointerMove={onGripMove} onPointerUp={onGripUp} onDoubleClick={onGripReset}
           onKeyDown={(e) => {
             const step = e.shiftKey ? 64 : 16;
             const now = wide || (e.currentTarget.parentElement as HTMLElement).getBoundingClientRect().width;
             if (e.key === "ArrowLeft") { e.preventDefault(); setWide(Math.min(1400, now + step)); }
             else if (e.key === "ArrowRight") { e.preventDefault(); setWide(Math.max(360, now - step)); }
             else if (e.key === "Home") { e.preventDefault(); onGripReset(); }
             else return;
             try { localStorage.setItem("bough:portal-w", String(wide)); } catch { /* private window */ }
           }} />
      <header className="portal-head">
        <span className="portal-title">{label}</span>
        {shown && <span className="portal-port mono">:{shown.guest}</span>}
        {shown && !shown.live && <span className="chip portal-closed">Closed</span>}
        {portals && portals.length > 1 && (
          <div className="seg portal-pick" role="group" aria-label="Which portal">
            {portals.map((p) => (
              <button key={p.host} type="button" className="seg-item" aria-pressed={p.host === at}
                      onClick={() => setAt(p.host)}>{p.name || `:${p.guest}`}</button>
            ))}
          </div>
        )}
        <div className="portal-acts">
          <button className="btn btn-ghost portal-ctl portal-x" onClick={onClose} title="Close">
            <GlyphX /><span className="portal-ctl-t">Close</span>
          </button>
        </div>
      </header>

      {shown && shown.live && (
        <form className="portal-bar" onSubmit={(e) => { e.preventDefault(); go(typed.trim()); barRef.current?.blur(); }}>
          <div className="portal-loc">
            <span className="portal-loc-port mono" title={shown.url}>{shown.guest}</span>
            <input ref={barRef} className="portal-url mono" value={typed} spellCheck={false}
                   aria-label={`Go to a path on port ${shown.guest}`} placeholder="/"
                   title="Clicks inside the page aren’t shown here."
                   onChange={(e) => setTyped(e.target.value)}
                   onKeyDown={(e) => { if (e.key === "Escape") { setTyped(path); e.currentTarget.blur(); } }} />
          </div>
          <button type="submit" className="btn btn-ghost portal-ctl" title="Go">
            <GlyphGo /><span className="portal-ctl-t">Go</span>
          </button>
          <button type="button" className="btn btn-ghost portal-ctl" onClick={() => setNonce((n) => n + 1)} title="Reload">
            <GlyphReload /><span className="portal-ctl-t">Reload</span>
          </button>
          <a className="btn btn-ghost portal-ctl" href={url || shown.url} target="_blank" rel="noreferrer" title="New tab">
            <GlyphTab /><span className="portal-ctl-t">New tab</span>
          </a>
        </form>
      )}

      <div className="portal-body">
        {fail ? <div className="portal-note"><InlineFail what="Couldn’t read this session’s portals" onRetry={() => void read()} /></div>
          : portals === null ? <p className="portal-note">Loading…</p>
          : portals.length === 0 ? (
            <PortalEmpty project={row.mode === "project"} ask={ask} askErr={askErr}
                         onAsk={() => void send(ASK_OPEN)} />
          )
          : !shown ? null
          : !shown.live ? (
            <PortalDead portal={shown} ask={ask} askErr={askErr} onCheck={() => void read()}
                        onAsk={() => void send(`The portal on port ${shown.guest} is dead — restart that server in the orb and open a portal to it again.`)} />
          ) : (
            <PortalFrame portal={shown} src={url || shown.url} nonce={nonce}
                         onReload={() => setNonce((n) => n + 1)} />
          )}
      </div>
    </aside>
  );
}

/** The user-facing name of a portal: the agent's name for it, else its guest port. */
export const portName = (p: OrbPortal) => p.name || `port ${p.guest}`;

const cap = (s: string) => s.replace(/^./, (c) => c.toUpperCase());

/**
 * The frame and its two honest states. A cross-origin frame cannot report an
 * HTTP status, so this reports only what is observable: whether a document has
 * loaded (`onLoad` — which fires for an error page too). It never names a
 * status code. Liveness of the port is PortalPane's branch, not ours.
 */
function PortalFrame({ portal, src, nonce, onReload }: {
  portal: OrbPortal; src: string; nonce: number; onReload: () => void;
}) {
  const id = `${portal.host}:${nonce}`;
  const [loaded, setLoaded] = useState(false);
  const [slow, setSlow] = useState(false);
  const [since, setSince] = useState(() => Date.now());

  useEffect(() => {
    setLoaded(false); setSlow(false); setSince(Date.now());
    const t = window.setTimeout(() => setSlow(true), 10_000);
    return () => clearTimeout(t);
  }, [id, src]);

  const port = portName(portal);
  const phase = loaded ? "shown" : slow ? "stalled" : "loading";

  return (
    <div className="portal-stage" data-phase={phase}>
      <iframe key={id} className="portal-frame" src={src}
              title={portal.name || `Port ${portal.guest} in the orb`}
              onLoad={() => setLoaded(true)} onError={() => setSlow(true)} />
      {phase === "loading" && (
        <div className="portal-veil" role="status" aria-live="polite">
          <span className="portal-veil-row"><Spinner /> Loading {port}… <Elapsed since={since} from={3} /></span>
        </div>
      )}
      {phase === "stalled" && (
        <div className="portal-veil portal-veil-hold">
          <EmptyState card={false} primary={false}
            title={`${cap(port)} is accepting, but the page has not arrived`}
            action={{ label: "Reload", onClick: onReload }}>
            The server answered the connection and then went quiet. A dev server
            building for the first time can take a minute — this keeps waiting.
          </EmptyState>
          <span className="portal-veil-row is-hold"><Spinner /> Still waiting <Elapsed since={since} /></span>
        </div>
      )}
    </div>
  );
}

const ASK_OPEN = "Start this project’s dev server inside the orb and open a portal to its port, then tell me which port it is on.";

type AskState = { ask: "" | "busy" | "sent"; askErr?: string; onAsk: () => void };

/**
 * What became of the ask. A prompt is a turn, so the only truthful report is
 * that it went — the pane finds the portal itself on the next poll.
 */
function AskNote({ ask, askErr, onAsk }: AskState) {
  if (askErr) return <p className="portal-ask-note"><InlineFail what="Couldn’t send that message" onRetry={onAsk} /></p>;
  if (ask !== "sent") return null;
  return <p className="portal-ask-note" role="status">Asked. The reply lands in the thread; this pane picks the portal up on its own.</p>;
}

function PortalEmpty({ project, ask, askErr, onAsk }: AskState & { project: boolean }) {
  if (!project) {
    return (
      <div className="portal-state">
        <EmptyState glyph="missing" primary={false} card title="No orb to look into">
          This is a local session, running on this machine. A portal shows a server
          running inside a project orb.
        </EmptyState>
      </div>
    );
  }
  return (
    <div className="portal-state">
      <EmptyState glyph="missing" card title="No portal open"
        action={{ label: "Ask the agent to open one", onClick: onAsk, busy: ask === "busy", busyLabel: "Asking…" }}>
        Nothing in this orb is being served yet. The agent opens a portal on the port
        its dev server listens on, and the page appears here.
      </EmptyState>
      <AskNote ask={ask} askErr={askErr} onAsk={onAsk} />
    </div>
  );
}

/** The record outlived its listener: the port is in the list and nothing answers. */
function PortalDead({ portal, ask, askErr, onAsk, onCheck }: AskState & { portal: OrbPortal; onCheck: () => void }) {
  return (
    <div className="portal-state">
      <EmptyState glyph="alert" card title={`Nothing answers on port ${portal.guest}`}
        action={{ label: "Ask the agent to reopen it", onClick: onAsk, busy: ask === "busy", busyLabel: "Asking…" }}
        secondary={{ label: "Check again", onClick: onCheck }}>
        A portal belongs to the session that opened it and closes when that session
        ends. The record is still here; the listener is gone.
      </EmptyState>
      <AskNote ask={ask} askErr={askErr} onAsk={onAsk} />
    </div>
  );
}

const GlyphX = () => (
  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" aria-hidden="true"><path d="M6 6l12 12M18 6L6 18" /></svg>
);
const GlyphGo = () => (
  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M5 12h13M12 5l7 7-7 7" /></svg>
);
const GlyphReload = () => (
  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M20 11a8 8 0 10-.7 4.3M20 5v6h-6" /></svg>
);
const GlyphTab = () => (
  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M14 5h5v5M19 5l-8 8M18 14v5H5V6h5" /></svg>
);
