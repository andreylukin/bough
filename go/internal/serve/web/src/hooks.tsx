import { useCallback, useEffect, useMemo, useState } from "react";
import { Back } from "./app";

// The hooks wire types live here, not in types.ts: they are read by this
// view and nothing else, and GET /api/hooks always sends every field.
export interface Hook {
  name: string;
  event: string;
  path: string;
  scope: "home" | "project";
  shadowed: boolean;
  lastFired: string | null;
  lastDecision: string;
  failing: boolean;
  error: string;
}

export interface Watcher {
  name: string;
  path: string;
  every: string;
  failing: boolean;
  error: string;
  lastRun: string | null;
  lastWoke: string | null;
}

export interface Fire {
  at: string;
  session: string;
  event: string;
  name: string;
  ms: number;
  decision: string;
  error: string;
}

export interface HooksData {
  hooks: Hook[];
  watchers: Watcher[];
  fires: Fire[];
}

const clock = (iso: string) =>
  new Date(iso).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
const when = (iso: string | null, never: string) => (iso ? clock(iso) : never);

const POLL_MS = 5000; // a watcher that just fired should not need a reload

async function req<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(path, {
    ...init,
    headers: init?.body ? { "content-type": "application/json" } : undefined,
  });
  if (!res.ok) {
    let detail = `${res.status} ${res.statusText}`;
    try {
      const body = (await res.json()) as { error?: string };
      if (body.error) detail = body.error;
    } catch {
      /* a non-JSON error body is not worth masking the status */
    }
    throw new Error(detail);
  }
  return (await res.json()) as T;
}

export const hooksApi = {
  all: () => req<HooksData>("/api/hooks"),
  read: (path: string) =>
    req<{ path: string; body: string }>(`/api/hooks/file?path=${encodeURIComponent(path)}`).then((r) => r.body),
  write: (path: string, body: string) =>
    req<{ ok: true }>("/api/hooks/file", { method: "PUT", body: JSON.stringify({ path, body }) }).then(() => {}),
  dryrun: (path: string, event: string) =>
    req<{ result: unknown; error: string; ms: number }>("/api/hooks/dryrun", {
      method: "POST",
      body: JSON.stringify({ path, event }),
    }),
};

export type Load = (path: string) => Promise<string>;
export type Save = (path: string, body: string) => Promise<void>;
export type DryRun = (path: string, event: string) => Promise<{ result: unknown; error: string; ms: number }>;

/**
 * The file behind one watcher or hook, opened in place. It loads on
 * first expand rather than with the list: most visits never open one,
 * and the list is the answer to "is this thing even loaded?".
 */
function Source({ path, event, load, save, dryrun }: {
  path: string; event?: string; load: Load; save: Save; dryrun?: DryRun;
}) {
  const [open, setOpen] = useState(false);
  const [body, setBody] = useState<string | null>(null);
  const [note, setNote] = useState("");
  const [err, setErr] = useState("");
  const id = `src-${path.replace(/\W+/g, "-")}`;

  const expand = () => {
    const next = !open;
    setOpen(next);
    if (!next || body !== null) return;
    load(path).then(setBody).catch((e: unknown) => setErr(e instanceof Error ? e.message : String(e)));
  };

  return (
    <div className="hk-src">
      <button className="hk-toggle" aria-expanded={open} aria-controls={id} onClick={expand}>
        {open ? "Hide file" : "Open file"}
      </button>
      <div className="hk-panel" id={id} hidden={!open}>
        <p className="mono hk-path">{path}</p>
        {err && <p className="err">{err}</p>}
        {body === null
          ? !err && <p className="proj-none">Loading…</p>
          : (
            <>
              <label className="visually-hidden" htmlFor={`${id}-body`}>File contents</label>
              <textarea id={`${id}-body`} className="hk-edit mono" rows={10} spellCheck={false}
                        value={body} onChange={(e) => setBody(e.target.value)} />
              <div className="hk-acts">
                <button className="btn btn-primary" onClick={() => {
                  setNote(""); setErr("");
                  save(path, body).then(() => setNote("Saved.")).catch((e: unknown) =>
                    setErr(e instanceof Error ? e.message : String(e)));
                }}>Save</button>
                {dryrun && event && (
                  <button className="btn" onClick={() => {
                    setNote(""); setErr("");
                    dryrun(path, event).then((r) => {
                      if (r.error) { setErr(r.error); return; }
                      setNote(`Dry run finished in ${r.ms}ms: ${JSON.stringify(r.result)}`);
                    }).catch((e: unknown) => setErr(e instanceof Error ? e.message : String(e)));
                  }}>Dry run</button>
                )}
                {note && <span className="hk-note" role="status">{note}</span>}
              </div>
            </>
          )}
      </div>
    </div>
  );
}

function WatcherRow({ w, load, save }: { w: Watcher; load: Load; save: Save }) {
  return (
    <div className="proj-row hk-row">
      <div className="hk-main">
        <span className="mono hk-name">{w.name}</span>
        {w.failing
          ? <span className="hk-state hk-bad">Failing</span>
          : <span className="hk-state hk-ok">Healthy</span>}
        <span className="hk-when">Every {w.every}</span>
        <span className="hk-when">Ran {when(w.lastRun, "never")}</span>
        <span className="hk-when">Woke a session {when(w.lastWoke, "never")}</span>
      </div>
      {w.failing && w.error && <p className="err hk-why">{w.error}</p>}
      <Source path={w.path} load={load} save={save} />
    </div>
  );
}

function HookRow({ h, load, save, dryrun }: { h: Hook; load: Load; save: Save; dryrun: DryRun }) {
  return (
    <div className="proj-row hk-row">
      <div className="hk-main">
        <span className="mono hk-name">{h.name}</span>
        <span className="hk-tag">{h.scope === "home" ? "Home" : "Project"}</span>
        {h.failing && <span className="hk-state hk-bad">Failing</span>}
        {h.shadowed && <span className="hk-state hk-shadow">Shadowed — a project file of the same name wins</span>}
        <span className="hk-when">Fired {when(h.lastFired, "never")}</span>
        {h.lastDecision && <span className="hk-when">Last decision: {h.lastDecision}</span>}
      </div>
      {h.failing && h.error && <p className="err hk-why">{h.error}</p>}
      <Source path={h.path} event={h.event} load={load} save={save} dryrun={dryrun} />
    </div>
  );
}

function Decision({ fire }: { fire: Fire }) {
  if (fire.error) return <span className="hk-state hk-bad">Errored — {fire.error}</span>;
  if (!fire.decision) return <span className="hk-when">Passed through</span>;
  const loud = fire.decision === "denied" || fire.decision === "blocked";
  return (
    <span className={"hk-state " + (loud ? "hk-bad" : "hk-shadow")}>
      {fire.decision[0].toUpperCase() + fire.decision.slice(1)}
    </span>
  );
}

export function HooksView({ data, onBack, load = hooksApi.read, save = hooksApi.write, dryrun = hooksApi.dryrun }: {
  data: HooksData; onBack?: () => void; load?: Load; save?: Save; dryrun?: DryRun;
}) {
  const { hooks, watchers, fires } = data;
  const byEvent = useMemo(() => {
    const m = new Map<string, Hook[]>();
    for (const h of hooks) {
      if (!m.has(h.event)) m.set(h.event, []);
      m.get(h.event)!.push(h);
    }
    return [...m.entries()];
  }, [hooks]);

  const broken = watchers.filter((w) => w.failing).length + hooks.filter((h) => h.failing).length;
  const recent = useMemo(
    () => [...fires].sort((a, b) => (a.at < b.at ? 1 : a.at > b.at ? -1 : 0)),
    [fires],
  );

  return (
    <div className="thread">
      <header className="thread-head">
        <Back onBack={onBack} />
        <div className="head-main">
          <h1>Hooks</h1>
          <span className="head-repo">
            {broken > 0
              ? `${broken} failing`
              : `${watchers.length} ${watchers.length === 1 ? "watcher" : "watchers"}, ${hooks.length} ${hooks.length === 1 ? "hook" : "hooks"}`}
          </span>
        </div>
      </header>

      <div className="scroll proj-body">
        <section className="proj">
          <div className="proj-head">
            <h2>Watchers</h2>
            <span className="num proj-count">
              {watchers.length} {watchers.length === 1 ? "watcher" : "watchers"}
            </span>
          </div>
          {watchers.length === 0
            ? (
              <div className="proj-empty">
                <p className="proj-empty-title">No watchers running</p>
                <p>A watcher is a <code className="mono">.js</code> file in <code className="mono">~/.bough/watchers</code> that
                   bough runs on an interval and that can wake a session. Drop one in — say
                   <code className="mono"> ci.js</code> — and it shows up here on the next tick.</p>
              </div>
            )
            : watchers.map((w) => <WatcherRow key={w.path} w={w} load={load} save={save} />)}
        </section>

        <section className="proj">
          <div className="proj-head">
            <h2>Hooks</h2>
            <span className="num proj-count">
              {hooks.length} {hooks.length === 1 ? "hook" : "hooks"} across {byEvent.length}{" "}
              {byEvent.length === 1 ? "event" : "events"}
            </span>
          </div>
          {byEvent.length === 0
            ? (
              <div className="proj-empty">
                <p className="proj-empty-title">No hooks installed</p>
                <p>A hook is a <code className="mono">.js</code> file in <code className="mono">~/.bough/hooks</code> (yours
                   everywhere) or <code className="mono">.bough/hooks</code> in a repo (that repo only). The file name is the
                   hook name; the event it listens for comes from the file itself.</p>
              </div>
            )
            : byEvent.map(([event, list]) => (
              <div key={event} className="hk-event">
                <h3 className="mono hk-event-name">{event}</h3>
                {list.map((h) => <HookRow key={h.path} h={h} load={load} save={save} dryrun={dryrun} />)}
              </div>
            ))}
        </section>

        <section className="proj">
          <div className="proj-head">
            <h2>Recent fires</h2>
            <span className="num proj-count">newest first</span>
          </div>
          {recent.length === 0
            ? <p className="proj-none">Nothing has fired yet. Once a hook runs, every call lands here with what it decided.</p>
            : recent.map((f, i) => (
              <div key={`${f.at}-${f.name}-${i}`} className="proj-row hk-row">
                <div className="hk-main">
                  <span className="num hk-when hk-time">{clock(f.at)}</span>
                  <span className="mono hk-name">{f.name}</span>
                  <span className="mono hk-when">{f.event}</span>
                  <span className="num hk-when">{f.ms}ms</span>
                  <Decision fire={f} />
                </div>
              </div>
            ))}
        </section>
      </div>
    </div>
  );
}

/** The live view: polls GET /api/hooks and hands the result to HooksView. */
export function HooksPage({ onBack }: { onBack?: () => void }) {
  const [data, setData] = useState<HooksData | null>(null);
  const [err, setErr] = useState("");

  const refresh = useCallback(() => {
    hooksApi.all().then((d) => { setData(d); setErr(""); })
      .catch((e: unknown) => setErr(e instanceof Error ? e.message : String(e)));
  }, []);

  useEffect(() => { refresh(); const t = setInterval(refresh, POLL_MS); return () => clearInterval(t); }, [refresh]);

  if (!data) {
    return (
      <div className="thread empty">
        <div>
          <h1>{err ? "Hooks did not load" : "Loading hooks…"}</h1>
          {err && <p>{err}</p>}
        </div>
      </div>
    );
  }
  return <HooksView data={data} onBack={onBack} />;
}
