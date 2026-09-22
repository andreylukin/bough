import { useMemo, useState } from "react";
import { Back, ago } from "./app";
import { EmptyState, Pending, useCopied } from "./loading";
import { Markdown } from "./render";
import type { Row } from "./types";
import { hasQuestion, shownStatus } from "./status";
import { Cites, literalUnderscores, useLoad, wikiApi, type MeData, type MeSignal, type MeTriage, type TriageAction, type WikiBlock } from "./wiki";

/*
 * Me: what the person is doing today, across everything the brief agent
 * can read. The page renders topics/me/briefs/<today>.md — the standup
 * they would write, every line cited — then the rows the same run wrote
 * to signals.json, then the projects as bough sees them. It never calls
 * a source itself: Refresh asks the tick to write the brief again.
 */

const KINDS: { kind: MeSignal["kind"]; label: string }[] = [
  { kind: "needs-you", label: "Needs you" }, { kind: "moving", label: "Moving" }, { kind: "waiting", label: "Waiting on others" }, { kind: "done", label: "Done since yesterday" },
];

/** A row's identity for triage: its citation, else its address, else its title. Same rule as the skill. */
export const signalKey = (s: MeSignal): string => s.cite || s.url || s.title;

export type SignalGroup = { kind: MeSignal["kind"] | "pinned"; label: string; items: MeSignal[] };

/**
 * The signals by kind, in the page's order; an empty kind is not a group.
 * Triage is applied first: a dismissed row is gone, a pinned one leads
 * in a group of its own whatever its kind.
 */
export function groupSignals(items: MeSignal[], triage?: MeTriage): SignalGroup[] {
  const dismissed = triage?.dismissed ?? {};
  const pinned = new Set(triage?.pinned ?? []);
  const kept = items.filter((i) => !(signalKey(i) in dismissed));
  const lead = kept.filter((i) => pinned.has(signalKey(i)));
  const rest = kept.filter((i) => !pinned.has(signalKey(i)));
  const groups: SignalGroup[] = lead.length ? [{ kind: "pinned", label: "Pinned", items: lead }] : [];
  return groups.concat(KINDS.map((k) => ({ ...k, items: rest.filter((i) => i.kind === k.kind) })).filter((g) => g.items.length > 0));
}

/** Where a steering sentence lands, as the server decides it; mirrored here so the confirmation can say so at once. */
export function steerSection(text: string): "Watch" | "Not mine" {
  const t = text.trim().toLowerCase().replace(/^[-•*\s]+/, "");
  return ["ignore", "skip", "hide", "drop", "not ", "no ", "never", "stop showing", "don't", "dont"].some((w) => t.startsWith(w)) ? "Not mine" : "Watch";
}

/** One line per project from the fleet: what wants a person, what is moving, when it last spoke. */
export function projectLines(rows: Row[], names: Record<string, string> = {}): { slug: string; name: string; needsYou: number; running: number; error: number; lastAt: string }[] {
  const m = new Map<string, { slug: string; name: string; needsYou: number; running: number; error: number; lastAt: string }>();
  const known = Object.keys(names).length > 0;
  for (const r of rows) {
    if (!r.project || r.archived) continue;
    // A slug no project directory answers to (deleted, or a demo) has no page to open.
    if (known && !(r.project in names)) continue;
    const p = m.get(r.project) ?? { slug: r.project, name: names[r.project] ?? r.project, needsYou: 0, running: 0, error: 0, lastAt: "" };
    const st = shownStatus(r);
    if (hasQuestion(r)) p.needsYou++;
    else if (st === "running" || st === "queued") p.running++;
    else if (st === "error") p.error++;
    const at = r.lastAt || r.modified;
    if (at > p.lastAt) p.lastAt = at;
    m.set(r.project, p);
  }
  return [...m.values()].sort((a, b) => (b.needsYou - a.needsYou) || (b.running - a.running) || (a.lastAt < b.lastAt ? 1 : -1));
}

/** The brief's "Since yesterday" and "Today" sections as the standup thread wants them. */
export function standupText(blocks: WikiBlock[]): string {
  const out: string[] = [];
  let section = "";
  for (const b of blocks) {
    if (b.kind === "heading") { section = b.text.toLowerCase(); continue; }
    if (b.kind !== "claim") continue;
    if (section.startsWith("since")) { if (!out.includes("Yesterday:")) out.push("Yesterday:"); out.push("• " + b.text); }
    else if (section.startsWith("today")) { if (!out.includes("Today:")) out.push("Today:"); out.push("• " + b.text); }
  }
  return out.join("\n");
}

const day = (iso: string) => new Date(iso + "T12:00:00").toLocaleDateString([], { weekday: "long", month: "short", day: "numeric" });

/**
 * One row, with what can be said about it: pin, or dismiss — just this
 * one, or with a rule about its repo or author that the brief reads
 * from then on. The actions sit beside the row, not inside its link.
 */
function Signal({ s, pinned, onOpenSession, onTriage }: {
  s: MeSignal; pinned?: boolean; onOpenSession?: (id: string) => void;
  onTriage?: (action: TriageAction, s: MeSignal, rule?: string) => void;
}) {
  const [menu, setMenu] = useState(false);
  const body = (
    <>
      <span className="me-sig-src mono">{s.source}</span>
      <span className="me-sig-main">
        <span className="me-sig-title">{s.title}</span>
        {(s.note || s.project) && <span className="me-sig-note">{[s.project, s.note].filter(Boolean).join(" · ")}</span>}
      </span>
      {s.at && <span className="num me-sig-when">{ago(s.at)}</span>}
    </>
  );
  const row = s.session && onOpenSession
    ? <button type="button" className="me-sig" onClick={() => onOpenSession(s.session!)}>{body}</button>
    : s.url ? <a className="me-sig" href={s.url} target="_blank" rel="noreferrer">{body}</a>
    : <div className="me-sig">{body}</div>;
  if (!onTriage) return row;
  const rules: { label: string; rule: string }[] = [
    { label: "Just this one", rule: "" },
    ...(s.repo ? [{ label: `Nothing from ${s.repo}`, rule: `nothing from ${s.repo}` }] : []),
    ...(s.author ? [{ label: `Nothing from ${s.author}`, rule: `nothing by ${s.author}` }] : []),
  ];
  return (
    <div className={"me-sig-wrap" + (menu ? " is-open" : "")}>
      {row}
      <span className="me-sig-acts">
        <button type="button" className={"me-act" + (pinned ? " is-on" : "")} title={pinned ? "Unpin" : "Pin: keep it first until it is done"}
                aria-label={pinned ? `Unpin ${s.title}` : `Pin ${s.title}`} aria-pressed={pinned || false}
                onClick={() => onTriage(pinned ? "unpin" : "pin", s)}>
          <svg width="14" height="14" viewBox="0 0 24 24" fill={pinned ? "currentColor" : "none"} stroke="currentColor" strokeWidth="1.6" strokeLinejoin="round" aria-hidden="true"><path d="M12 3l2.6 5.4 5.9.8-4.3 4.1 1.1 5.9L12 16.4 6.7 19.2l1.1-5.9-4.3-4.1 5.9-.8z" /></svg>
        </button>
        <button type="button" className="me-act" title="Dismiss" aria-label={`Dismiss ${s.title}`} aria-haspopup="menu" aria-expanded={menu}
                onClick={() => setMenu((v) => !v)}>
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" aria-hidden="true"><path d="M6 6l12 12M18 6L6 18" /></svg>
        </button>
      </span>
      {menu && (
        <div className="me-menu" role="menu" aria-label="Dismiss">
          {rules.map((r) => (
            <button key={r.label} type="button" role="menuitem" className="me-menu-item"
                    onClick={() => { setMenu(false); onTriage("dismiss", s, r.rule || undefined); }}>{r.label}</button>
          ))}
        </div>
      )}
    </div>
  );
}

/**
 * The steering line: a sentence for the brief, filed under Watch or Not
 * mine in the profile. The reader is the agent, so any wording works;
 * the confirmation says where it went and that the next brief reads it.
 */
function Steer({ onSteer }: { onSteer: (text: string) => Promise<string> }) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [said, setSaid] = useState("");
  const [err, setErr] = useState("");
  const send = async () => {
    const t = text.trim();
    if (!t || busy) return;
    setBusy(true); setErr(""); setSaid("");
    try { const section = await onSteer(t); setSaid(`Filed under ${section}. The next brief reads it.`); setText(""); }
    catch (e) { setErr(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(false); }
  };
  const to = text.trim() ? steerSection(text) : "";
  return (
    <form className="me-steer" onSubmit={(e) => { e.preventDefault(); void send(); }}>
      <input className="field me-steer-box" value={text} aria-label="Tell the brief what to watch or ignore" disabled={busy}
             placeholder="Tell the brief what to watch or ignore… (“watch Bradley’s provenance PR”, “ignore uni-route-availability”)"
             onChange={(e) => { setText(e.target.value); setSaid(""); }} />
      <button type="submit" className="btn btn-sm" disabled={!text.trim() || busy}>{busy ? "Filing…" : to ? `Add to ${to}` : "Add"}</button>
      {said && <span className="me-steer-said" role="status">{said}</span>}
      {err && <span className="err me-steer-said" role="alert">{err}</span>}
    </form>
  );
}

export function MePage({ data, error, rows = [], projectNames = {}, refreshing, onRefresh, onRetry, onOpenSession, onOpenProject, onOpenPage, onBack, onTriage, onSteer }: {
  data: MeData | null; error?: string; rows?: Row[]; projectNames?: Record<string, string>;
  refreshing?: boolean; onRefresh?: () => void; onRetry?: () => void;
  onOpenSession?: (id: string) => void; onOpenProject?: (slug: string) => void; onOpenPage?: (path: string) => void; onBack?: () => void;
  /** Pin or dismiss a row, with a rule for the profile when the dismissal should teach one. */
  onTriage?: (action: TriageAction, s: MeSignal, rule?: string) => void;
  /** A sentence for the brief; resolves to the profile section it was filed under. */
  onSteer?: (text: string) => Promise<string>;
}) {
  const [copied, copy] = useCopied();
  const groups = useMemo(() => groupSignals(data?.signals?.items ?? [], data?.triage), [data?.signals, data?.triage]);
  const pinnedKeys = useMemo(() => new Set(data?.triage?.pinned ?? []), [data?.triage]);
  const dismissedCount = useMemo(() => {
    const items = data?.signals?.items ?? [];
    const d = data?.triage?.dismissed ?? {};
    return items.filter((i) => signalKey(i) in d).length;
  }, [data?.signals, data?.triage]);
  const projects = useMemo(() => projectLines(rows, projectNames), [rows, projectNames]);
  const [hot, setHot] = useState<number | null>(null);
  if (!data) {
    return <div className="thread me-loading">{error ? <EmptyState glyph="search" title="Couldn’t read the brief" primary={false} action={onRetry ? { label: "Retry", onClick: onRetry } : undefined}>{error}</EmptyState> : <Pending what="Brief" />}</div>;
  }
  const page = data.page;
  const standup = page ? standupText(page.blocks) : "";
  return (
    <div className="me">
      <section className="me-main">
        <header className="me-bar">
          <Back onBack={onBack} />
          <h1 className="me-title">{day(data.date)}</h1>
          {/* The freshness stamp sits with the actions, one line, so the bar keeps the rail's baseline. */}
          {page && (
            <span className="me-sub num">
              {data.stale ? <span className="me-stale">Last brief is from {day(data.path!.slice(-13, -3))} · </span> : null}as of {data.asOf ? ago(data.asOf) : "—"} ago
            </span>
          )}
          {standup && <button type="button" className="btn btn-sm" onClick={() => copy(standup)}>{copied ? "Copied" : "Copy standup"}</button>}
          {onRefresh && data.hasProfile && <button type="button" className="btn btn-sm btn-primary" disabled={refreshing} onClick={onRefresh}>{refreshing ? "Writing…" : "Refresh"}</button>}
        </header>

        <div className="scroll me-body">
          {!data.hasProfile && (
            <EmptyState title="Tell the brief whose work this is" primary={false}
                        action={onOpenPage ? { label: "Write your profile", onClick: () => onOpenPage("topics/me/profile.md") } : undefined}>
              The brief reads <span className="mono">~/.bough/wiki/topics/me/profile.md</span> first: your teams, channels, ids, the repos you own. Without it nothing is written.
            </EmptyState>
          )}
          {data.hasProfile && !page && (
            <div className="me-none">
              <EmptyState title="No brief yet today" primary={false} action={onRefresh ? { label: "Write it now", onClick: onRefresh } : undefined}>
                The tick writes one within half an hour of your working-hours start.
              </EmptyState>
            </div>
          )}
          {page && (
            <article className="me-brief" aria-label="Brief">
              {page.blocks.map((b, i) => {
                if (b.kind === "heading") return <h2 key={i} className="me-h">{b.text}</h2>;
                if (b.kind === "lede") return <p key={i} className="me-lede"><Markdown text={literalUnderscores(b.text)} /><Cites block={b} onCite={() => {}} onHot={(on) => setHot(on ? i : null)} /></p>;
                if (b.kind === "claim") return (
                  <div key={i} className={"me-claim" + (b.bullet ? " me-bullet" : "") + (hot === i ? " is-hot" : "")}>
                    {/* One span for the text and its chips: the bullet grid has two cells, the dot and this. */}
                    <span className="me-claim-body"><Markdown text={literalUnderscores(b.text)} /><Cites block={b} onCite={() => {}} onHot={(on) => setHot(on ? i : null)} /></span>
                  </div>
                );
                return null;
              })}
            </article>
          )}
          {page && onSteer && data.hasProfile && <Steer onSteer={onSteer} />}
          {groups.map((g) => (
            <section key={g.kind} className="me-group" data-kind={g.kind} aria-label={g.label}>
              <h2 className="eyebrow me-group-h">{g.label} <span className="num">{g.items.length}</span></h2>
              {g.items.map((s) => <Signal key={signalKey(s)} s={s} pinned={pinnedKeys.has(signalKey(s))} onOpenSession={onOpenSession} onTriage={onTriage} />)}
            </section>
          ))}
          {dismissedCount > 0 && <p className="me-dim me-dismissed">{dismissedCount} dismissed {dismissedCount === 1 ? "row" : "rows"} hidden. Rules you taught are in your profile.</p>}
        </div>
      </section>

      <aside className="me-rail" aria-label="Projects and sources">
        <section className="me-sec">
          <h3 className="eyebrow me-sec-h">Projects</h3>
          {projects.length === 0 && <p className="me-dim">No project sessions yet.</p>}
          {projects.map((p) => (
            <button key={p.slug} type="button" className="me-proj" onClick={onOpenProject ? () => onOpenProject(p.slug) : undefined}>
              <span className="me-proj-name">{p.name}</span>
              <span className="me-proj-counts num">
                {p.needsYou > 0 && <span className="me-c me-c-you">{p.needsYou} need{p.needsYou === 1 ? "s" : ""} you</span>}
                {p.running > 0 && <span className="me-c me-c-run">{p.running} running</span>}
                {p.error > 0 && <span className="me-c me-c-err">{p.error} error{p.error === 1 ? "" : "s"}</span>}
                {p.needsYou + p.running + p.error === 0 && <span className="me-dim">quiet</span>}
              </span>
              {p.lastAt && <span className="num me-dim">{ago(p.lastAt)}</span>}
            </button>
          ))}
        </section>
        <section className="me-sec">
          <h3 className="eyebrow me-sec-h">Sources</h3>
          {!data.signals?.sources?.length && <p className="me-dim">Sources appear after the first brief.</p>}
          {data.signals?.sources?.map((s) => (
            <p key={s.name} className={"me-src" + (s.ok ? "" : " is-off")}>
              <span className="mono">{s.name}</span>
              {s.ok
                ? <span className="me-dim">{s.at ? ago(s.at) : "ok"}</span>
                : <span className="me-src-off">
                    <span className="me-dim">{s.error || "off"}</span>
                    {onOpenPage && <button type="button" className="link" title={s.error || "off"} onClick={() => onOpenPage("topics/me/profile.md")}>{/expired|invalid|revoked/i.test(s.error || "") ? "reconnect" : "connect"}</button>}
                  </span>}
            </p>
          ))}
        </section>
        {data.days.length > 1 && (
          <section className="me-sec">
            <h3 className="eyebrow me-sec-h">Earlier</h3>
            {data.days.slice(1, 8).map((d) => (
              <button key={d} type="button" className="link me-day" onClick={onOpenPage ? () => onOpenPage(`topics/me/briefs/${d}.md`) : undefined}>{day(d)}</button>
            ))}
          </section>
        )}
      </aside>
    </div>
  );
}

/** The page with its fetching: the brief polls, since the tick rewrites it behind the page's back. */
export function MeView({ rows, projectNames, onOpenSession, onOpenProject, onOpenPage, onBack }: {
  rows: Row[]; projectNames?: Record<string, string>;
  onOpenSession?: (id: string) => void; onOpenProject?: (slug: string) => void; onOpenPage?: (path: string) => void; onBack?: () => void;
}) {
  const me = useLoad(wikiApi.me, "me", 30_000);
  const [refreshing, setRefreshing] = useState(false);
  const refresh = async () => {
    setRefreshing(true);
    try { await wikiApi.refreshMe(); } finally { setTimeout(() => { setRefreshing(false); void me.reload(); }, 20_000); }
  };
  // Triage lands at once: the answer carries the new file, and the page
  // shows it before the next poll rather than after.
  const [triage, setTriage] = useState<MeTriage | null>(null);
  const data = me.data && triage ? { ...me.data, triage } : me.data;
  const onTriage = (action: TriageAction, s: MeSignal, rule?: string) => {
    void wikiApi.triage(action, signalKey(s), rule).then(setTriage).then(() => me.reload());
  };
  return <MePage data={data} error={me.err} rows={rows} projectNames={projectNames} refreshing={refreshing}
                 onRefresh={() => { void refresh(); }} onRetry={me.retry} onTriage={onTriage}
                 onSteer={(text) => wikiApi.steer(text)}
                 onOpenSession={onOpenSession} onOpenProject={onOpenProject} onOpenPage={onOpenPage} onBack={onBack} />;
}
