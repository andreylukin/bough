import { useMemo, useState } from "react";
import { Back, ago } from "./app";
import { EmptyState, Pending, useCopied } from "./loading";
import { Markdown } from "./render";
import type { Row } from "./types";
import { hasQuestion, shownStatus } from "./status";
import { Cites, literalUnderscores, useLoad, wikiApi, type MeData, type MeSignal, type WikiBlock } from "./wiki";

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

/** The signals by kind, in the page's order; an empty kind is not a group. */
export function groupSignals(items: MeSignal[]): { kind: MeSignal["kind"]; label: string; items: MeSignal[] }[] {
  return KINDS.map((k) => ({ ...k, items: items.filter((i) => i.kind === k.kind) })).filter((g) => g.items.length > 0);
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

function Signal({ s, onOpenSession }: { s: MeSignal; onOpenSession?: (id: string) => void }) {
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
  if (s.session && onOpenSession) return <button type="button" className="me-sig" onClick={() => onOpenSession(s.session!)}>{body}</button>;
  if (s.url) return <a className="me-sig" href={s.url} target="_blank" rel="noreferrer">{body}</a>;
  return <div className="me-sig">{body}</div>;
}

export function MePage({ data, error, rows = [], projectNames = {}, refreshing, onRefresh, onRetry, onOpenSession, onOpenProject, onOpenPage, onBack }: {
  data: MeData | null; error?: string; rows?: Row[]; projectNames?: Record<string, string>;
  refreshing?: boolean; onRefresh?: () => void; onRetry?: () => void;
  onOpenSession?: (id: string) => void; onOpenProject?: (slug: string) => void; onOpenPage?: (path: string) => void; onBack?: () => void;
}) {
  const [copied, copy] = useCopied();
  const groups = useMemo(() => groupSignals(data?.signals?.items ?? []), [data?.signals]);
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
                if (b.kind === "lede") return <p key={i} className="me-lede"><Markdown text={literalUnderscores(b.text)} /><Cites block={b} short onCite={() => {}} onHot={(on) => setHot(on ? i : null)} /></p>;
                if (b.kind === "claim") return (
                  <div key={i} className={"me-claim" + (b.bullet ? " me-bullet" : "") + (hot === i ? " is-hot" : "")}>
                    {/* One span for the text and its chips: the bullet grid has two cells, the dot and this. */}
                    <span className="me-claim-body"><Markdown text={literalUnderscores(b.text)} /><Cites block={b} short onCite={() => {}} onHot={(on) => setHot(on ? i : null)} /></span>
                  </div>
                );
                return null;
              })}
            </article>
          )}
          {groups.map((g) => (
            <section key={g.kind} className="me-group" data-kind={g.kind} aria-label={g.label}>
              <h2 className="eyebrow me-group-h">{g.label} <span className="num">{g.items.length}</span></h2>
              {g.items.map((s, i) => <Signal key={i} s={s} onOpenSession={onOpenSession} />)}
            </section>
          ))}
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
  return <MePage data={me.data} error={me.err} rows={rows} projectNames={projectNames} refreshing={refreshing}
                 onRefresh={() => { void refresh(); }} onRetry={me.retry}
                 onOpenSession={onOpenSession} onOpenProject={onOpenProject} onOpenPage={onOpenPage} onBack={onBack} />;
}
