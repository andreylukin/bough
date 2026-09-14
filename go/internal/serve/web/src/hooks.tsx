import { useCallback, useEffect, useId, useMemo, useState } from "react";
import { Back } from "./app";
import { EmptySection, Pending } from "./context";
import { plainTitle } from "./render";

// Shared wire types for the Hooks page and per-turn inspection.
export interface Hook {
  description?: string;
  id: string;
  off: boolean;
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
  id: string;
  off: boolean;
  name: string;
  path: string;
  every: string;
  failing: boolean;
  error: string;
  lastRun: string | null;
  lastWoke: string | null;
}

export interface Fire {
  description?: string;
  at: string;
  session: string;
  event: string;
  name: string;
  ms: number;
  decision: string;
  error: string;
  notice: string;
  truncated: string[];
  path?: string;
  input?: unknown;
  output?: unknown;
  inputBytes?: number;
  outputBytes?: number;
  inputTruncated?: boolean;
  outputTruncated?: boolean;
  inputError?: string;
  outputError?: string;
}

/**
 * One rules file. `kind` is what the file does: prose is always in
 * context, scoped only applies to the paths in `globs`, and a gate can
 * refuse a tool call. A home rule and a repo rule of the same name both
 * apply — they stack, which is why the two scopes are shown apart.
 */
export interface Rule {
  id: string;
  name: string;
  path: string;
  scope: "home" | "repo";
  kind: "prose" | "scoped" | "gate";
  globs: string[];
  off: boolean;
}

export interface Plugin {
  id: string;
  name: string;
  marketplace: string;
  version: string;
  scope: "user" | "project";
  projectPath: string;
  installPath: string;
  present: boolean;
  skills: string[];
  commands: string[];
  off: boolean;
}

export interface HooksData {
  hooks: Hook[];
  watchers: Watcher[];
  fires: Fire[];
  rules: Rule[];
  plugins: Plugin[];
}

const clock = (iso: string) =>
  new Date(iso).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
const when = (iso: string | null, never: string) => (iso ? clock(iso) : never);
const hms = (iso: string) => new Date(iso).toLocaleTimeString([], { hour12: false, hour: "2-digit", minute: "2-digit", second: "2-digit" });
/** An audit timestamp: date, time to the second, and the zone's offset. */
const stamp = (iso: string) => new Date(iso).toLocaleString([], { year: "numeric", month: "short", day: "numeric",
  hour12: false, hour: "2-digit", minute: "2-digit", second: "2-digit", timeZoneName: "shortOffset" });
const day = (iso: string) => new Date(iso).toLocaleDateString([], { weekday: "short", month: "short", day: "numeric" });

/** A fire nobody needs to read twice: it passed, and said nothing. */
const quiet = (f: Fire) => !f.decision && !f.error && !f.notice && !(f.truncated?.length > 0);

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

/**
 * Turning something off is one endpoint for every kind of thing, so the
 * id it takes carries its kind: `hook:`, `watcher:`, `rule:`, `plugin:`,
 * `skill:`. The server writes ~/.bough/off.yml and rejects a kind it
 * does not know.
 */
export type OffKind = "hook" | "watcher" | "rule" | "plugin" | "skill";
export type SetOff = (id: string, off: boolean) => Promise<void>;

export const offId = (kind: OffKind, id: string) => `${kind}:${id}`;

export const setOffApi: SetOff = (id, off) =>
  req<{ ok: true }>("/api/off", { method: "POST", body: JSON.stringify({ id, off }) }).then(() => {});

/**
 * The on/off control every listed thing gets. Everything is on until
 * someone says otherwise, so the resting word is "Turn off"; an item
 * that is off keeps its row, says the word "Off" beside its name, and
 * offers the way back.
 */
export function OffToggle({ id, off, what, setOff, onChange }: {
  id: string; off: boolean; what: string; setOff: SetOff; onChange: (off: boolean) => void;
}) {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");

  const flip = () => {
    setBusy(true);
    setErr("");
    setOff(id, !off)
      .then(() => onChange(!off))
      .catch((e: unknown) => setErr(e instanceof Error ? e.message : String(e)))
      .finally(() => setBusy(false));
  };

  return (
    <>
      <button className="hk-offbtn hk-act" disabled={busy} onClick={flip}>
        {off ? "Enable globally" : "Disable globally"}
        <span className="visually-hidden"> {what}</span>
      </button>
      {err && <span className="hk-state hk-bad">Did not save — {err}</span>}
    </>
  );
}

/** The word an off row carries, so the state is never dimming alone. */
function OffWord({ off }: { off: boolean }) {
  return off ? <span className="hk-state hk-offword">Off</span> : null;
}

export type Load = (path: string) => Promise<string>;
export type Save = (path: string, body: string) => Promise<void>;
export type DryRun = (path: string, event: string) => Promise<{ result: unknown; error: string; ms: number }>;

/**
 * The file behind one watcher or hook, opened in place. It loads on
 * first expand rather than with the list: most visits never open one,
 * and the list is the answer to "is this thing even loaded?".
 */
function Source({ path, event, load, save, dryrun, definition = false }: {
  path: string; event?: string; load: Load; save: Save; dryrun?: DryRun; definition?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [body, setBody] = useState<string | null>(null);
  const [note, setNote] = useState("");
  const [err, setErr] = useState("");
  const [saving, setSaving] = useState(false);
  const id = useId();

  const read = () => {
    setErr("");
    load(path).then(setBody).catch((e: unknown) => setErr(e instanceof Error ? e.message : String(e)));
  };

  const expand = () => {
    const next = !open;
    setOpen(next);
    if (!next || body !== null) return;
    read();
  };

  return (
    <div className={definition ? "hk-src hk-definition" : "hk-src"}>
      <div className="hk-source-head">
        {definition && <span className="mono hk-path">{path}</span>}
        <button className="hk-toggle" aria-expanded={open} aria-controls={id} onClick={expand}>
          {definition ? (open ? "Hide definition" : "Open definition") : (open ? "Hide file" : "Open file")}
        </button>
      </div>
      {definition && <p className="hk2-note">Current file; may differ from this run. Saving affects future runs.</p>}
      <div className="hk-panel" id={id} hidden={!open}>
        <p className="mono hk-path">{path}</p>
        {err && <p className="err" role="alert">{err}</p>}
        {body === null
          ? err ? <button className="btn" onClick={read}>Retry</button> : <p className="proj-none">Loading…</p>
          : (
            <>
              <label className="visually-hidden" htmlFor={`${id}-body`}>File contents</label>
              <textarea id={`${id}-body`} className="hk-edit mono" rows={10} spellCheck={false}
                        value={body} disabled={saving} onChange={(e) => { setBody(e.target.value); setNote(""); }} />
              <div className="hk-acts">
                <button className="btn btn-primary" disabled={saving} onClick={() => {
                  setNote(""); setErr(""); setSaving(true);
                  save(path, body).then(() => setNote("Saved.")).catch((e: unknown) =>
                    setErr(e instanceof Error ? e.message : String(e))).finally(() => setSaving(false));
                }}>{saving ? "Saving…" : "Save"}</button>
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

function Payload({ fire, side }: { fire: Partial<Fire>; side: "input" | "output" }) {
  const value = fire[side];
  const bytes = fire[side === "input" ? "inputBytes" : "outputBytes"];
  const cut = fire[side === "input" ? "inputTruncated" : "outputTruncated"];
  const error = fire[side === "input" ? "inputError" : "outputError"];
  const label = side === "input" ? "Input" : "Output";
  return (
    <section className="hk-payload" aria-label={label}>
      <h4>{label}{bytes !== undefined && <span className="num hk-when"> · {bytes.toLocaleString()} bytes</span>}</h4>
      {error ? <p className="hk-bad">Capture error — {error}</p>
        : cut ? <p className="hk-when">Oversize — omitted in full at the 64 KiB capture cap. No partial payload was stored.</p>
        : value === undefined ? <p className="hk-when">Unavailable — this record has no captured {side} (legacy records did not capture payloads).</p>
        : value === null ? <p className="hk-when"><code className="mono">null</code> — {side === "output" ? "no output returned" : "no input"}</p>
        : <pre className="mono" tabIndex={0}>{JSON.stringify(value, null, 2)}</pre>}
    </section>
  );
}

/** Uses only recorded identity: a name cannot distinguish home, project or Go hooks. */
export function FireInspection({ fire, load = hooksApi.read, save = hooksApi.write, showDefinition = true }: {
  fire: Partial<Fire>; load?: Load; save?: Save; showDefinition?: boolean;
}) {
  return (
    <div className="hk-inspect">
      <p className="hk2-note hk-description">{fire.description || "No description recorded for this run."}</p>
      <div className="hk-io"><Payload fire={fire} side="input" /><Payload fire={fire} side="output" /></div>
      {showDefinition && (fire.path
        ? <Source key={fire.path} path={fire.path} load={load} save={save} definition />
        : <p className="hk2-note">Definition unavailable — no file path was recorded. Legacy and Go hooks are not matched to files by name.</p>)}
    </div>
  );
}

/**
 * One listed thing. Every row in this view is the same shape — state,
 * name, a demoted line of facts, and the controls — so the eye can run
 * down a column instead of re-reading each row's layout. Anything
 * secondary (the file, its globs, what a plugin contributes) lives
 * behind the row's own disclosure rather than on the page at all times.
 */
function Row({ name, state, tags, facts, detail, actions, off, alert }: {
  name: string;
  state?: { word: string; tone: "ok" | "bad" | "warn" };
  tags?: string[];
  facts?: React.ReactNode;
  detail?: React.ReactNode;
  actions?: React.ReactNode;
  off: boolean;
  alert?: string;
}) {
  return (
    <div className={"hk2-row" + (off ? " hk2-off" : "")}>
      <div className="hk2-line">
        {state && <span className={"hk2-state hk2-" + state.tone}>{state.word}</span>}
        <span className="mono hk2-name">{name}</span>
        {(tags ?? []).map((t) => <span key={t} className="hk2-tag">{t}</span>)}
        <span className="hk2-facts">{facts}</span>
        <span className="hk2-actions">{actions}</span>
      </div>
      {alert && <p className="hk2-alert">{alert}</p>}
      {detail}
    </div>
  );
}

function WatcherRow({ w, off, setOff, onOff, load, save }: {
  w: Watcher; off: boolean; setOff: SetOff; onOff: (off: boolean) => void; load: Load; save: Save;
}) {
  return (
    <Row
      name={w.name}
      off={off}
      state={w.failing ? { word: "Failing", tone: "bad" } : { word: "Healthy", tone: "ok" }}
      facts={<>every {w.every} · ran {when(w.lastRun, "never")} · woke a session {when(w.lastWoke, "never")}</>}
      alert={w.failing ? w.error : ""}
      actions={<>
        <OffToggle id={offId("watcher", w.id)} off={off} what={`the watcher ${w.name}`}
                   setOff={setOff} onChange={onOff} />
      </>}
      detail={<Source path={w.path} load={load} save={save} />}
    />
  );
}

function HookRow({ h, latest, off, setOff, onOff, load, save, dryrun }: {
  h: Hook; latest?: Fire; off: boolean; setOff: SetOff; onOff: (off: boolean) => void;
  load: Load; save: Save; dryrun: DryRun;
}) {
  return (
    <Row
      name={h.name}
      off={off}
      state={h.failing ? { word: "Failing", tone: "bad" }
           : h.shadowed ? { word: "Shadowed", tone: "warn" }
           : undefined}
      tags={[h.scope === "home" ? "Home" : "Project"]}
      facts={h.lastDecision
        ? <>last decided {when(h.lastFired, "never")} · {h.lastDecision}</>
        : <>no decisions recorded</>}
      alert={h.failing ? h.error : h.shadowed ? "A project file of the same name wins over this one." : ""}
      actions={<OffToggle id={offId("hook", h.id)} off={off} what={`the hook ${h.name}`}
                          setOff={setOff} onChange={onOff} />}
      detail={<>
        <p className="hk2-note hk-description">{h.description || "No description provided. Add a // Description: comment at the top of the hook file."}</p>
        <Source path={h.path} event={h.event} load={load} save={save} dryrun={dryrun} definition />
        {latest ? <details className="hk2-more">
          <summary>Latest recorded input / output · {clock(latest.at)}</summary>
          <div className="hk2-more-body">
            <p className="hk2-note">{stamp(latest.at)} · {latest.event} · {latest.ms}ms · <Decision fire={latest} />{latest.session && <> · <a className="hk-session" href={`#/s/${latest.session}`} title={latest.session}>{latest.session.slice(-8)}</a></>}</p>
            <FireInspection fire={latest} load={load} save={save} showDefinition={false} />
          </div>
        </details> : <p className="hk2-note">Input / output never captured for this file in the available history.</p>}
      </>}
    />
  );
}

const kindWord: Record<Rule["kind"], string> = {
  prose: "Prose — always in context",
  scoped: "Scoped — only the paths below",
  gate: "Gate — can refuse a tool call",
};

/** Exported so the per-session Context panel shows a rule the same way. */
export function RuleRow({ r, off, setOff, onOff, load, save }: {
  r: Rule; off: boolean; setOff: SetOff; onOff: (off: boolean) => void; load: Load; save: Save;
}) {
  return (
    <Row
      name={r.name}
      off={off}
      tags={[r.scope === "home" ? "Home" : "Repo"]}
      facts={<>
        {kindWord[r.kind]}
        {r.kind === "scoped" && r.globs.length > 0 && (
          <span className="mono hk2-globs">{r.globs.join("  ")}</span>
        )}
      </>}
      actions={<OffToggle id={offId("rule", r.id)} off={off} what={`the rule ${r.name}`}
                          setOff={setOff} onChange={onOff} />}
      detail={<Source path={r.path} load={load} save={save} />}
    />
  );
}

function PluginRow({ p, off, setOff, onOff }: {
  p: Plugin; off: boolean; setOff: SetOff; onOff: (off: boolean) => void;
}) {
  const gives = [
    ...p.skills.map((s) => ["skill", s] as const),
    ...p.commands.map((c) => ["command", c] as const),
  ];
  return (
    <Row
      name={p.name}
      off={off}
      state={p.present ? undefined : { word: "Not present", tone: "bad" }}
      tags={[p.scope === "user" ? "User" : "Project"]}
      facts={<>
        <span className="num">v{p.version}</span> · {p.marketplace} ·{" "}
        {gives.length === 0 ? "no skills or commands"
          : `${p.skills.length} skills, ${p.commands.length} commands`}
      </>}
      alert={p.present ? "" : "Nothing is installed at its path, so it contributes nothing."}
      actions={<OffToggle id={offId("plugin", p.id)} off={off} what={`the plugin ${p.name}`}
                          setOff={setOff} onChange={onOff} />}
      detail={gives.length === 0 && !p.projectPath ? undefined : (
        <details className="hk2-more">
          <summary>What it brings</summary>
          <div className="hk2-more-body">
            {p.scope === "project" && p.projectPath && (
              <p className="hk2-note">Only inside <span className="mono">{p.projectPath}</span></p>
            )}
            <p className="mono hk2-path">{p.installPath}</p>
            <ul className="hk2-gives">
              {gives.map(([what, name]) => (
                <li key={`${what}-${name}`}>
                  <span className="mono hk2-give">{what === "skill" ? `/${name}` : name}</span>
                  <span className="hk2-facts"> {what}</span>
                </li>
              ))}
            </ul>
          </div>
        </details>
      )}
    />
  );
}

function Decision({ fire }: { fire: Fire }) {
  if (fire.error) return <span className="hk-state hk-bad">Errored — {fire.error}</span>;
  // No decision is the hook letting the call through unchanged, not a skipped hook.
  if (!fire.decision) return <span className="hk-when">Passed through · no decision</span>;
  const loud = fire.decision === "denied" || fire.decision === "blocked";
  return (
    <span className={"hk-state " + (loud ? "hk-bad" : "hk-shadow")}>
      {fire.decision[0].toUpperCase() + fire.decision.slice(1)}
    </span>
  );
}

/** One kind with nothing configured: where it goes, copyable, and how, behind a click. */
function SetupItem({ title, path, children }: { title: string; path: string; children: React.ReactNode }) {
  return (
    <details className="ctx-empty hk-setup-item">
      <summary>
        <span className="hk-setup-name">{title}</span>
        <code className="mono hk-path">{path}</code>
        <span className="link">Add {title.toLowerCase()}…</span>
      </summary>
      <p>{children}{" "}<button className="link" onClick={() => void navigator.clipboard?.writeText(path)}>Copy path</button></p>
    </details>
  );
}

/**
 * What the user has just turned off or on, over what the last poll said.
 * The poll catches up within seconds; until it does, the row must show
 * the state the click asked for.
 */
export function useOffs() {
  const [offs, setOffs] = useState<Record<string, boolean>>({});
  const isOff = useCallback((id: string, wire: boolean) => offs[id] ?? wire, [offs]);
  const mark = useCallback((id: string) => (off: boolean) => setOffs((o) => ({ ...o, [id]: off })), []);
  return { isOff, mark };
}

export function HooksView({ data, onBack, load = hooksApi.read, save = hooksApi.write, dryrun = hooksApi.dryrun, setOff = setOffApi,
  titles = {}, stale, onRetry }: {
  data: HooksData; onBack?: () => void; load?: Load; save?: Save; dryrun?: DryRun; setOff?: SetOff;
  /** Session id to a short title, so a fire names the work, not an id. */
  titles?: Record<string, string>;
  /** Set when the latest refresh failed: when the data on screen was read. */
  stale?: { at: number; err: string };
  onRetry?: () => void;
}) {
  const { hooks, watchers, fires, rules, plugins } = data;
  const { isOff, mark } = useOffs();
  const homeRules = rules.filter((r) => r.scope === "home");
  const repoRules = rules.filter((r) => r.scope === "repo");
  const byEvent = useMemo(() => {
    const m = new Map<string, Hook[]>();
    for (const h of hooks) {
      if (!m.has(h.event)) m.set(h.event, []);
      m.get(h.event)!.push(h);
    }
    return [...m.entries()];
  }, [hooks]);

  const active = [
    ...watchers.map((w) => !isOff(offId("watcher", w.id), w.off)),
    ...hooks.map((h) => !h.shadowed && !isOff(offId("hook", h.id), h.off)),
    ...rules.map((r) => !isOff(offId("rule", r.id), r.off)),
    ...plugins.map((p) => p.present && !isOff(offId("plugin", p.id), p.off)),
  ].filter(Boolean).length;
  // Only what is on: a failing hook that is off or shadowed runs nothing.
  const broken = watchers.filter((w) => w.failing && !isOff(offId("watcher", w.id), w.off)).length
    + hooks.filter((h) => h.failing && !h.shadowed && !isOff(offId("hook", h.id), h.off)).length;
  // The server's own `off` is the fallback, not `false`: a toggle made
  // in this tab wins, but anything already off stays counted.
  const offCount = [
    ...watchers.map((w) => [offId("watcher", w.id), w.off] as const),
    ...hooks.map((h) => [offId("hook", h.id), h.off] as const),
    ...rules.map((r) => [offId("rule", r.id), r.off] as const),
    ...plugins.map((p) => [offId("plugin", p.id), p.off] as const),
  ].filter(([id, wire]) => isOff(id, wire)).length;
  const recent = useMemo(
    () => [...fires].sort((a, b) => (a.at < b.at ? 1 : a.at > b.at ? -1 : 0)),
    [fires],
  );
  // A fire with no file behind it came from a handler compiled into bough.
  const builtin = useMemo(() => fires.filter((f) => !f.path).length, [fires]);
  const empty = [
    watchers.length === 0 && "Watchers", byEvent.length === 0 && "Hooks",
    rules.length === 0 && "Rules", plugins.length === 0 && "Plugins",
  ].filter(Boolean) as string[];
  // Runs of identical quiet fires fold to one row with ×N; anything that
  // decided, errored or left a note always keeps its own row.
  const runs = useMemo(() => {
    const out: { f: Fire; n: number; key: string; all: Fire[] }[] = [];
    recent.forEach((f, i) => {
      const last = out[out.length - 1];
      if (last && quiet(f) && quiet(last.f) && last.f.name === f.name && last.f.event === f.event
          && last.f.path === f.path && last.f.session === f.session && day(last.f.at) === day(f.at)) { last.n++; last.all.push(f); return; }
      out.push({ f, n: 1, key: `${f.at}-${f.name}-${i}`, all: [f] });
    });
    return out;
  }, [recent]);

  return (
    <div className="thread">
      <header className="thread-head page-head">
        <Back onBack={onBack} />
        <div className="head-main">
          <h1>Hooks</h1>
        </div>
      </header>

      <div className="scroll proj-body">
        {stale && (
          <p className="hk-stale" role="status">
            Stale · last updated {new Date(stale.at).toLocaleTimeString([], { hour12: false })} · {stale.err}
            {onRetry && <button className="link" onClick={onRetry}>Retry</button>}
          </p>
        )}
        {/* What needs attention, before any list: on a page of five
            sections the counts are the only thing most visits need. */}
        {/* Configured now and recorded history are different questions: a
            built-in handler records events with nothing configured at all. */}
        <div className="hk2-sum">
          <span className="hk2-sum-group">Configured now</span>
          <span><span className="hk2-sum-n">{active}</span>{" "}
            <span className="hk2-sum-lab">active</span></span>
          <span><span className={"hk2-sum-n" + (broken ? " hk2-bad" : "")}>{broken}</span>{" "}
            <span className="hk2-sum-lab">failing</span></span>
          <span><span className="hk2-sum-n">{offCount}</span>{" "}
            <span className="hk2-sum-lab">turned off</span></span>
          <span className="hk2-sum-group">Recorded history</span>
          <span><span className="hk2-sum-n">{recent.length}</span>{" "}
            <span className="hk2-sum-lab">events</span></span>
          <span><span className="hk2-sum-n">{builtin}</span>{" "}
            <span className="hk2-sum-lab">from built-in handlers</span></span>
        </div>
        {recent.length === 0
          ? <EmptySection title="Recent decisions">Nothing recorded yet. Every time a hook runs — passing a call through, blocking, rewriting, throwing or leaving a note — it lands here.</EmptySection>
          : (
        <section className="proj">
          <div className="proj-head">
            <h2>Recent decisions</h2>
            <span className="num proj-count">newest first</span>
          </div>
          <div className="hk-main hk-cols" aria-hidden="true">
            <span>Time</span><span>Hook</span><span>Session</span><span>Event</span><span>Took</span><span>Outcome</span><span />
          </div>
          {runs.map(({ f, n, key, all }, i) => (
              <div key={key} className="hk-fire">
                {(i === 0 || day(runs[i - 1].f.at) !== day(f.at)) && <h3 className="hk-day">{day(f.at)}</h3>}
                {/* The row opens: a folded run lists every fire in it, and on a
                    phone the event and timing live here instead of the row. */}
                <details className={"hk-fold" + (n > 1 ? " hk-group" : "")}>
                <summary className="hk-main" title={new Date(f.at).toString()}>
                  <span className="num hk-when hk-time">{hms(f.at)}</span>
                  <span className="mono hk-name">{f.name}</span>
                  {f.session
                    ? <a className="link hk-when hk-sess" href={`#/s/${f.session}`} title={f.session}>{titles[f.session] || f.session.slice(0, 8)}</a>
                    : <span />}
                  <span className="mono hk-when">{f.event}</span>
                  <span className="num hk-when">{f.ms}ms</span>
                  <span className="hk-dec"><Decision fire={f} />{n > 1 && <span className="num hk-when"> ×{n}</span>}</span>
                  <span className="hk-chev" aria-hidden="true">›</span>
                </summary>
                <div className="hk-fold-body">
                  {/* On a phone the row keeps time, session and decision; the rest is here. */}
                  <p className="hk-when hk-tech">
                    <span className="mono">{f.name}</span> · <span className="mono">{f.event}</span>
                    {f.session && <> · <span className="mono">{f.session}</span></>}
                  </p>
                  <ul className="hk-runs">
                    {all.map((x, j) => (
                      <li key={j}>
                        <p className="num hk-when">{stamp(x.at)} · {x.ms}ms</p>
                        <FireInspection fire={x} load={load} save={save} />
                      </li>
                    ))}
                  </ul>
                </div>
                </details>
                {f.notice && (
                  /* A notice never reached the model; this is the only
                     place it survives after the turn scrolls away. */
                  <p className="hk-notice">{f.notice}</p>
                )}
                {f.truncated?.length > 0 && (
                  <p className="hk-notice hk-cut">
                    Truncated at the 10,000-character cap: {f.truncated.join(", ")}
                  </p>
                )}
              </div>
            ))}
        </section>
          )}
        {empty.length > 0 && (
          <section className="proj hk-setup">
            <div className="proj-head">
              <h2>{empty.length === 4 ? "No custom automation configured" : `Not configured: ${empty.join(" · ")}`}</h2>
            </div>
            {watchers.length === 0 && <SetupItem title="Watchers" path="~/.bough/watchers">A <code className="mono">.js</code> file
                   bough runs on an interval that can wake a session. Drop one in — say
                   <code className="mono"> ci.js</code> — and it shows up here on the next tick.</SetupItem>}
            {byEvent.length === 0 && <SetupItem title="Hooks" path="~/.bough/hooks">A <code className="mono">.js</code> file here
                   (yours everywhere) or in <code className="mono">.bough/hooks</code> in a repo (that repo only). The file name is the
                   hook name; the event it listens for comes from the file itself.</SetupItem>}
            {rules.length === 0 && <SetupItem title="Rules" path="~/.claude/rules">A <code className="mono">.md</code> file here
                   (every repo) or in <code className="mono">.claude/rules</code> in a repo (that repo only). It appears here, and in the
                   Context panel of every session it applies to.</SetupItem>}
            {plugins.length === 0 && <SetupItem title="Plugins" path="~/.claude/settings.json">Add a marketplace to this file and install
                   a plugin; the skills and slash commands it brings are listed here.</SetupItem>}
          </section>
        )}
        {watchers.length > 0 && (
        <section className="proj">
          <div className="proj-head">
            <h2>Watchers</h2>
            <span className="num proj-count">
              {watchers.length} {watchers.length === 1 ? "watcher" : "watchers"}
            </span>
          </div>
          {watchers.map((w) => (
              <WatcherRow key={w.path} w={w} load={load} save={save} setOff={setOff}
                          off={isOff(offId("watcher", w.id), w.off)} onOff={mark(offId("watcher", w.id))} />
            ))}
        </section>
        )}

        {byEvent.length > 0 && (
        <section className="proj">
          <div className="proj-head">
            <h2>Hooks</h2>
            <span className="num proj-count">
              {hooks.length} {hooks.length === 1 ? "hook" : "hooks"} across {byEvent.length}{" "}
              {byEvent.length === 1 ? "event" : "events"}
            </span>
          </div>
          {byEvent.map(([event, list]) => (
              <div key={event} className="hk-event">
                <h3 className="mono hk-event-name">{event}</h3>
                {list.map((h) => (
                  <HookRow key={h.path} h={h} latest={recent.find((f) => !!h.path && f.path === h.path)} load={load} save={save} dryrun={dryrun} setOff={setOff}
                           off={isOff(offId("hook", h.id), h.off)} onOff={mark(offId("hook", h.id))} />
                ))}
              </div>
            ))}
        </section>
        )}

        {rules.length > 0 && (
        <section className="proj">
          <div className="proj-head">
            <h2>Rules</h2>
            <span className="num proj-count">
              {rules.length} {rules.length === 1 ? "rule" : "rules"}
            </span>
          </div>
          {(
              <>
                <div className="hk-event">
                  <h3 className="hk-event-name">Home — every repo</h3>
                  {homeRules.length === 0
                    ? <p className="proj-none">Nothing in <code className="mono">~/.claude/rules</code> yet.</p>
                    : homeRules.map((r) => (
                      <RuleRow key={r.id} r={r} load={load} save={save} setOff={setOff}
                               off={isOff(offId("rule", r.id), r.off)} onOff={mark(offId("rule", r.id))} />
                    ))}
                </div>
                <div className="hk-event">
                  <h3 className="hk-event-name">Repo — stacks on top of the home rules</h3>
                  {repoRules.length === 0
                    ? <p className="proj-none">No repo checked out here adds rules of its own.</p>
                    : repoRules.map((r) => (
                      <RuleRow key={r.id} r={r} load={load} save={save} setOff={setOff}
                               off={isOff(offId("rule", r.id), r.off)} onOff={mark(offId("rule", r.id))} />
                    ))}
                </div>
              </>
            )}
        </section>
        )}

        {plugins.length > 0 && (
        <section className="proj">
          <div className="proj-head">
            <h2>Plugins</h2>
            <span className="num proj-count">
              {plugins.length} {plugins.length === 1 ? "plugin" : "plugins"}
            </span>
          </div>
          {plugins.map((p) => (
              <PluginRow key={p.id} p={p} setOff={setOff} off={isOff(offId("plugin", p.id), p.off)} onOff={mark(offId("plugin", p.id))} />
            ))}
        </section>
        )}

      </div>
    </div>
  );
}

/** The live view: polls GET /api/hooks and hands the result to HooksView. */
export function HooksPage({ onBack, rows = [] }: { onBack?: () => void; rows?: { id: string; title?: string }[] }) {
  const [data, setData] = useState<HooksData | null>(null);
  const [err, setErr] = useState("");
  const [at, setAt] = useState(0);
  const titles = useMemo(() => Object.fromEntries(rows.map((r) => [r.id, plainTitle(r.title ?? "")])), [rows]);

  const refresh = useCallback(() => {
    hooksApi.all().then((d) => { setData(d); setErr(""); setAt(Date.now()); })
      .catch((e: unknown) => setErr(e instanceof Error ? e.message : String(e)));
  }, []);

  useEffect(() => { refresh(); const t = setInterval(refresh, POLL_MS); return () => clearInterval(t); }, [refresh]);

  if (!data) return <Pending title="Hooks" what="hooks" err={err} onBack={onBack} onRetry={refresh} />;
  return <HooksView data={data} onBack={onBack} titles={titles} stale={err ? { at, err } : undefined} onRetry={refresh} />;
}
