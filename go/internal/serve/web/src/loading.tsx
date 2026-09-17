import { useEffect, useRef, useState } from "react";

// One vocabulary for "not here yet" across the web UI: a spinner and a
// label, an elapsed hint once the wait is noticeable, and a client-side
// timeout that turns a hung request into an error you can retry.

/** A server or network error as a sentence: no "wiki: not found", no bare "TypeError". */
export function humanError(err: unknown): string {
  const m = (err instanceof Error ? err.message : String(err ?? "")).trim();
  if (!m) return "Unable to load this. Try again.";
  if (/not found|^404\b/i.test(m)) return "It could not be found — it may have been moved or deleted.";
  if (/forbidden|^403\b/i.test(m)) return "You do not have access to this.";
  if (/failed to fetch|networkerror|load failed/i.test(m)) return "The server did not answer. Check that bough serve is running.";
  if (/^5\d\d\b/.test(m)) return `The server hit an error (${m}).`;
  return m.replace(/^[a-z]+: /, "").replace(/^./, (c) => c.toUpperCase());
}

/** A Go duration ("5m0s", "1h2m0s", "60s") or milliseconds, without zero units: "5m", "1m", "1h 2m". */
export function duration(d: string | number): string {
  let s = typeof d === "number" ? Math.round(d / 1000) : 0;
  if (typeof d === "string") {
    for (const [, n, u] of d.matchAll(/([\d.]+)(h|ms|m|s)/g)) s += Number(n) * ({ h: 3600, m: 60, s: 1, ms: 0.001 }[u as "h"]);
    s = Math.round(s);
  }
  if (s < 60) return `${s}s`;
  const h = Math.floor(s / 3600), m = Math.floor(s / 60) % 60, r = s % 60;
  return [h && `${h}h`, m && `${m}m`, !h && r && `${r}s`].filter(Boolean).join(" ");
}

/** A live, ticking elapsed time since `since` (ISO or ms), in the `.num` face. */
export function Elapsed({ since, title, from = 0 }: { since: string | number; title?: string; /** Seconds before it shows at all. */ from?: number }) {
  const start = typeof since === "number" ? since : Date.parse(since);
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = window.setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, []);
  if (!Number.isFinite(start)) return null;
  if (now - start < from * 1000) return null;
  return <span className="num elapsed" title={title}>{elapsed(Math.max(0, now - start))}</span>;
}

/** A ticking duration keeps its smaller unit: "1m 12s", "1h 0m". Shared by the Working tail and the header clock. */
export function elapsed(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  return m < 60 ? `${m}m ${s % 60}s` : `${Math.floor(m / 60)}h ${m % 60}m`;
}

/** One centered empty/loading/error state: a title, one sentence, one primary action. */
export function EmptyState({ title, children, action, role }: {
  title: string;
  children?: React.ReactNode;
  action?: { label: string; onClick: () => void };
  role?: "alert" | "status";
}) {
  return (
    <div className="empty-state" role={role}>
      <h2>{title}</h2>
      {children && <p>{children}</p>}
      {action && <button className="btn btn-primary" onClick={action.onClick}>{action.label}</button>}
    </div>
  );
}

/**
 * One voice for a failure: a red dot and a title, a sentence, one action
 * (Retry only when trying again can help, otherwise a way back), and the
 * raw error behind a disclosure.
 */
export function ErrorNote({ title, err, children, action, className = "" }: {
  title: string;
  err?: unknown;
  children?: React.ReactNode;
  action?: { label: string; onClick: () => void };
  className?: string;
}) {
  const raw = (err instanceof Error ? err.message : String(err ?? "")).trim();
  return (
    <div className={("error-note " + className).trim()} role="alert">
      <p className="error-note-title"><span className="error-dot" aria-hidden="true" />{title}</p>
      {(children ?? (raw && humanError(err))) && <p className="error-note-body">{children ?? humanError(err)}</p>}
      {action && <button className="btn" onClick={action.onClick}>{action.label}</button>}
      {raw && <details className="error-note-details"><summary>Details</summary><pre className="mono">{raw}</pre></details>}
    </div>
  );
}

export function Spinner({ size = 12 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
         strokeLinecap="round" className="spin-mark" aria-hidden="true">
      <circle cx="12" cy="12" r="8.5" strokeDasharray="40 14" />
    </svg>
  );
}

/**
 * The loading/error state of one region. While `err` is empty it spins,
 * shows the elapsed seconds after `hintAfter` ms, and after `timeout` ms
 * gives up with a Retry. `err` set shows the error with the same Retry.
 * `lines` > 0 draws that many skeleton lines instead of the spinner row.
 */
export function Pending({ what, err, onRetry, action, timeout = 20_000, hintAfter = 3_000, lines = 0, inline = false }: {
  what: string;
  /** Replaces Retry as the one action, e.g. "Back to wiki" for a page that is gone. */
  action?: { label: string; onClick: () => void };
  err?: string | null;
  onRetry?: () => void;
  timeout?: number;
  hintAfter?: number;
  lines?: number;
  inline?: boolean;
}) {
  const [start, setStart] = useState(() => Date.now());
  const [now, setNow] = useState(start);
  const tick = useRef(0);
  useEffect(() => {
    if (err) return;
    tick.current = window.setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(tick.current);
  }, [err, start]);

  const elapsed = now - start;
  const timedOut = !err && elapsed >= timeout;
  const retry = onRetry && (() => { setStart(Date.now()); setNow(Date.now()); onRetry(); });
  const cls = "pending" + (inline ? " pending-inline" : "");

  if ((err || timedOut) && !inline) {
    return (
      <ErrorNote title={timedOut ? `${what} is taking too long` : `Couldn’t load ${what.toLowerCase()}`} err={timedOut ? undefined : err}
        action={action ?? (retry && { label: "Retry", onClick: retry })}>
        {timedOut ? "The server has not answered yet. Try again." : undefined}
      </ErrorNote>
    );
  }
  if (err || timedOut) {
    return (
      <div className={cls + " pending-err"} role="alert">
        <span className="pending-msg">
          {timedOut ? `${what} is taking too long to load.` : `Couldn’t load ${what.toLowerCase()}. ${humanError(err)}`}
        </span>
        {retry && <button className="btn" onClick={retry}>Retry</button>}
      </div>
    );
  }
  return (
    <div className={lines > 0 ? "skeleton" : cls} role="status" aria-live="polite" aria-busy="true">
      {lines > 0 ? (
        <>
          <span className="visually-hidden">Loading {what.toLowerCase()}…</span>
          <i className="skeleton-h1" />
          {Array.from({ length: lines - 1 }, (_, i) => <i key={i} style={{ width: `${92 - (i * 17) % 40}%` }} />)}
        </>
      ) : (
        <span className="pending-msg">
          <Spinner /> Loading {what.toLowerCase()}…
          {elapsed >= hintAfter && <span className="pending-hint"> {Math.floor(elapsed / 1000)}s</span>}
        </span>
      )}
    </div>
  );
}

/** Copy to the clipboard and say so for a moment: `[copied, copy]`. */
export function useCopied(ms = 1500): [boolean, (text: string) => void] {
  const [copied, setCopied] = useState(false);
  const t = useRef(0);
  useEffect(() => () => clearTimeout(t.current), []);
  const copy = (text: string) => {
    void navigator.clipboard?.writeText(text).then(() => {
      setCopied(true);
      clearTimeout(t.current);
      t.current = window.setTimeout(() => setCopied(false), ms);
    });
  };
  return [copied, copy];
}

/** A small button that copies `text` and reads "Copied" for a moment. */
export function CopyButton({ text, label = "Copy", className = "btn btn-sm" }: { text: string; label?: string; className?: string }) {
  const [copied, copy] = useCopied();
  return (
    <button className={className} onClick={() => copy(text)} aria-live="polite">{copied ? "Copied" : label}</button>
  );
}
