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

/** "8m", "3h", "2d": how long ago, as a sidebar reads it. */
export function ago(iso: string): string {
  const s = Math.max(0, (Date.now() - Date.parse(iso)) / 1000);
  if (s < 60) return "<1m";
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
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

/** A turn error's first line as a short title, and the provider's own message: "Anthropic returned 401 Unauthorized", "API key is invalid.". */
export function providerError(text: string): { title: string; body: string } {
  const first = text.trim().split("\n")[0] ?? "";
  const names: Record<string, string> = { anthropic: "Anthropic", openai: "OpenAI", openrouter: "OpenRouter", cerebras: "Cerebras", google: "Google", gemini: "Gemini" };
  const m = /^llm-([a-z]+):.*?\b(\d{3})\b:?\s*([A-Za-z ]+)?/.exec(first);
  let title: string, code = "";
  if (m) {
    code = m[2];
    const name = names[m[1]] ?? m[1].replace(/^./, (c) => c.toUpperCase());
    // Only a status phrase ("Unauthorized"), not the start of a sentence ("Missing Authentication header").
    const phrase = (m[3] ?? "").trim();
    title = `${name} returned ${code}${/^(Unauthorized|Forbidden|Not Found|Bad Request|Too Many Requests|Internal Server Error|Bad Gateway|Service Unavailable|Gateway Timeout|Payment Required|Request Timeout|Overloaded)$/i.test(phrase) ? " " + phrase : ""}`;
  } else {
    title = humanError(first);
    if (title.length > 120) title = title.slice(0, 119).trimEnd() + "…";
  }
  let body = "";
  const j = text.indexOf("{");
  if (j >= 0) {
    try {
      const v = JSON.parse(text.slice(j, text.lastIndexOf("}") + 1));
      const msg = v?.error?.message ?? v?.message;
      if (typeof msg === "string") body = msg.trim();
    } catch { /* not JSON */ }
  }
  if (code === "401" || code === "403") body = (body ? body.replace(/([^.!?])$/, "$1.") + " " : "") + "Check the key in Settings, or switch to another model.";
  return { title, body };
}

type Glyph = "alert" | "missing" | "search";
/** The 16px mark of a state: a red alert for a failure, a neutral question or search for a fact. */
export function StateIcon({ kind, size = 16 }: { kind: Glyph; size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round"
         strokeLinejoin="round" aria-hidden="true" className={"state-icon state-icon-" + kind}>
      {kind === "alert" ? <><circle cx="12" cy="12" r="9" /><path d="M12 7.5v5M12 16h.01" /></>
        : kind === "missing" ? <><path d="M14 3H6a1 1 0 0 0-1 1v16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1V8z" /><path d="M14 3v5h5M10.3 12.2a1.8 1.8 0 1 1 2.5 1.7c-.5.2-.8.6-.8 1.1M12 17.5h.01" /></>
        : <><circle cx="11" cy="11" r="6.5" /><path d="m20 20-4.2-4.2" /></>}
    </svg>
  );
}

/** An inline list failure: a small red glyph, a quiet sentence, a ghost Retry. */
export function InlineFail({ what, onRetry }: { what: string; onRetry?: () => void }) {
  return (
    <span className="inline-fail" role="alert">
      <StateIcon kind="alert" size={12} />
      <span className="inline-fail-text">{what}</span>
      {onRetry && <button type="button" className="link" onClick={onRetry}>Retry</button>}
    </span>
  );
}

/** `busyLabel` is for an action that is not a retry: "Asking…" must not read "Retrying…". */
type Act = { label: string; onClick: () => void; busy?: boolean; busyLabel?: string };
function StateButton({ a, primary }: { a: Act; primary?: boolean }) {
  return (
    <button className={"btn" + (primary ? " btn-primary" : "")} onClick={a.onClick} disabled={a.busy} aria-busy={a.busy || undefined}>
      {a.busy ? <><Spinner /> {a.busyLabel ?? "Retrying…"}</> : a.label}
    </button>
  );
}

/** "Show details" over the verbatim text; closed by default. */
export function RawDetails({ raw, className = "state-details" }: { raw: string; className?: string }) {
  const [open, setOpen] = useState(false);
  return (
    <details className={className} onToggle={(e) => setOpen((e.currentTarget as HTMLDetailsElement).open)}>
      <summary>
        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden="true"><path d="m9 6 6 6-6 6" /></svg>
        {open ? "Hide details" : "Show details"}
      </summary>
      <pre className="mono">{raw}</pre>
    </details>
  );
}

/** One centered empty/not-found state (MB-ERR): an optional neutral glyph, a title, one sentence, at most one primary. */
export function EmptyState({ title, children, action, secondary, glyph, primary = true, card = false, role }: {
  title: string;
  children?: React.ReactNode;
  action?: Act;
  secondary?: Act;
  glyph?: Glyph;
  /** The action is the point of the page (New project…); otherwise it is a secondary button. */
  primary?: boolean;
  /** An empty section inside a page sits in a hairline card. */
  card?: boolean;
  role?: "alert" | "status";
}) {
  return (
    <div className={"empty-state state" + (card ? " state-card" : "") + (glyph ? " state-" + glyph : "")} role={role}>
      {glyph && <span className="state-glyph"><StateIcon kind={glyph} /></span>}
      <h2>{title}</h2>
      {children && <p>{children}</p>}
      {(action || secondary) && (
        <div className="state-actions">
          {action && <StateButton a={action} primary={primary} />}
          {secondary && <StateButton a={secondary} />}
        </div>
      )}
    </div>
  );
}

/**
 * One voice for a failure: a red alert glyph in a neutral box, a title, a
 * sentence, a primary action (Retry when trying again can help, otherwise a
 * way back), an optional way out, and the raw error behind "Show details".
 */
export function ErrorNote({ title, err, children, action, secondary, className = "" }: {
  title: string;
  err?: unknown;
  children?: React.ReactNode;
  action?: Act;
  secondary?: Act;
  className?: string;
}) {
  const raw = (err instanceof Error ? err.message : String(err ?? "")).trim();
  return (
    <div className={("error-note state state-alert " + className).trim()}>
      <span className="state-glyph"><StateIcon kind="alert" /></span>
      <h2 className="error-note-title" role="alert">{title}</h2>
      {(children ?? (raw && humanError(err))) && <p className="error-note-body">{children ?? humanError(err)}</p>}
      {(action || secondary) && (
        <div className="state-actions">
          {action && <StateButton a={action} primary />}
          {secondary && <StateButton a={secondary} />}
        </div>
      )}
      {raw && <RawDetails raw={raw} className="state-details error-note-details" />}
    </div>
  );
}

/** A create in flight: the palette has closed, and nothing else on the page says a child is starting. */
export function StartingStatus() {
  return <div className="updated" role="status"><Spinner /><span className="updated-text">Starting a session…</span></div>;
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
        {timedOut ? "The server has not answered yet." : undefined}
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
  const done = () => {
    setCopied(true);
    clearTimeout(t.current);
    t.current = window.setTimeout(() => setCopied(false), ms);
  };
  // Without the async clipboard (plain http, an old webview) a hidden
  // textarea and execCommand still copy, so the button always confirms.
  const fallback = (text: string) => {
    const ta = document.createElement("textarea");
    ta.value = text; ta.setAttribute("readonly", ""); ta.style.position = "fixed"; ta.style.opacity = "0";
    document.body.appendChild(ta); ta.select();
    try { document.execCommand("copy"); } catch { /* nothing to copy with */ }
    ta.remove();
    done();
  };
  const copy = (text: string) => {
    if (!navigator.clipboard?.writeText) return fallback(text);
    navigator.clipboard.writeText(text).then(done, () => fallback(text));
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
