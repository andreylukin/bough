import { useEffect, useRef, useState } from "react";

// One vocabulary for "not here yet" across the web UI: a spinner and a
// label, an elapsed hint once the wait is noticeable, and a client-side
// timeout that turns a hung request into an error you can retry.

/** A server or network error as a sentence: no "wiki: not found", no bare "TypeError". */
export function humanError(err: unknown): string {
  const m = (err instanceof Error ? err.message : String(err ?? "")).trim();
  if (!m) return "Something went wrong.";
  if (/not found|^404\b/i.test(m)) return "It could not be found — it may have been moved or deleted.";
  if (/failed to fetch|networkerror|load failed/i.test(m)) return "The server did not answer. Check that bough serve is running.";
  if (/^5\d\d\b/.test(m)) return `The server hit an error (${m}).`;
  return m.replace(/^[a-z]+: /, "").replace(/^./, (c) => c.toUpperCase());
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
export function Pending({ what, err, onRetry, timeout = 20_000, hintAfter = 3_000, lines = 0, inline = false }: {
  what: string;
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

  if (err || timedOut) {
    return (
      <div className={cls + " pending-err"} role="alert">
        <span className="pending-msg">
          {timedOut ? `${what} is taking too long to load.` : `${what} did not load. ${humanError(err)}`}
        </span>
        {retry && <button className="btn" onClick={retry}>Retry</button>}
      </div>
    );
  }
  return (
    <div className={cls} role="status" aria-live="polite" aria-busy="true">
      {lines > 0 ? (
        <>
          <span className="visually-hidden">Loading {what.toLowerCase()}…</span>
          {Array.from({ length: lines }, (_, i) => <span key={i} className="pending-line" style={{ width: `${92 - (i * 17) % 40}%` }} />)}
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
