import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import { Back } from "./app";
import { CopyButton, EmptyState, Pending as Waiting, duration, humanError } from "./loading";
import { plainTitle, sessionTitle } from "./render";
import { STATUS } from "./status";

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
const took = (ms: number) => ms < 1 ? "<1ms" : `${ms}ms`;
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

/** "1 command", "2 skills"; zero is left out by the caller. */
const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

/** A marketplace version: "v1.2.0", or a commit hash as "commit e637cf0". */
export function versionLabel(v: string): string {
  if (/^[0-9a-f]{7,40}$/i.test(v)) return `commit ${v.slice(0, 7)}`;
  return /^v\d/i.test(v) ? v : `v${v}`;
}

/** JSON as a reader wants it: indented, with newlines inside strings shown as line breaks. */
export function formatJson(value: unknown): string {
  return JSON.stringify(value, null, 2).replace(/(?<!\\)\\n/g, "\n");
}

export type Load = (path: string) => Promise<string>;
export type Save = (path: string, body: string) => Promise<void>;
export type DryRun = (path: string, event: string) => Promise<{ result: unknown; error: string; ms: number }>;

/**
 * The file behind one watcher or hook, opened in place. It loads on
 * first expand rather than with the list: most visits never open one,
 * and the list is the answer to "is this thing even loaded?".
 */
function Source({ path, event, load, save, dryrun, definition = false, inRun = false }: {
  path: string; event?: string; load: Load; save: Save; dryrun?: DryRun; definition?: boolean;
  /** Opened from a recorded run, where the file on disk may have changed since. */
  inRun?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [body, setBody] = useState<string | null>(null);
  const [note, setNote] = useState("");
  const [err, setErr] = useState("");
  const [saving, setSaving] = useState(false);
  // Whether a read was ever sent: the hidden panel of a file nobody opened
  // is empty, not a "Loading file…" that no request stands behind.
  const [asked, setAsked] = useState(false);
  // Dry runs still out. The button stays live (a second press runs it
  // again) but says so, or a slow hook looked like a press that did nothing.
  const [running, setRunning] = useState(0);
  const id = useId();

  const read = () => {
    setAsked(true);
    setErr("");
    // A hung request must not spin forever: after 15s it becomes an error with a Retry.
    let late = false;
    const t = setTimeout(() => { late = true; setErr("The file took too long to load."); }, 15000);
    load(path).then((b) => { if (!late) setBody(b); })
      .catch((e: unknown) => { if (!late) setErr(humanError(e)); })
      .finally(() => clearTimeout(t));
  };

  const expand = () => {
    const next = !open;
    setOpen(next);
    if (!next || body !== null) return;
    read();
  };

  return (
    <SourceView id={id} path={path} definition={definition} inRun={inRun}
      state={{ open, body, note, err, saving, asked, running }}
      onToggle={expand} onRead={read}
      // A dry run's error is about the text it ran; left up after an edit it
      // read as the verdict on code nobody has run yet.
      onEdit={(text) => { setBody(text); setNote(""); setErr(""); }}
      onSave={() => {
        setNote(""); setErr(""); setSaving(true);
        save(path, body!).then(() => setNote("Saved.")).catch((e: unknown) =>
          setErr(e instanceof Error ? e.message : String(e))).finally(() => setSaving(false));
      }}
      onDryRun={dryrun && event ? () => {
        setNote(""); setErr(""); setRunning((n) => n + 1);
        dryrun(path, event).then((r) => {
          if (r.error) { setErr(r.error); return; }
          setNote(`Dry run finished in ${r.ms}ms: ${JSON.stringify(r.result)}`);
        }).catch((e: unknown) => setErr(e instanceof Error ? e.message : String(e)))
          .finally(() => setRunning((n) => n - 1));
      } : undefined} />
  );
}

/** Everything Source holds, so each of its states can be rendered on its own. */
export interface SourceState {
  open: boolean; body: string | null; note: string; err: string; saving: boolean;
  /** Whether a read was ever sent. */
  asked: boolean;
  /** Dry runs still out. */
  running: number;
}

/** Source's markup as a function of its state; Source owns the state and the requests. */
export function SourceView({ id, path, definition = false, inRun = false, state, onToggle, onRead, onEdit, onSave, onDryRun }: {
  id: string; path: string; definition?: boolean; inRun?: boolean; state: SourceState;
  onToggle: () => void; onRead: () => void; onEdit: (text: string) => void; onSave: () => void;
  /** Absent when there is no event to dry-run the file against. */
  onDryRun?: () => void;
}) {
  const { open, body, note, err, saving, asked, running } = state;
  return (
    <div className={definition ? "hk-src hk-definition" : "hk-src"}>
      <div className="hk-source-head">
        <span className="mono hk-path" title={path}>{path}</span>
        <button className="hk-toggle" aria-expanded={open} aria-controls={id} onClick={onToggle}>
          {definition ? (open ? "Hide definition" : "Open definition") : (open ? "Hide file" : "Open file")}
        </button>
      </div>
      {definition && inRun && <p className="hk2-note">Current file; may differ from this run. Saving affects future runs.</p>}
      <div className="hk-panel" id={id} hidden={!open}>
        {err && body !== null && <p className="err" role="alert">{err}</p>}
        {body === null
          ? err
            ? <p className="err hk-loaderr" role="alert">Could not open the file — {err} <button className="btn btn-sm" onClick={onRead}>Retry</button></p>
            : asked && <Waiting what="File" inline timeout={15_000} onRetry={onRead} />
          : (
            <>
              <label className="visually-hidden" htmlFor={`${id}-body`}>File contents</label>
              <textarea id={`${id}-body`} className="hk-edit mono" rows={10} spellCheck={false}
                        value={body} disabled={saving}
                        onChange={(e) => onEdit(e.target.value)} />
              <div className="hk-acts">
                <button className="btn btn-primary" disabled={saving} onClick={onSave}>{saving ? "Saving…" : "Save"}</button>
                {onDryRun && (
                  <button className="btn" aria-busy={running > 0 || undefined} onClick={onDryRun}>{running > 0 ? "Running…" : "Dry run"}</button>
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
  // What the model saw was cut at the 10,000-character cap: said once, where the output is.
  const capped = side === "output" && (fire.truncated?.length ?? 0) > 0;
  return (
    <section className="hk-payload" aria-label={label}>
      <h4>{label}{bytes !== undefined && value !== null && <span className="num hk-when"> · {bytes.toLocaleString()} bytes</span>}
        {capped && <span className="num hk-when" title={`Truncated: ${fire.truncated!.join(", ")}`}> · truncated to 10,000 bytes</span>}
        {value != null && <CopyButton text={formatJson(value)} label="Copy" className="btn btn-sm" />}</h4>
      {error ? <p className="hk-bad">Capture error — {error}</p>
        : cut ? <p className="hk-when">Oversize — omitted in full at the 64 KiB capture cap. No partial payload was stored.</p>
        : value === undefined ? <p className="hk-when">Unavailable — this record has no captured {side} (legacy records did not capture payloads).</p>
        : value === null ? <p className="hk-when">{side === "output" ? "No output returned" : "No input"}</p>
        : <pre className="mono" tabIndex={0}>{formatJson(value)}</pre>}
    </section>
  );
}

/** Uses only recorded identity: a name cannot distinguish home, project or Go hooks. */
export function FireInspection({ fire, load = hooksApi.read, save = hooksApi.write, showDefinition = true, showDescription = true }: {
  fire: Partial<Fire>; load?: Load; save?: Save; showDefinition?: boolean; showDescription?: boolean;
}) {
  return (
    <div className="hk-inspect">
      {showDescription && fire.description && <p className="hk2-note hk-description">{fire.description}</p>}
      <div className={"hk-io" + (fire.output === null ? " is-quiet" : "")}><Payload fire={fire} side="input" /><Payload fire={fire} side="output" /></div>
      {showDefinition && (fire.path
        ? <Source key={fire.path} path={fire.path} load={load} save={save} definition inRun />
        : <p className="hk2-note">Definition unavailable — no file path was recorded. Built-in handler.</p>)}
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
export function Row({ name, state, tags, facts, detail, actions, off, alert }: {
  name: string;
  state?: { word: string; tone: "ok" | "bad" | "warn" };
  tags?: string[];
  facts?: React.ReactNode;
  detail?: React.ReactNode;
  actions?: React.ReactNode;
  off: boolean;
  alert?: string;
}) {
  // Only a failure is red; healthy, shadowed and off are resting states.
  const shown = state ?? (off ? { word: "Disabled", tone: "warn" as const } : undefined);
  return (
    <div className={"hk2-row" + (off ? " hk2-off" : "")}>
      <div className="hk2-line">
        {shown && <StateWord word={shown.word} failed={shown.tone === "bad"} />}
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
      facts={<>every {duration(w.every)} · ran {when(w.lastRun, "never")} · {w.lastWoke ? `last woke a session ${clock(w.lastWoke)}` : "never woke a session"}</>}
      alert={w.failing ? w.error : ""}
      actions={<>
        <OffToggle id={offId("watcher", w.id)} off={off} what={`the watcher ${w.name}`}
                   setOff={setOff} onChange={onOff} />
      </>}
      detail={<Source path={w.path} load={load} save={save} />}
    />
  );
}

function HookRow({ h, latest, off, setOff, onOff, load, save, dryrun, titles }: {
  h: Hook; latest?: Fire; off: boolean; setOff: SetOff; onOff: (off: boolean) => void;
  load: Load; save: Save; dryrun: DryRun; titles: Record<string, string>;
}) {
  return (
    <Row
      name={h.name}
      off={off}
      state={h.failing ? { word: "Failing", tone: "bad" }
           : h.shadowed ? { word: "Shadowed", tone: "warn" }
           : undefined}
      tags={[h.scope === "home" ? "Home" : "Project"]}
      // Passing a call through records a run with no decision: "no decisions" read as "never ran".
      facts={h.lastDecision
        ? <>last decided {when(h.lastFired, "never")} · {h.lastDecision}</>
        : latest || h.lastFired
          ? <>last ran {when(latest?.at ?? h.lastFired, "never")} · passed through</>
          : <>no runs recorded</>}
      alert={h.failing ? h.error : h.shadowed ? "A project file of the same name wins over this one." : ""}
      actions={<OffToggle id={offId("hook", h.id)} off={off} what={`the hook ${h.name}`}
                          setOff={setOff} onChange={onOff} />}
      detail={<>
        {h.description && <p className="hk2-note hk-description">{h.description}</p>}
        <Source path={h.path} event={h.event} load={load} save={save} dryrun={dryrun} definition />
        {latest ? <details className="hk2-more">
          <summary>Latest recorded input / output · {clock(latest.at)}</summary>
          <div className="hk2-more-body">
            <p className="hk2-note">{stamp(latest.at)} · {latest.event} · {took(latest.ms)} · <Decision fire={latest} />{latest.session && <> · <a className="hk-session link" href={`#/s/${latest.session}`}>{sessionTitle({ id: latest.session, title: titles[latest.session] })}</a></>}</p>
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
      // Absent is one badge; its explanation is the badge's tooltip, and the toggle has nothing to act on.
      state={p.present ? undefined : { word: "Not installed", tone: "bad" }}
      tags={[p.scope === "user" ? "Home" : "Project"]}
      facts={<>
        <span className="num" title={p.version}>{versionLabel(p.version)}</span> · {p.marketplace}
        {gives.length === 0 ? " · no skills or commands"
          : " · " + [p.skills.length > 0 && plural(p.skills.length, "skill"), p.commands.length > 0 && plural(p.commands.length, "command")].filter(Boolean).join(", ")}
      </>}
      actions={p.present
        ? <OffToggle id={offId("plugin", p.id)} off={off} what={`the plugin ${p.name}`}
                     setOff={setOff} onChange={onOff} />
        : <span className="hk2-facts">Nothing installed at its path</span>}
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

/** A glyph and a sentence-case word; red only when something failed. */
export function StateWord({ word, failed = false }: { word: string; failed?: boolean }) {
  return (
    <span className={"hk2-mark " + (failed ? "is-failed" : word === "Passed" ? "is-done" : word === "Blocked" ? "is-blocked" : "is-changed")}>
      <svg className="state-mark" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5"
           strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">{(failed ? STATUS.error : word === "Passed" ? STATUS.done : STATUS.idle).glyph}</svg>
      {word}
    </span>
  );
}

/** One short word per outcome. "denied" and "blocked" are the same act, so both read Blocked. */
export function outcomeWord(fire: Pick<Fire, "error" | "decision">): string {
  if (fire.error) return "Errored";
  // No decision is the hook letting the call through unchanged, not a skipped hook.
  if (!fire.decision) return "Passed";
  if (fire.decision === "denied" || fire.decision === "blocked") return "Blocked";
  return fire.decision[0].toUpperCase() + fire.decision.slice(1);
}

function Decision({ fire }: { fire: Fire }) {
  return <StateWord word={outcomeWord(fire)} failed={!!fire.error} />;
}

/**
 * One recorded run (or a folded run of identical quiet ones). The chevron
 * is the row's button; the session link sits above it, so it stays a link.
 */
export function FireRow({ f, n, all, titles, load, save }: {
  f: Fire; n: number; all: Fire[]; titles: Record<string, string>; load: Load; save: Save;
}) {
  const [open, setOpen] = useState(false);
  const id = useId();
  const title = f.session ? sessionTitle({ id: f.session, title: titles[f.session] }) : "";
  const shared = all.every((x) => x.description === f.description);
  return (
    <div className={"hk-fire" + (open ? " hk-open" : "")}>
      <div className="hk-main" title={new Date(f.at).toString()}>
        <span className="num hk-when hk-time">{hms(f.at)}</span>
        <span className="mono hk-name" title={f.name}>{f.name}</span>
        {f.session
          ? <a className="hk-sess link" href={`#/s/${f.session}`} title={f.session}>{title}</a>
          : <span className="hk-sess" />}
        <span className="mono hk-when hk-ev" title={f.event}>{f.event}</span>
        <span className="num hk-when hk-took">{took(f.ms)}</span>
        <span className="hk-dec"><Decision fire={f} />{n > 1 && <span className="num hk-count" title={`${n} runs collapsed`}>{n}</span>}</span>
        <button className="hk-chev" aria-expanded={open} aria-controls={id} onClick={() => setOpen(!open)}>
          <svg aria-hidden="true" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round"><path d="M9 6l6 6-6 6" /></svg>
          <span className="visually-hidden">{open ? "Hide" : "Show"} input and output of {f.name}, {outcomeWord(f)}</span>
        </button>
      </div>
      {open && (
        <div className="hk-fold-body" id={id}>
          {f.error && <p className="hk2-alert">{f.error}</p>}
          {/* A notice never reached the model; this is the only place it survives after the turn scrolls away. */}
          {f.notice && <p className="hk-notice">{f.notice}</p>}
          {/* Folded runs share one hook: its purpose and definition are said once, not per run. */}
          {shared && f.description && <p className="hk2-note hk-description">{f.description}</p>}
          {!f.path && <p className="hk2-note">Definition unavailable — no file path was recorded. Built-in handler.</p>}
          {n > 1 && <p className="hk-runs-head">{n} runs, newest first</p>}
          <ul className="hk-runs">
            {all.map((x, j) => (
              <li key={j}>
                <p className="num hk-when">{stamp(x.at)} · {took(x.ms)}</p>
                <FireInspection fire={x} load={load} save={save} showDefinition={j === 0 && !!x.path} showDescription={!shared} />
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}

type Filter = "all" | "failing" | "off" | "missing";

/** One kind with nothing configured: where it goes, copyable, and how, behind a click. */
function SetupItem({ title, path, children }: { title: string; path: string; children: React.ReactNode }) {
  const [open, setOpen] = useState(false);
  const id = useId();
  return (
    <section className="proj hk-setup">
      <div className="proj-head">
        <h2>{title}</h2>
        <span className="hk2-state hk2-muted">Not configured</span>
        <button className="link" aria-expanded={open} aria-controls={id} onClick={() => setOpen(!open)}>
          Add {title.toLowerCase()}…
        </button>
      </div>
      <div className="ctx-empty hk-setup-item" id={id} hidden={!open}>
        <p><code className="mono hk-path">{path}</code>{" "}
          <CopyButton text={path} label="Copy path" /></p>
        <p>{children}</p>
      </div>
    </section>
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
  const { hooks: allHooks, watchers: allWatchers, fires, rules: allRules, plugins: allPlugins } = data;
  const { isOff, mark } = useOffs();
  const [filter, setFilter] = useState<Filter>("all");
  // What each chip keeps: failing counts only what is on, off counts what is off, missing is an absent plugin.
  const keep = (failing: boolean, off: boolean, missing = false) =>
    filter === "all" || (filter === "failing" && failing && !off) || (filter === "off" && off && !missing) || (filter === "missing" && missing);
  const watchers = allWatchers.filter((w) => keep(w.failing, isOff(offId("watcher", w.id), w.off)));
  const hooks = allHooks.filter((h) => keep(h.failing && !h.shadowed, isOff(offId("hook", h.id), h.off)));
  const rules = allRules.filter((r) => keep(false, isOff(offId("rule", r.id), r.off)));
  const plugins = allPlugins.filter((p) => keep(false, isOff(offId("plugin", p.id), p.off), !p.present));
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

  // Counts come from everything configured, never from the filtered lists.
  const total = allWatchers.length + allHooks.length + allRules.length + allPlugins.length;
  // Only what is on: a failing hook that is off or shadowed runs nothing.
  const broken = allWatchers.filter((w) => w.failing && !isOff(offId("watcher", w.id), w.off)).length
    + allHooks.filter((h) => h.failing && !h.shadowed && !isOff(offId("hook", h.id), h.off)).length;
  // The server's own `off` is the fallback, not `false`: a toggle made
  // in this tab wins, but anything already off stays counted.
  const offCount = [
    ...allWatchers.map((w) => [offId("watcher", w.id), w.off] as const),
    ...allHooks.map((h) => [offId("hook", h.id), h.off] as const),
    ...allRules.map((r) => [offId("rule", r.id), r.off] as const),
    // A plugin that is not installed says so; it is not counted as disabled.
    ...allPlugins.filter((p) => p.present).map((p) => [offId("plugin", p.id), p.off] as const),
  ].filter(([id, wire]) => isOff(id, wire)).length;
  const missing = allPlugins.filter((p) => !p.present).length;
  const chips: [Filter, string, number][] = [["all", "All", total], ["failing", "Failing", broken], ["off", "Disabled", offCount], ["missing", "Not installed", missing]];
  const recent = useMemo(
    () => [...fires].sort((a, b) => (a.at < b.at ? 1 : a.at > b.at ? -1 : 0)),
    [fires],
  );
  // A fire with no file behind it came from a handler compiled into bough.
  const builtin = useMemo(() => fires.filter((f) => !f.path).length, [fires]);
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
        {/* Filters, not a sentence: each chip wraps whole and narrows the lists below. */}
        {total > 0 && (
          <div className="hk-chips" role="group" aria-label="Show">
            {chips.map(([key, label, n]) => (
              <button key={key} className="hk-chip" aria-pressed={filter === key} onClick={() => setFilter(key)}>
                {label} <span className={"count" + (key === "failing" && n > 0 ? " is-failed" : "")}>{n}</span>
              </button>
            ))}
          </div>
        )}
        {filter !== "all" && watchers.length + hooks.length + rules.length + plugins.length === 0 && (
          <EmptyState title="Nothing matches" action={{ label: "Show all", onClick: () => setFilter("all") }}>
            No watcher, hook, rule or plugin is {chips.find(([k]) => k === filter)![1].toLowerCase()} right now.
          </EmptyState>
        )}
        {filter === "all" && (recent.length === 0
          ? (
            <section className="proj hk-decisions">
              <EmptyState title="No recorded runs yet">Every time a hook runs — passing a call through, blocking, rewriting, throwing or leaving a note — it lands here.</EmptyState>
            </section>
          )
          : (
        <section className="proj hk-decisions">
          <div className="proj-head">
            <h2>Recent decisions</h2>
            <span className="num proj-count">last {recent.length} {recent.length === 1 ? "run" : "runs"}{builtin > 0 && builtin !== recent.length && ` · ${builtin} from built-in handlers`} · newest first</span>
          </div>
          <div className="hk-table">
          <div className="hk-main hk-cols" aria-hidden="true">
            <span>Time</span><span>Hook</span><span>Session</span><span className="hk-ev">Event</span><span className="hk-took">Took</span><span className="hk-dec">Outcome</span><span />
          </div>
          {/* One wrapper per day, so the day heading stays stuck while its rows scroll. */}
          {runs.reduce<(typeof runs)[]>((days, r, i) => {
            if (i === 0 || day(runs[i - 1].f.at) !== day(r.f.at)) days.push([]);
            days[days.length - 1].push(r);
            return days;
          }, []).map((group) => (
            <div key={group[0].key} className="hk-dayrun">
            <h3 className="hk-day">{day(group[0].f.at)}</h3>
          {group.map(({ f, n, key, all }) => (
              <FireRow key={key} f={f} n={n} all={all} titles={titles} load={load} save={save} />
            ))}
            </div>
          ))}
          </div>
        </section>
          ))}
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
              {hooks.length} {hooks.length === 1 ? "hook" : "hooks"} on {byEvent.length}{" "}
              {byEvent.length === 1 ? "event type" : "event types"}
            </span>
          </div>
          {hooks.some((h) => !h.description) && (
            <p className="hk2-note">A hook describes itself here with a <code className="mono">// Description:</code> comment at the top of its file.</p>
          )}
          {byEvent.map(([event, list]) => (
              <div key={event} className="hk-event">
                <h3 className="mono hk-event-name">{event}</h3>
                {list.map((h) => (
                  <HookRow key={h.path} h={h} latest={recent.find((f) => !!h.path && f.path === h.path)} load={load} save={save} dryrun={dryrun} setOff={setOff} titles={titles}
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

        {/* What is not configured comes after what is, one section per kind. */}
        {filter === "all" && allWatchers.length === 0 && <SetupItem title="Watchers" path="~/.bough/watchers">A <code className="mono">.js</code> file
               bough runs on an interval that can wake a session. Drop one in — say
               <code className="mono"> ci.js</code> — and it shows up here on the next tick.</SetupItem>}
        {filter === "all" && allHooks.length === 0 && <SetupItem title="Hooks" path="~/.bough/hooks">A <code className="mono">.js</code> file here
               (yours everywhere) or in <code className="mono">.bough/hooks</code> in a repo (that repo only). The file name is the
               hook name; the event it listens for comes from the file itself.</SetupItem>}
        {filter === "all" && allRules.length === 0 && <SetupItem title="Rules" path="~/.claude/rules">A <code className="mono">.md</code> file here
               (every repo) or in <code className="mono">.claude/rules</code> in a repo (that repo only). It appears here, and in the
               Context panel of every session it applies to.</SetupItem>}
        {filter === "all" && allPlugins.length === 0 && <SetupItem title="Plugins" path="~/.claude/settings.json">Add a marketplace to this file and install
               a plugin; the skills and slash commands it brings are listed here.</SetupItem>}

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

  // One read at a time; a hidden tab does not poll (the server answers an unchanged ledger with a 304).
  const inFlight = useRef(false);
  const refresh = useCallback(() => {
    if (inFlight.current) return;
    inFlight.current = true;
    hooksApi.all().then((d) => { setData(d); setErr(""); setAt(Date.now()); })
      .catch((e: unknown) => setErr(e instanceof Error ? e.message : String(e)))
      .finally(() => { inFlight.current = false; });
  }, []);

  useEffect(() => { refresh(); const t = setInterval(() => { if (!document.hidden) refresh(); }, POLL_MS); return () => clearInterval(t); }, [refresh]);

  return <HooksPageView data={data} err={err} at={at} onBack={onBack} titles={titles} onRetry={refresh} />;
}

/** HooksPage's markup as a function of what it last read; `timeout` is the list's "taking too long". */
export function HooksPageView({ data, err, at, onBack, titles, onRetry, timeout }: {
  data: HooksData | null; err: string; at: number; onBack?: () => void; titles?: Record<string, string>;
  onRetry: () => void; timeout?: number;
}) {
  if (!data) {
    return (
      <div className="thread">
        <header className="thread-head page-head">
          <Back onBack={onBack} />
          <div className="head-main"><h1>Hooks</h1></div>
        </header>
        <div className="scroll proj-body"><Waiting what="Hooks" err={err} onRetry={onRetry} timeout={timeout} lines={6} /></div>
      </div>
    );
  }
  return <HooksView data={data} onBack={onBack} titles={titles} stale={err ? { at, err } : undefined} onRetry={onRetry} />;
}
