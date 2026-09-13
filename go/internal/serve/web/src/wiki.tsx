import { useCallback, useEffect, useMemo, useState } from "react";
import { Back } from "./app";
import { Markdown } from "./render";

// The wiki wire types live here, like the hooks ones: this view is
// their only reader. They mirror plugins/wiki/store.go.
export type ClaimState = "cited" | "inferred" | "uncited" | "unsupported" | "superseded";

export interface WikiCounts { cited: number; inferred: number; uncited: number; unsupported: number; superseded: number }

export interface WikiCite {
  session: string;
  seq: number;
  label: string;
  excerpt: string;
  at?: string;
  problem?: string;
}

export interface WikiBlock {
  kind: "lede" | "heading" | "claim" | "code" | "links";
  text: string;
  line: number;
  end: number;
  raw: string;
  bullet: boolean;
  state?: ClaimState;
  cites: WikiCite[];
  supersededBy?: WikiCite;
}

export interface WikiPageRef {
  path: string;
  topic: string;
  title: string;
  summary: string;
  updated: string;
  counts: WikiCounts;
}

export interface WikiPageData extends WikiPageRef {
  blocks: WikiBlock[];
  sessions: string[];
  linkedFrom: WikiPageRef[];
  body: string;
}

export interface WikiIndexData {
  dir: string;
  exists: boolean;
  topics: { name: string; pages: WikiPageRef[] }[];
  health: {
    installed: boolean;
    every: string;
    pending: number;
    ingesting: boolean;
    lastIngest: string | null;
    unsupported: number;
    superseded: number;
    uncited: number;
  };
  thin: number;
  orphans: number;
}

export interface WikiFlag {
  kind: "unsupported" | "superseded" | "uncited" | "problem";
  page: string;
  title: string;
  line: number;
  end: number;
  raw: string;
  claim: string;
  why: string;
  evidence: string;
  cite?: WikiCite;
}

export interface WikiPending { id: string; title: string; entries: number; last: string }

export interface WikiReviewData { flags: WikiFlag[]; pending: WikiPending[] }

export interface WikiSourceData {
  session: { id: string; title: string; repo: string; branch: string; cwd: string };
  seq: number;
  at: string;
  total: number;
  lines: { seq: number; label: string; text: string }[];
  citedBy: WikiPageRef[];
}

export interface WikiOutcome { id: string; title: string; disposition: string; pages: string[] }

export interface WikiRun {
  session: string;
  command: string;
  at: string;
  done: string | null;
  ms: number;
  cost: number;
  running: boolean;
  outcomes: WikiOutcome[];
  files: string[];
  commit: string;
}

export interface WikiActivityData {
  runs: WikiRun[];
  every: string;
  pending: number;
  today: { runs: number; ingested: number; noMaterial: number; spent: number };
  spent: number;
}

export interface WikiCommit { hash: string; at: string; subject: string }
export interface WikiProblem { page: string; line: number; msg: string }
export interface WikiHit extends WikiPageRef { excerpt: string }

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

const q = encodeURIComponent;

export const wikiApi = {
  index: () => req<WikiIndexData>("/api/wiki"),
  page: (path: string) => req<WikiPageData>(`/api/wiki/page?path=${q(path)}`),
  save: (path: string, body: string) =>
    req<{ ok: true }>("/api/wiki/page", { method: "PUT", body: JSON.stringify({ path, body }) }).then(() => {}),
  history: (path: string) => req<{ commits: WikiCommit[] }>(`/api/wiki/history?path=${q(path)}`).then((r) => r.commits),
  source: (session: string, seq: number) => req<WikiSourceData>(`/api/wiki/source?session=${q(session)}&seq=${seq}`),
  review: () => req<WikiReviewData>("/api/wiki/review"),
  claim: (f: WikiFlag, action: "inference" | "drop") =>
    req<{ ok: true }>("/api/wiki/claim", {
      method: "POST",
      body: JSON.stringify({ path: f.page, line: f.line, end: f.end, raw: f.raw, action }),
    }).then(() => {}),
  activity: () => req<WikiActivityData>("/api/wiki/activity"),
  check: () => req<{ problems: WikiProblem[] }>("/api/wiki/check", { method: "POST" }).then((r) => r.problems),
  search: (text: string) => req<{ hits: WikiHit[] }>(`/api/wiki/search?q=${q(text)}`).then((r) => r.hits),
  ingest: (session = "") =>
    req<{ ok: true }>("/api/wiki/ingest", { method: "POST", body: JSON.stringify({ session }) }).then(() => {}),
};

/** Where you are in the wiki. It lives in the URL, like a session does. */
export type WikiRoute =
  | { at: "index" }
  | { at: "review" }
  | { at: "activity" }
  | { at: "page"; path: string; cite?: { session: string; seq: number } };

/** "wiki/p/topics/a/b.md~<session>~<seq>" → a route; null when it is not a wiki hash. */
export function parseWikiHash(h: string): WikiRoute | null {
  if (h === "wiki") return { at: "index" };
  if (h === "wiki/review") return { at: "review" };
  if (h === "wiki/activity") return { at: "activity" };
  const m = /^wiki\/p\/([^~]+\.md)(?:~([^~]+)~(\d+))?$/.exec(h);
  if (!m) return null;
  const path = decodeURIComponent(m[1]);
  return m[2] ? { at: "page", path, cite: { session: decodeURIComponent(m[2]), seq: Number(m[3]) } } : { at: "page", path };
}

export function wikiHash(r: WikiRoute): string {
  switch (r.at) {
    case "index": return "wiki";
    case "review": return "wiki/review";
    case "activity": return "wiki/activity";
    case "page": return `wiki/p/${r.path}${r.cite ? `~${q(r.cite.session)}~${r.cite.seq}` : ""}`;
  }
}

const msg = (e: unknown) => (e instanceof Error ? e.message : String(e));
const plural = (n: number, one: string, many = one + "s") => `${n} ${n === 1 ? one : many}`;

/** A session id as a citation marker: a UUID's first block, anything else whole. */
export function shortId(id: string): string {
  return /^[0-9a-f]{8}-[0-9a-f]{4}-/.test(id) ? id.slice(0, 8) : id;
}
const citeName = (c: { session: string; seq: number }) => `${shortId(c.session)}#${c.seq}`;

function since(iso: string | null): string {
  if (!iso) return "never";
  const s = Math.max(0, (Date.now() - Date.parse(iso)) / 1000);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  return `${Math.floor(s / 86400)} d ago`;
}
const hhmm = (iso: string) => new Date(iso).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
const dayOf = (iso: string) => new Date(iso).toLocaleDateString([], { month: "short", day: "numeric" });
const money = (n: number) => (n > 0 && n < 0.01 ? "<$0.01" : `$${n.toFixed(2)}`);
function took(ms: number): string {
  const s = Math.round(ms / 1000);
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, "0")}s`;
}

/** A page's claims as one line of facts; zeros are not facts. */
export function countsLine(c: WikiCounts): string {
  const parts = [
    c.cited && `${c.cited} cited`,
    c.inferred && `${c.inferred} inferred`,
    c.uncited && `${c.uncited} uncited`,
    c.unsupported && `${c.unsupported} broken`,
    c.superseded && `${c.superseded} superseded`,
  ].filter(Boolean);
  return parts.length ? parts.join(" · ") : "no claims";
}

/**
 * The two failure states must never look alike: unsupported is red (the
 * citation is broken), superseded is amber (a later session replaced it,
 * and the old claim stays).
 */
function PageState({ c }: { c: WikiCounts }) {
  if (c.unsupported) return <span className="hk2-state hk2-bad">Unsupported</span>;
  if (c.superseded) return <span className="hk2-state hk2-warn">Superseded</span>;
  return null;
}

/** Resolves a relative markdown link from one page to another. */
function resolveLink(from: string, href: string): string {
  const parts = from.split("/").slice(0, -1);
  for (const seg of href.split("#")[0].split("/")) {
    if (seg === "..") parts.pop();
    else if (seg && seg !== ".") parts.push(seg);
  }
  return parts.join("/");
}

function Crumbs({ onIndex, trail, title }: { onIndex?: () => void; trail?: string[]; title: string }) {
  return (
    <div className="head-main wk-crumb">
      {onIndex && <><button className="wk-crumb-link" onClick={onIndex}>Wiki</button><span className="wk-sep">/</span></>}
      {(trail ?? []).map((t) => <span key={t} className="wk-crumb-link wk-crumb-static">{t}<span className="wk-sep"> /</span></span>)}
      <h1>{title}</h1>
    </div>
  );
}

// ——— Index ———————————————————————————————————————————————————————

export function WikiIndexView({ data, onOpen, onReview, onActivity, check, onBack }: {
  data: WikiIndexData;
  onOpen: (path: string) => void;
  onReview: () => void;
  onActivity: () => void;
  check?: () => Promise<WikiProblem[]>;
  onBack?: () => void;
}) {
  const [problems, setProblems] = useState<WikiProblem[] | null>(null);
  const [checking, setChecking] = useState(false);
  const [err, setErr] = useState("");
  const h = data.health;
  const flagged = h.unsupported + h.superseded + h.uncited;
  const pages = data.topics.reduce((n, t) => n + t.pages.length, 0);

  const runCheck = () => {
    if (!check) return;
    setChecking(true); setErr("");
    check().then(setProblems).catch((e) => setErr(msg(e))).finally(() => setChecking(false));
  };

  return (
    <div className="thread">
      <header className="thread-head">
        <Back onBack={onBack} />
        <div className="head-main">
          <h1>Wiki</h1>
          <span className="mono head-repo">{data.dir}</span>
        </div>
        {data.exists && (
          <div className="hk-acts">
            {check && <button className="btn" onClick={runCheck} disabled={checking}>{checking ? "Checking…" : "Check citations"}</button>}
            <button className="btn" onClick={onActivity}>Activity</button>
          </div>
        )}
      </header>

      <div className="scroll proj-body">
        {!data.exists ? (
          <div className="proj-empty">
            <p className="proj-empty-title">No wiki yet</p>
            <p>The wiki is markdown compiled from your session history, one cited claim at a time.
               Run <code className="mono">bough wiki install</code> and a scheduler ingests finished
               sessions every five minutes; pages appear here as it writes them.</p>
          </div>
        ) : (
          <>
            {/* Health: three components, each one word and its facts. */}
            <div className="wk-health">
              <span className="wk-hpart">
                <span className={"hk2-state " + (h.installed ? "hk2-ok" : "hk2-warn")}>
                  {h.installed ? "Scheduler" : "Not scheduled"}
                </span>
                <span className="wk-facts">
                  {h.installed ? `every ${h.every || "tick"}` : <>ingests run only by hand · <code className="mono">bough wiki install</code></>}
                </span>
              </span>
              <span className="wk-hpart">
                <span className="hk2-state hk2-ok">{h.ingesting ? "Ingesting" : "Ingest"}</span>
                <span className="wk-facts">
                  {plural(h.pending, "session")} waiting · last ingest {since(h.lastIngest)}
                </span>
              </span>
              <span className="wk-hpart">
                <span className={"hk2-state " + (h.unsupported ? "hk2-bad" : flagged ? "hk2-warn" : "hk2-ok")}>Citations</span>
                <span className="wk-facts">
                  {flagged === 0 ? "every claim cited"
                    : [h.unsupported && `${h.unsupported} unsupported`, h.superseded && `${h.superseded} superseded`,
                       h.uncited && `${h.uncited} uncited`].filter(Boolean).join(" · ")}
                </span>
                {flagged > 0 && <button className="wk-link" onClick={onReview}>Review {plural(flagged, "claim")}</button>}
              </span>
            </div>

            {(problems || err) && (
              <div role="status" className="wk-check">
                {err ? <p className="hk2-alert">{err}</p>
                  : problems!.length === 0 ? <p className="wk-facts">wiki check: every citation and link resolves.</p>
                  : (
                    <pre className="wk-ev wk-ev-bad">
                      {problems!.map((p) => `${p.page}:${p.line}: ${p.msg}`).join("\n")}
                    </pre>
                  )}
              </div>
            )}

            {pages === 0 ? (
              <div className="proj-empty">
                <p className="proj-empty-title">Nothing compiled yet</p>
                <p>An ingest reads each finished session and writes a page only when there is something a
                   later session would want. Most sessions are not that. Activity shows every run.</p>
              </div>
            ) : data.topics.map((t) => (
              <section key={t.name || "untitled"} className="proj">
                <div className="proj-head">
                  <h2>{t.name || "Not in the index"}</h2>
                  <span className="num proj-count">{plural(t.pages.length, "page")}</span>
                </div>
                {t.pages.map((p) => (
                  <button key={p.path} className="wk-row" onClick={() => onOpen(p.path)}>
                    <span className="wk-row-main">
                      <span className="wk-title-line">
                        <span className="wk-title">{p.title}</span>
                        <PageState c={p.counts} />
                      </span>
                      {p.summary && <span className="wk-sum">{p.summary}</span>}
                    </span>
                    <span className="wk-counts">{countsLine(p.counts)}</span>
                    <span className="wk-date">{p.updated}</span>
                  </button>
                ))}
              </section>
            ))}

            {/* What a graph view would have been drawn for, as a sentence. */}
            {pages > 0 && (
              <p className="wk-foot">
                {plural(data.thin, "page")} {data.thin === 1 ? "rests" : "rest"} on a single citation ·{" "}
                {plural(data.orphans, "page")} nothing links to · {plural(h.pending, "session")} not yet ingested.
              </p>
            )}
          </>
        )}
      </div>
    </div>
  );
}

// ——— Page ————————————————————————————————————————————————————————

function Cites({ block, cite, onCite }: {
  block: WikiBlock; cite?: { session: string; seq: number } | null; onCite: (c: WikiCite) => void;
}) {
  return (
    <>
      {block.cites.map((c) => {
        const on = cite && cite.session === c.session && cite.seq === c.seq;
        return (
          <button key={`${c.session}#${c.seq}`}
                  className={"wk-cite" + (on ? " wk-cite-on" : "") + (c.problem ? " wk-cite-bad" : "")}
                  title={c.problem || `${c.label}: ${c.excerpt}`}
                  aria-pressed={on ? true : false}
                  onClick={() => onCite(c)}>
            {citeName(c)}
          </button>
        );
      })}
      {block.state === "uncited" && <span className="wk-need">citation needed</span>}
    </>
  );
}

/** The margin note: the first citation's entry, or why it is broken. */
function Note({ block }: { block: WikiBlock }) {
  const c = block.cites.find((x) => x.problem) ?? block.cites[0];
  if (!c) return <div />;
  const more = block.cites.length - 1;
  if (c.problem) {
    return (
      <div className="wk-note wk-note-bad">
        <span className="wk-note-src">{citeName(c)}</span>{c.problem}
      </div>
    );
  }
  return (
    <div className="wk-note">
      <span className="wk-note-src">{citeName(c)}{more > 0 ? ` · +${more} more` : ""} · {c.label}</span>
      {c.excerpt}
    </div>
  );
}

function Claim({ block, cite, onCite }: {
  block: WikiBlock; cite?: { session: string; seq: number } | null; onCite: (c: WikiCite) => void;
}) {
  const cls = "wk-text" + (block.bullet ? " wk-bullet" : "") + (block.state === "inferred" ? " wk-inferred" : "")
    + (block.state === "superseded" ? " wk-struck" : "");
  return (
    <div className="wk-claim" data-state={block.state}>
      <div>
        <div className={cls}>
          {block.state === "inferred" && <span className="wk-infer-label">inferred</span>}
          <Markdown text={block.text} />
          <Cites block={block} cite={cite} onCite={onCite} />
        </div>
        {block.state === "superseded" && (
          <div className="wk-super">
            <span className="wk-super-word">Superseded</span>
            <span className="wk-facts">
              a later session replaced it
              {block.supersededBy && <>{" — "}<button className="wk-cite" title={block.supersededBy.excerpt}
                onClick={() => onCite(block.supersededBy!)}>{citeName(block.supersededBy)}</button></>}
            </span>
          </div>
        )}
      </div>
      <Note block={block} />
    </div>
  );
}

export function WikiSourcePane({ source, error, onClose, onOpenSession, onOpenPage }: {
  source: WikiSourceData | null;
  error?: string;
  onClose: () => void;
  onOpenSession?: (id: string) => void;
  onOpenPage: (path: string) => void;
}) {
  return (
    <section className="wk-src" aria-label="Cited entry">
      <div className="wk-src-head">
        <div style={{ minWidth: 0 }}>
          <div className="wk-src-title">{source ? source.session.title || source.session.id : error ? "Entry not found" : "Loading…"}</div>
          {source && (
            <div className="wk-src-meta">
              {[source.session.repo?.split("/").pop() || shortId(source.session.id), source.at && `${dayOf(source.at)} ${hhmm(source.at)}`,
                `entry ${source.seq} of ${source.total}`].filter(Boolean).join(" · ")}
            </div>
          )}
        </div>
        <div className="hk-acts" style={{ marginInlineStart: "auto", flexShrink: 0 }}>
          {source && onOpenSession && (
            <button className="wk-link" onClick={() => onOpenSession(source.session.id)}>Open conversation</button>
          )}
          <button className="wk-x" onClick={onClose} aria-label="Close the entry">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8"
                 strokeLinecap="round" aria-hidden="true"><path d="M6 6l12 12M18 6L6 18" /></svg>
          </button>
        </div>
      </div>
      <div className="scroll" style={{ flexGrow: 1 }}>
        {error && <p className="hk2-alert" style={{ margin: 16 }}>{error}</p>}
        {source?.lines.map((l) => {
          const on = l.seq === source.seq;
          const code = l.label === "ran" || l.label === "output" || l.label === "error" || l.label === "cwd";
          return (
            <div key={l.seq} className={"wk-ent" + (on ? " wk-ent-on" : "")} aria-current={on ? "true" : undefined}>
              <span className="wk-ent-seq">{l.seq}</span>
              <span className="wk-ent-kind">{l.label}</span>
              <span className={"wk-ent-text" + (code ? " mono" : "")}>{l.text}</span>
            </div>
          );
        })}
      </div>
      {source && source.citedBy.length > 0 && (
        <div className="wk-src-foot">
          Cited by {plural(source.citedBy.length, "page")} ·{" "}
          {source.citedBy.map((p, i) => (
            <span key={p.path}>{i > 0 && ", "}<button className="wk-link" onClick={() => onOpenPage(p.path)}>{p.title}</button></span>
          ))}
        </div>
      )}
    </section>
  );
}

export function WikiPageView({ page, cite, source, sourceError, onCite, onCloseSource, onOpenPage, onIndex, onOpenSession, onSave, loadHistory, onBack }: {
  page: WikiPageData;
  cite?: { session: string; seq: number } | null;
  source?: WikiSourceData | null;
  sourceError?: string;
  onCite: (c: WikiCite) => void;
  onCloseSource: () => void;
  onOpenPage: (path: string) => void;
  onIndex: () => void;
  onOpenSession?: (id: string) => void;
  onSave?: (body: string) => Promise<void>;
  loadHistory?: () => Promise<WikiCommit[]>;
  onBack?: () => void;
}) {
  const [editing, setEditing] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [err, setErr] = useState("");
  const [history, setHistory] = useState<WikiCommit[] | null>(null);
  const [showHistory, setShowHistory] = useState(false);
  const open = Boolean(cite);
  useEffect(() => { setEditing(null); setHistory(null); setShowHistory(false); }, [page.path]);

  // Page-to-page links are relative markdown links; followed by the
  // browser they would leave the app.
  const follow = (e: React.MouseEvent) => {
    const a = (e.target as HTMLElement).closest("a");
    const href = a?.getAttribute("href") ?? "";
    if (!a || /^[a-z]+:/i.test(href) || !href.split("#")[0].endsWith(".md")) return;
    e.preventDefault();
    onOpenPage(resolveLink(page.path, href));
  };

  const toggleHistory = () => {
    const next = !showHistory;
    setShowHistory(next);
    if (next && history === null && loadHistory) loadHistory().then(setHistory).catch((e) => setErr(msg(e)));
  };

  const c = page.counts;
  return (
    <div className="thread">
      <header className="thread-head">
        <Back onBack={onBack} />
        <Crumbs onIndex={onIndex} trail={page.topic ? [page.topic] : []} title={page.title} />
        <div className="hk-acts">
          {onSave && editing === null && <button className="btn" onClick={() => setEditing(page.body)}>Edit</button>}
          {loadHistory && <button className="btn" aria-expanded={showHistory} onClick={toggleHistory}>History</button>}
        </div>
      </header>

      <div className="wk-split" data-source={open ? "1" : "0"}>
        <div className="scroll wk-doc" onClick={follow}>
          {err && <p className="hk2-alert">{err}</p>}
          {showHistory && (
            <div className="wk-history">
              {history === null ? <p className="wk-facts">Loading…</p>
                : history.length === 0 ? <p className="wk-facts">No recorded changes: the wiki is not a git repo.</p>
                : history.map((h) => (
                  <p key={h.hash} className="wk-facts"><span className="mono">{h.hash}</span> · {dayOf(h.at)} {hhmm(h.at)} · {h.subject}</p>
                ))}
            </div>
          )}
          {editing !== null && onSave ? (
            <div className="hk-panel">
              <label className="visually-hidden" htmlFor="wk-body">Page text</label>
              <textarea id="wk-body" className="hk-edit mono" rows={24} spellCheck={false}
                        value={editing} onChange={(e) => setEditing(e.target.value)} />
              <div className="hk-acts">
                <button className="btn btn-primary" disabled={saving} onClick={() => {
                  setSaving(true); setErr("");
                  onSave(editing).then(() => setEditing(null)).catch((e) => setErr(msg(e))).finally(() => setSaving(false));
                }}>Save</button>
                <button className="btn" onClick={() => setEditing(null)}>Cancel</button>
                <span className="hk-note">Saving commits the change to the wiki’s history.</span>
              </div>
            </div>
          ) : (
            <>
              {page.blocks.map((b) => {
                switch (b.kind) {
                  case "heading": return <h2 key={b.line} className="wk-h">{b.text}</h2>;
                  case "lede":
                    return (
                      <div key={b.line} className="wk-claim">
                        <div className="wk-text wk-lede"><Markdown text={b.text} /><Cites block={b} cite={cite} onCite={onCite} /></div>
                        <div />
                      </div>
                    );
                  case "claim": return <Claim key={b.line} block={b} cite={cite} onCite={onCite} />;
                  case "links": return <div key={b.line} className="wk-links"><Markdown text={"- " + b.text} /></div>;
                  default: return <div key={b.line} className="wk-code"><Markdown text={b.text} /></div>;
                }
              })}
              <div className="wk-pagefoot">
                {page.updated && <>Updated {page.updated} · </>}
                compiled from {plural(page.sessions.length, "session")} · {countsLine(c)}
                {page.linkedFrom.length > 0 && (
                  <details className="hk2-more">
                    <summary>Linked from {plural(page.linkedFrom.length, "page")}</summary>
                    <div className="hk2-more-body">
                      {page.linkedFrom.map((p) => (
                        <p key={p.path} className="hk2-note"><button className="wk-link" onClick={() => onOpenPage(p.path)}>{p.title}</button></p>
                      ))}
                    </div>
                  </details>
                )}
              </div>
            </>
          )}
        </div>
        {open && (
          <WikiSourcePane source={source ?? null} error={sourceError} onClose={onCloseSource}
                          onOpenSession={onOpenSession} onOpenPage={onOpenPage} />
        )}
      </div>
    </div>
  );
}

// ——— Review ——————————————————————————————————————————————————————

const flagWord: Record<WikiFlag["kind"], { word: string; cls: string }> = {
  unsupported: { word: "Unsupported", cls: "hk2-state hk2-bad" },
  superseded: { word: "Superseded", cls: "hk2-state hk2-warn" },
  uncited: { word: "Uncited", cls: "wk-tag" },
  problem: { word: "Check", cls: "hk2-state hk2-bad" },
};

type Filter = "all" | WikiFlag["kind"];

export function WikiReviewView({ data, onOpenPage, onAct, onSearch, onIngest, onIndex, onBack }: {
  data: WikiReviewData;
  onOpenPage: (path: string, cite?: WikiCite) => void;
  onAct: (f: WikiFlag, action: "inference" | "drop") => Promise<void>;
  onSearch?: (text: string) => void;
  onIngest?: (session: string) => Promise<void>;
  onIndex: () => void;
  onBack?: () => void;
}) {
  const [filter, setFilter] = useState<Filter>("all");
  const [busy, setBusy] = useState<Record<string, string>>({});
  const counts = useMemo(() => {
    const m: Record<string, number> = {};
    for (const f of data.flags) m[f.kind] = (m[f.kind] ?? 0) + 1;
    return m;
  }, [data.flags]);
  const shown = filter === "all" ? data.flags : data.flags.filter((f) => f.kind === filter);
  const key = (f: WikiFlag) => `${f.page}:${f.line}:${f.kind}`;

  const act = (f: WikiFlag, action: "inference" | "drop") => {
    setBusy((b) => ({ ...b, [key(f)]: "…" }));
    onAct(f, action)
      .then(() => setBusy((b) => { const n = { ...b }; delete n[key(f)]; return n; }))
      .catch((e) => setBusy((b) => ({ ...b, [key(f)]: msg(e) })));
  };

  return (
    <div className="thread">
      <header className="thread-head">
        <Back onBack={onBack} />
        <Crumbs onIndex={onIndex} title="Review" />
        <span className="head-repo">
          {data.flags.length === 0 ? "nothing to look at" : `${plural(data.flags.length, "claim")} the last check could not stand behind`}
        </span>
      </header>
      <div className="scroll proj-body">
        {data.flags.length > 0 && (
          <div className="wk-filters" role="group" aria-label="Show">
            <button className="wk-filter" aria-pressed={filter === "all"} onClick={() => setFilter("all")}>All {data.flags.length}</button>
            {(["unsupported", "superseded", "uncited", "problem"] as const).filter((k) => counts[k]).map((k) => (
              <button key={k} className="wk-filter" aria-pressed={filter === k} onClick={() => setFilter(k)}>
                {flagWord[k].word} {counts[k]}
              </button>
            ))}
          </div>
        )}
        <section className="proj">
          {data.flags.length === 0 && (
            <p className="proj-none">Every claim cites an entry that exists, and nothing has been superseded.</p>
          )}
          {shown.map((f) => {
            const k = key(f);
            const state = busy[k];
            return (
              <div key={k} className="wk-item">
                <div className="wk-title-line">
                  <span className={flagWord[f.kind].cls}>{flagWord[f.kind].word}</span>
                  <span className="wk-facts">{f.why}</span>
                  <button className="wk-where" onClick={() => onOpenPage(f.page, f.kind === "superseded" ? f.cite : undefined)}>
                    {f.page}:{f.line}
                  </button>
                </div>
                {f.claim && <div className="wk-item-claim"><Markdown text={f.claim} /></div>}
                {f.evidence && <pre className={"wk-ev" + (f.kind === "unsupported" ? " wk-ev-bad" : "")}>{f.evidence}</pre>}
                <div className="wk-acts">
                  {f.kind === "superseded" && f.cite && (
                    <button className="btn" onClick={() => onOpenPage(f.page, f.cite)}>Open the newer entry</button>
                  )}
                  {(f.kind === "unsupported" || f.kind === "uncited") && onSearch && (
                    <button className="btn" onClick={() => onSearch(f.claim)}>Search history</button>
                  )}
                  {(f.kind === "unsupported" || f.kind === "uncited") && (
                    <button className="btn" disabled={Boolean(state)} onClick={() => act(f, "inference")}>Mark as inference</button>
                  )}
                  {f.kind !== "problem" && (
                    <button className="btn" disabled={Boolean(state)} onClick={() => act(f, "drop")}>
                      {f.kind === "superseded" ? "Drop the old claim" : "Drop the claim"}
                    </button>
                  )}
                  {f.kind === "problem" && <button className="btn" onClick={() => onOpenPage(f.page)}>Open the page</button>}
                  {state && state !== "…" && <span className="hk-state hk-bad">Did not save — {state}</span>}
                </div>
              </div>
            );
          })}
        </section>

        {/* The other half of review: what happened but was never compiled. */}
        <section className="proj">
          <div className="proj-head">
            <h2>Not compiled</h2>
            <span className="proj-count">finished sessions no ingest has read yet</span>
          </div>
          {data.pending.length === 0
            ? <p className="proj-none">Every finished session has been ingested.</p>
            : data.pending.map((p) => <PendingRow key={p.id} p={p} onIngest={onIngest} />)}
        </section>
      </div>
    </div>
  );
}

function PendingRow({ p, onIngest }: { p: WikiPending; onIngest?: (session: string) => Promise<void> }) {
  const [state, setState] = useState("");
  return (
    <div className="proj-row">
      <span className="wk-row-main wk-title" style={{ fontSize: 14, color: "var(--text-2)" }}>{p.title || shortId(p.id)}</span>
      <span className="wk-counts">{shortId(p.id)} · {plural(p.entries, "entry", "entries")} · {dayOf(p.last)}</span>
      {onIngest && (
        <button className="btn" disabled={state !== "" && state !== "failed"} onClick={() => {
          setState("starting");
          onIngest(p.id).then(() => setState("started")).catch(() => setState("failed"));
        }}>{state === "started" ? "Started" : state === "failed" ? "Retry" : "Ingest"}</button>
      )}
    </div>
  );
}

// ——— Activity ————————————————————————————————————————————————————

/** A run is worth a row only for what it did to the wiki. */
function runWord(r: WikiRun): { word: string; cls: string } {
  if (r.running) return { word: "Ingesting", cls: "hk2-state hk2-ok" };
  if (!r.done) return { word: "Stopped", cls: "hk2-state hk2-bad" };
  const ds = r.outcomes.map((o) => o.disposition.toLowerCase());
  if (ds.some((d) => d.includes("disputed"))) return { word: "Disputed", cls: "hk2-state hk2-warn" };
  if (ds.some((d) => d.startsWith("new"))) return { word: "New", cls: "hk2-state hk2-ok" };
  if (ds.some((d) => d.startsWith("update"))) return { word: "Update", cls: "hk2-state hk2-ok" };
  if (ds.length && ds.every((d) => d === "no material")) return { word: "No material", cls: "wk-tag" };
  return { word: r.command.startsWith("/llm-wiki ingest") ? "Unlogged" : "Run", cls: "wk-tag" };
}

export function WikiActivityView({ data, onIngest, onOpenPage, onOpenSession, onIndex, onBack, note }: {
  data: WikiActivityData;
  onIngest?: () => void;
  onOpenPage: (path: string) => void;
  onOpenSession?: (id: string) => void;
  onIndex: () => void;
  onBack?: () => void;
  note?: string;
}) {
  const t = data.today;
  return (
    <div className="thread">
      <header className="thread-head">
        <Back onBack={onBack} />
        <Crumbs onIndex={onIndex} title="Activity" />
        {onIngest && (
          <div className="hk-acts">
            {note && <span className="hk-note" role="status">{note}</span>}
            <button className="btn" onClick={onIngest}>Ingest now</button>
          </div>
        )}
      </header>
      <div className="scroll proj-body">
        {/* The day in one line. Ticks that found nothing leave no record,
            so they are not counted — only runs that reached a model are. */}
        <div className="hk2-sum">
          <span><span className="hk2-sum-n">{t.runs}</span> <span className="hk2-sum-lab">{t.runs === 1 ? "run" : "runs"} today</span></span>
          <span><span className="hk2-sum-n" style={{ color: "var(--accent)" }}>{t.ingested}</span> <span className="hk2-sum-lab">compiled</span></span>
          <span><span className="hk2-sum-n" style={{ color: "var(--text-3)" }}>{t.noMaterial}</span> <span className="hk2-sum-lab">no material</span></span>
          <span><span className="hk2-sum-n">{money(t.spent)}</span> <span className="hk2-sum-lab">spent today</span></span>
          <span><span className="hk2-sum-n" style={{ color: data.pending ? "var(--amber)" : undefined }}>{data.pending}</span> <span className="hk2-sum-lab">waiting</span></span>
          <span><span className="hk2-sum-lab">{data.every ? `scheduler every ${data.every}` : "not scheduled"} · {money(data.spent)} all time</span></span>
        </div>

        {data.runs.length === 0 ? (
          <div className="proj-empty">
            <p className="proj-empty-title">No ingest has run yet</p>
            <p>A run starts when a finished session has been quiet for half an hour. Ingest now starts one
               immediately if anything is waiting.</p>
          </div>
        ) : (
          <div className="wk-list">
            {data.runs.map((r, i) => {
              const w = runWord(r);
              const prev = data.runs[i - 1];
              const gap = prev ? Date.parse(prev.at) - Date.parse(r.done ?? r.at) : 0;
              const pages = r.outcomes.flatMap((o) => o.pages);
              const other = r.files.filter((f) => f !== "log.md" && f !== "index.md" && !pages.includes(f));
              return (
                <div key={r.session} style={{ display: "contents" }}>
                  {gap > 60 * 60_000 && (
                    <div className="wk-quiet">{hhmm(r.done ?? r.at)} – {hhmm(prev!.at)} · nothing ingested</div>
                  )}
                  <div className="wk-run">
                    <div className="wk-run-head">
                      <span className="wk-run-when" title={new Date(r.at).toLocaleString()}>
                        {dayOf(r.at) === dayOf(new Date().toISOString()) ? hhmm(r.at) : `${dayOf(r.at)} ${hhmm(r.at)}`}
                      </span>
                      <span className={w.cls}>{w.word}</span>
                      <span className="wk-run-what">
                        {r.outcomes.length > 0
                          ? r.outcomes.map((o) => o.title || shortId(o.id)).join(", ")
                          : r.command || "an ingest"}
                      </span>
                      <span className="wk-run-meta">
                        {r.running ? "running" : took(r.ms)} · {money(r.cost)}
                        {r.commit ? <> · {r.commit}</> : !r.running && <> · not committed</>}
                        {onOpenSession && <> · <button className="wk-link" onClick={() => onOpenSession(r.session)}>transcript</button></>}
                      </span>
                    </div>
                    {(pages.length > 0 || other.length > 0) && (
                      <ul className="wk-diff">
                        {r.outcomes.filter((o) => o.pages.length).map((o) => o.pages.map((p) => (
                          <li key={o.id + p}>
                            <span className={o.disposition.toLowerCase().startsWith("new") ? "wk-add" : "wk-mod"}>
                              {o.disposition.toLowerCase().startsWith("new") ? "+ page" : "~ page"}
                            </span>{"  "}
                            <button className="wk-link mono" onClick={() => onOpenPage(p)}>{p}</button>
                          </li>
                        )))}
                        {other.map((f) => <li key={f}><span className="wk-mod">~ file</span>{"  "}<span className="wk-facts mono">{f}</span></li>)}
                      </ul>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}

// ——— Live container ——————————————————————————————————————————————

function useLoad<T>(load: (() => Promise<T>) | null, key: string, poll = 0) {
  const [data, setData] = useState<T | null>(null);
  const [err, setErr] = useState("");
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const reload = useCallback(() => {
    if (!load) return Promise.resolve();
    return load().then((d) => { setData(d); setErr(""); }).catch((e) => setErr(msg(e)));
  }, [key]);
  useEffect(() => {
    setData(null); setErr("");
    reload();
    if (!poll) return;
    const t = setInterval(reload, poll);
    return () => clearInterval(t);
  }, [reload, poll]);
  return { data, err, reload };
}

function Loading({ what, err }: { what: string; err: string }) {
  return (
    <div className="thread empty">
      <div>
        <h1>{err ? `${what} did not load` : `Loading ${what.toLowerCase()}…`}</h1>
        {err && <p>{err}</p>}
      </div>
    </div>
  );
}

/** The wiki view: routes to one of the four screens and keeps each live. */
export function WikiPage({ route, onRoute, onBack, onOpenSession, onSearch }: {
  route: WikiRoute;
  onRoute: (r: WikiRoute) => void;
  onBack?: () => void;
  onOpenSession?: (id: string) => void;
  onSearch?: (text: string) => void;
}) {
  const toIndex = () => onRoute({ at: "index" });
  const toPage = (path: string, cite?: { session: string; seq: number }) => onRoute({ at: "page", path, cite });

  const index = useLoad(route.at === "index" ? wikiApi.index : null, "index:" + route.at, 15_000);
  const review = useLoad(route.at === "review" ? wikiApi.review : null, "review:" + route.at);
  const activity = useLoad(route.at === "activity" ? wikiApi.activity : null, "activity:" + route.at, 5_000);
  const path = route.at === "page" ? route.path : "";
  const page = useLoad(path ? () => wikiApi.page(path) : null, "page:" + path);
  const cite = route.at === "page" ? route.cite : undefined;
  const source = useLoad(cite ? () => wikiApi.source(cite.session, cite.seq) : null,
    cite ? `src:${cite.session}#${cite.seq}` : "src:");
  const [note, setNote] = useState("");

  switch (route.at) {
    case "index":
      return index.data
        ? <WikiIndexView data={index.data} onBack={onBack} onOpen={(p) => toPage(p)}
                         onReview={() => onRoute({ at: "review" })} onActivity={() => onRoute({ at: "activity" })}
                         check={wikiApi.check} />
        : <Loading what="The wiki" err={index.err} />;
    case "review":
      return review.data
        ? <WikiReviewView data={review.data} onBack={onBack} onIndex={toIndex} onSearch={onSearch}
                          onOpenPage={(p, c) => toPage(p, c)}
                          onAct={(f, a) => wikiApi.claim(f, a).then(() => { review.reload(); })}
                          onIngest={(id) => wikiApi.ingest(id)} />
        : <Loading what="Review" err={review.err} />;
    case "activity":
      return activity.data
        ? <WikiActivityView data={activity.data} onBack={onBack} onIndex={toIndex} onOpenPage={(p) => toPage(p)}
                            onOpenSession={onOpenSession} note={note}
                            onIngest={() => {
                              setNote("");
                              wikiApi.ingest().then(() => {
                                setNote(activity.data!.pending ? "Started — the run appears here in a moment." : "Nothing is waiting to be ingested.");
                                activity.reload();
                              }).catch((e) => setNote(msg(e)));
                            }} />
        : <Loading what="Activity" err={activity.err} />;
    case "page":
      return page.data
        ? <WikiPageView page={page.data} cite={cite ?? null} source={source.data} sourceError={source.err}
                        onBack={onBack} onIndex={toIndex} onOpenSession={onOpenSession}
                        onCite={(c) => toPage(route.path, { session: c.session, seq: c.seq })}
                        onCloseSource={() => toPage(route.path)}
                        onOpenPage={(p) => toPage(p)}
                        onSave={(body) => wikiApi.save(route.path, body).then(() => { page.reload(); })}
                        loadHistory={() => wikiApi.history(route.path)} />
        : <Loading what="The page" err={page.err} />;
  }
}
