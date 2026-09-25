import { type RefObject, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { Back, ago } from "./app";
import { duration, EmptyState, Pending, useCopied } from "./loading";
import { Markdown } from "./render";

// The wiki wire types live here, like the hooks ones: this view is
// their only reader. They mirror plugins/wiki/store.go.
export type ClaimState = "cited" | "inferred" | "uncited" | "unsupported" | "superseded";

export interface WikiCounts { cited: number; inferred: number; uncited: number; unsupported: number; superseded: number }

export interface WikiCite {
  session: string;
  seq: number;
  /** An external citation (`gh:owner/repo#7801`): source and ref instead of session and seq; url when it has an address. */
  source?: string;
  ref?: string;
  url?: string;
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
  /** Set by the server when the page does not parse as a wiki page; absent on older servers. */
  malformed?: boolean | string;
  /** What the server found wrong with the page (plugins/wiki store.go Problems). */
  problems?: string[];
}

/** Why a page reads as malformed, or "" when it doesn't: the server's problems, else the older flag. */
const malformedWhy = (p: WikiPageRef): string =>
  p.problems?.length ? p.problems.join("; ") : p.malformed ? (typeof p.malformed === "string" ? p.malformed : " ") : "";

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

let indexRead: Promise<WikiIndexData> | null = null;

/** One row under the brief: what wants the person, what is moving, what they wait on, what got done. */
export interface MeSignal {
  kind: "needs-you" | "moving" | "waiting" | "done";
  source: string;
  title: string;
  note?: string;
  project?: string;
  /** owner/name and the login, when the source has them: a dismissal can then teach a rule. */
  repo?: string;
  author?: string;
  at?: string;
  cite?: string;
  url?: string;
  session?: string;
}
export interface MeSource { name: string; ok: boolean; at?: string; error?: string }
export interface MeSignals { asOf?: string; items?: MeSignal[]; sources?: MeSource[] }
/** GET /api/me, mirroring plugins/wiki/brief.go. */
export interface MeData {
  date: string;
  hasProfile: boolean;
  path?: string;
  page?: WikiPageData;
  stale?: boolean;
  asOf?: string;
  signals?: MeSignals | null;
  days: string[];
  /** What the person said about the rows: keys closed, keys pinned. */
  triage: MeTriage;
  /** A brief this serve started is still running. */
  running?: boolean;
}
export interface MeTriage { dismissed: Record<string, string>; pinned: string[] }
export type TriageAction = "dismiss" | "undismiss" | "pin" | "unpin";

export const wikiApi = {
  me: () => req<MeData>("/api/me"),
  refreshMe: () => req<{ ok: true }>("/api/me/refresh", { method: "POST" }).then(() => {}),
  triage: (action: TriageAction, key: string, rule?: string) =>
    req<{ ok: true; triage: MeTriage }>("/api/me/triage", { method: "POST", body: JSON.stringify({ action, key, rule }) }).then((r) => r.triage),
  steer: (text: string) => req<{ ok: true; section: string }>("/api/me/steer", { method: "POST", body: JSON.stringify({ text }) }).then((r) => r.section),
  // One index read at a time for every caller (the nav count and the page):
  // a read still in flight is shared, not queued behind.
  index: () => indexRead ??= req<WikiIndexData>("/api/wiki").finally(() => { indexRead = null; }),
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
  const m = /^wiki\/p\/([^~]+?)(?:~([^~]+)~(\d+))?$/.exec(h);
  if (!m) return null;
  // A path typed without its extension still names the page.
  const path = decodeURIComponent(m[1]).replace(/(?<!\.md)$/, ".md");
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

/** A page that is not there, or a path that cannot be one: trying again will not help. */
export const pageMissing = (err?: string | null) => /not found|not a page path|^404\b/i.test(err ?? "");

const msg = (e: unknown) => (e instanceof Error ? e.message : String(e));
const capitalize = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);
const plural = (n: number, one: string, many = one + "s") => `${n} ${n === 1 ? one : many}`;

/** A session id as a citation marker: a UUID's first block, anything else whole. */
export function shortId(id: string): string {
  return /^[0-9a-f]{8}-[0-9a-f]{4}-/.test(id) ? id.slice(0, 8) : id;
}
/** A citation as a chip reads it: the entry, with the session in the tooltip. */
const citeName = (c: { session: string; seq: number; source?: string; ref?: string }) => c.source ? shortRef(c.source, c.ref ?? "") : `#${c.seq}`;
/**
 * An external reference as a chip: the shortest token that still names it.
 * "asi/repo#7801" → "repo#7801", "repo@a1b2c3d…" → "git:a1b2c3d", a Linear key
 * as is; the full reference stays in the tooltip.
 */
export function shortRef(source: string, ref: string): string {
  const name = source === "gh" ? (() => { const m = /^(?:[^/#]+\/)?([^/#]+)(#\d+)$/.exec(ref); return m ? m[1] + m[2] : `gh:${ref}`; })()
    : source === "git" ? (() => { const m = /@([0-9a-f]{7,40})$/i.exec(ref); return m ? `git:${m[1].slice(0, 7)}` : `git:${ref}`; })()
    : source === "linear" ? ref
    : `${source}:${ref}`;
  // A long repo name loses its head, not its tail: the number at the end
  // is what tells two chips from the same repo apart.
  return name.length > CHIP_MAX ? "…" + name.slice(name.length - CHIP_MAX + 1) : name;
}
/** The longest chip; the CSS cap is the same width, so this is the only truncation that fires. */
const CHIP_MAX = 28;
const citeWho = (c: { session: string; seq: number; source?: string; ref?: string }) => c.source ? `${c.source} ${c.ref}` : `session ${shortId(c.session)}, entry ${c.seq}`;
const UUID = /\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b/g;
/** Full session UUIDs in free text (commit subjects, commands) as short, labeled ids. */
const shortIds = (s: string) => s.replace(UUID, (id) => `session ${shortId(id)}`);

// Dates: one relative style (the sidebar's "1h ago") and one absolute
// style ("Sep 14, 16:41"; the time alone for today) on every wiki surface.
function since(iso: string | null): string {
  return iso ? `${ago(iso)} ago` : "never";
}
/** The health line: one dot whose colour is the worst state on the page. */
export function healthTone(h: { installed: boolean }, orphans: number, flagged: number): string {
  return flagged > 0 || orphans > 0 ? "is-attn" : h.installed ? "is-on" : "is-waiting";
}
const hhmm = (iso: string) => new Date(iso).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
const dayOf = (iso: string) => new Date(iso).toLocaleDateString([], { month: "short", day: "numeric" });
/** The absolute style. A bare date ("2026-09-11") has no time to show. */
export function stamp(iso: string): string {
  if (!iso || Number.isNaN(Date.parse(iso))) return iso;
  if (/^\d{4}-\d{2}-\d{2}$/.test(iso)) return new Date(iso + "T12:00:00").toLocaleDateString([], { month: "short", day: "numeric" });
  return dayOf(iso) === dayOf(new Date().toISOString()) ? hhmm(iso) : `${dayOf(iso)}, ${hhmm(iso)}`;
}

/** "retry-budget" → "Retry budget": a title that is only its slug, readable. */
export function humanTitle(title: string, path = ""): string {
  const stem = path.split("/").pop()?.replace(/\.md$/, "") ?? "";
  const t = title.trim();
  if (t && t !== stem && !/^[a-z0-9]+([-_][a-z0-9]+)*$/.test(t)) return t;
  const words = (t || stem).replace(/[-_]+/g, " ").trim();
  return words ? words[0].toUpperCase() + words.slice(1) : "Untitled page";
}

/** Excerpt text as stored: literal "\n" escapes out, orphan fence lines gone. */
export function cleanExcerpt(s: string): string {
  return s.replace(/\\n/g, "\n").replace(/\\t/g, "  ")
    .split("\n").filter((l) => !/^\s*(```|~~~)\S*\s*$/.test(l)).join("\n").trim();
}
const money = (n: number) => (n > 0 && n < 0.01 ? "<$0.01" : `$${n.toFixed(2)}`);
function took(ms: number): string {
  const s = Math.round(ms / 1000);
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, "0")}s`;
}

/**
 * Wiki markdown with underscores kept literal: snake_case and __init__ are
 * names here, never emphasis. Code spans and fences are left as written.
 */
export function literalUnderscores(md: string): string {
  return md.split(/(```[\s\S]*?```|`[^`\n]*`)/).map((part, i) => (i % 2 ? part : part.replace(/_/g, "\\_"))).join("");
}

/** A body written from a numbered view ("12|text"), its gutter removed; the empty "Sessions:,,," line dropped. */
export function denumber(body: string): string {
  return body.split("\n")
    .map((l) => l.replace(/^\s*\d+\|/, "").replace(/\s*·?\s*Sessions:\s*,{2,}\s*$/, ""))
    .join("\n");
}

/** A page's claims as one line of facts; zeros are not facts. "Cited" counts claims, not citation chips. */
/** Every claim on a page, whatever its citation state. */
export const claimTotal = (c: WikiCounts) => c.cited + c.inferred + c.uncited + c.unsupported + c.superseded;

export function countsLine(c: WikiCounts): string {
  const parts = [
    c.cited && `${plural(c.cited, "cited claim")}`,
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

function Crumbs({ onIndex, trail, title, slug, muted, quiet }: { onIndex?: () => void; trail?: string[]; title: string; slug?: string; /** A missing page: the slug, quiet. */ muted?: boolean; /** The body states the title as its H1: the crumb stays small. */ quiet?: boolean }) {
  return (
    <div className={"head-main wk-crumb" + (slug ? " wk-crumb-slugged" : "")}>
      {onIndex && <><button className="wk-crumb-link" onClick={onIndex}>Wiki</button><span className="wk-sep" aria-hidden="true">/</span></>}
      {(trail ?? []).map((t) => <span key={t} className="wk-crumb-link wk-crumb-static wk-crumb-trail">{t}<span className="wk-sep" aria-hidden="true">/</span></span>)}
      <h1 className={[muted && "wk-crumb-missing", quiet && "wk-crumb-quiet"].filter(Boolean).join(" ") || undefined}>{title}</h1>
      {/* Phones read "Wiki / <title>" on one line, cut when long. */}
      {slug && <span className={"wk-crumb-slug" + (muted ? " wk-crumb-missing" : "")} aria-hidden="true">{title}</span>}
    </div>
  );
}

/** "/private/tmp/x/.bough/wiki" → "…/.bough/wiki": the last two segments say where. */
const elide = (dir: string) => {
  const parts = dir.split("/").filter(Boolean);
  return parts.length > 2 ? "…/" + parts.slice(-2).join("/") : dir;
};

function CopyCommand({ text }: { text: string }) {
  const [done, copy] = useCopied();
  return (
    <p className="wk-cmd">
      <code className="mono">{text}</code>
      <button className="btn" onClick={() => copy(text)}>{done ? "Copied" : "Copy command"}</button>
    </p>
  );
}

// ——— Index ———————————————————————————————————————————————————————

/** Copies the full wiki path; an icon that shows on hover or focus of the path. */
function CopyPath({ text, label = "Copy full path" }: { text: string; label?: string }) {
  const [done, copy] = useCopied();
  return (
    <button className="wk-x wk-copy" onClick={() => copy(text)} aria-label={done ? "Copied" : label} title={done ? "Copied" : label}>
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        {done ? <path d="M5 12l5 5 9-10" /> : <><rect x="9" y="9" width="11" height="11" rx="2" /><path d="M5 15V5a1 1 0 0 1 1-1h9" /></>}
      </svg>
    </button>
  );
}

export function WikiIndexView({ data, onOpen, onReview, onActivity, onIngest, ingestErr, check, onBack }: {
  data: WikiIndexData;
  onOpen: (path: string) => void;
  onReview: () => void;
  onActivity: () => void;
  /** Starts an ingest from the "Indexing" state. */
  onIngest?: () => void;
  /** Why the last Ingest now did not start; "" when it did. */
  ingestErr?: string;
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
      <header className="thread-head page-head">
        <Back onBack={onBack} />
        <div className="head-main">
          <h1>Wiki</h1>
          <span className="mono wk-dir" title={data.dir}>{elide(data.dir)}</span>
          <CopyPath text={data.dir} />
        </div>
        {data.exists && (
          <div className="hk-acts">
            {check && (
              <button className="btn" onClick={runCheck} disabled={checking} aria-busy={checking}>
                {checking && <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
                  strokeLinecap="round" className="spin-mark" aria-hidden="true"><circle cx="12" cy="12" r="8.5" strokeDasharray="40 14" /></svg>}
                {checking ? " Checking…" : "Check citations"}
              </button>
            )}
            <button className="btn btn-ghost" onClick={onActivity}>Activity</button>
          </div>
        )}
      </header>

      <div className="scroll proj-body">
        {!data.exists ? (
          h.installed || h.ingesting ? (<>
            {ingestErr && <p className="hk2-alert" role="alert">Ingest did not start: {ingestErr}</p>}
            <EmptyState title="Indexing" role="status" action={onIngest && { label: "Ingest now", onClick: onIngest }}>
              {plural(h.pending, "session")} {h.pending === 1 ? "is" : "are"} waiting
              {h.every ? `; the next tick is within ${duration(h.every)}` : ""}. Pages appear here as the first ingest writes them.
            </EmptyState>
          </>) : (
            <div className="empty-state">
              <h2>Not enabled</h2>
              <p>The wiki is markdown compiled from your session history, one cited claim at a time. Enabling it
                 installs a background scheduler that ingests finished sessions every five minutes. Run this in a terminal:</p>
              <CopyCommand text="bough wiki install" />
            </div>
          )
        ) : (
          <>
            {/* Health as one sentence; only flagged claims are red. */}
            {/* Each fact is its own span in a wrapping row: as one string with
                " · " between, a phone wrapped it mid-sentence with a dangling dot. */}
            <p className="wk-health">
              <span className="wk-fact">
                <span className={"wk-dot " + healthTone(h, data.orphans, flagged)} aria-hidden="true" />
                {h.installed ? `Scheduler every ${h.every ? duration(h.every) : "tick"}`
                  : <>Not scheduled <code className="mono">bough wiki install</code><CopyPath text="bough wiki install" label="Copy command" /></>}
              </span>
              <span className="wk-fact">{h.ingesting ? "Ingesting now" : h.lastIngest ? `Last ingest ${since(h.lastIngest)}` : "Never ingested"}</span>
              {h.pending > 0 && <span className="wk-fact">{plural(h.pending, "session")} waiting</span>}
              {data.thin > 0 && <span className="wk-fact">{check ? <button className="wk-link" onClick={runCheck}>{plural(data.thin, "page")} {data.thin === 1 ? "rests" : "rest"} on one citation</button>
                : <>{plural(data.thin, "page")} {data.thin === 1 ? "rests" : "rest"} on one citation</>}</span>}
              {data.orphans > 0 && <span className="wk-fact">{check ? <button className="wk-link" onClick={runCheck} title="Pages with no inbound links">{plural(data.orphans, "page")} nothing links to</button>
                : <span title="Pages with no inbound links">{plural(data.orphans, "page")} nothing links to</span>}</span>}
              {flagged > 0 && <span className="wk-fact"><button className="wk-link wk-link-bad" onClick={onReview}>Review {plural(flagged, "flagged claim")}</button></span>}
            </p>

            {(problems || err) && (
              <div role="status" className="wk-check">
                {err ? <p className="hk2-alert">Check failed: {err}</p>
                  : problems!.length === 0 ? <p className="wk-facts">Checked: every citation and link resolves.</p>
                  : (<>
                    <p className="wk-facts">Checked: {plural(problems!.length, "problem")} found.</p>
                    <pre className="wk-ev wk-ev-bad">
                      {problems!.map((p) => `${p.page}:${p.line}: ${p.msg}`).join("\n")}
                    </pre>
                  </>)}
              </div>
            )}

            {pages === 0 ? (
              <EmptyState title={h.ingesting ? "Indexing" : "No pages yet"} action={{ label: "Open activity", onClick: onActivity }}>
                An ingest writes a page only when a session holds something a later session would want; most do not.
              </EmptyState>
            ) : data.topics.map((t) => (
              <section key={t.name || "untitled"} className="proj">
                <div className="proj-head">
                  <h2>{t.name || "Not in the index"}</h2>
                  <span className="num proj-count">{plural(t.pages.length, "page")}</span>
                </div>
                <div className="wk-list">
                {t.pages.map((p) => (
                  <a key={p.path} className="wk-row" href={"#/" + wikiHash({ at: "page", path: p.path })}
                     onClick={(e) => { if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return; e.preventDefault(); onOpen(p.path); }}>
                    <span className="wk-row-main">
                      <span className="wk-title-line">
                        <span className="wk-title">{humanTitle(p.title, p.path)}</span>
                        <PageState c={p.counts} />
                        {malformedWhy(p) && <span className="hk2-state hk2-warn">Malformed</span>}
                      </span>
                      {p.summary && <span className="wk-sum">{plainText(p.summary).replace(/^Inference:\s*/i, "")}</span>}
                    </span>
                    <span className="wk-counts num" title={countsLine(p.counts)}>{countsLine(p.counts)}</span>
                    <span className="wk-date num">{stamp(p.updated)}</span>
                  </a>
                ))}
                </div>
              </section>
            ))}
          </>
        )}
      </div>
    </div>
  );
}

// ——— Page ————————————————————————————————————————————————————————

export function Cites({ block, cite, onCite, onHot }: {
  block: WikiBlock; cite?: { session: string; seq: number } | null; onCite: (c: WikiCite) => void;
  /** Hovering or focusing a chip highlights its margin note. */
  onHot?: (on: boolean) => void;
}) {
  return (
    <>
      {block.cites.map((c) => {
        // An external citation is a link out, or a name when it has no address; never a source pane.
        if (c.source) {
          const key = `${c.source}:${c.ref}`;
          return c.url
            ? <a key={key} className="wk-cite wk-cite-ext" href={c.url} target="_blank" rel="noreferrer" title={citeWho(c)}>{citeName(c)}</a>
            : <span key={key} className="wk-cite wk-cite-ext" title={citeWho(c)}>{citeName(c)}</span>;
        }
        const on = cite && cite.session === c.session && cite.seq === c.seq;
        return (
          <button key={`${c.session}#${c.seq}`}
                  className={"wk-cite" + (on ? " wk-cite-on" : "") + (c.problem ? " wk-cite-bad" : "")}
                  title={`${citeWho(c)} — ${c.problem || `${c.label}: ${plainText(c.excerpt)}`}`}
                  aria-label={`Citation: ${citeWho(c)}`}
                  aria-pressed={on ? true : false}
                  onMouseEnter={() => onHot?.(true)} onMouseLeave={() => onHot?.(false)}
                  onFocus={() => onHot?.(true)} onBlur={() => onHot?.(false)}
                  onClick={() => { onHot?.(true); onCite(c); }}>
            {citeName(c)}
          </button>
        );
      })}
      {block.state === "uncited" && <span className="wk-need">[citation needed]</span>}
    </>
  );
}

/** Markdown syntax out of an excerpt: the margin is too narrow to render it. */
const plainText = (s: string) => cleanExcerpt(s)
  .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
  .replace(/\*\*|__|`/g, "")
  .replace(/(^|\s)\*([^*\n]+)\*/g, "$1$2")
  .replace(/^#{1,6}\s+/gm, "")
  .replace(/^(\s*)[-*]\s+/gm, "$1• ");

/** The margin note: the first citation's entry, or why it is broken. */
function Note({ block, hot = false }: { block: WikiBlock; hot?: boolean }) {
  const c = block.cites.find((x) => x.problem) ?? block.cites.find((x) => !x.source);
  if (!c) return <div />;
  return <NoteBody c={c} more={block.cites.length - 1} claim={block.text} hot={hot} />;
}

/** A claim's first few words, to say which claim a margin note backs. */
const firstWords = (s: string) => {
  const words = plainText(s).replace(/\s+/g, " ").trim().split(" ");
  return words.slice(0, 5).join(" ") + (words.length > 5 ? "…" : "");
};

/** Clamped to a few lines, so a long excerpt never pushes the paragraph below it down. */
function NoteBody({ c, more, claim, hot }: { c: WikiCite; more: number; claim: string; hot: boolean }) {
  const [open, setOpen] = useState(false);
  // An excerpt stored as a one-element list ("[…]") shows without its brackets.
  const text = c.problem || plainText(c.excerpt).replace(/^\[\s*"?([\s\S]*?)"?\s*\]$/, "$1");
  const long = text.split("\n").length > 2 || text.length > 100;
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (hot) box.current?.scrollIntoView?.({ block: "nearest", behavior: "smooth" });
  }, [hot]);
  const about = firstWords(claim);
  const btn = useRef<HTMLButtonElement>(null);
  const top = useRef<number | null>(null);
  // Expanding grows the note downward: the button stays where the reader clicked it.
  useLayoutEffect(() => {
    const el = btn.current, was = top.current;
    top.current = null;
    if (!el || was === null) return;
    const scroller = el.closest(".scroll");
    if (scroller) scroller.scrollTop += el.getBoundingClientRect().top - was;
  }, [open]);
  return (
    <div ref={box} className={"wk-note" + (c.problem ? " wk-note-bad" : "") + (hot ? " wk-note-on" : "")}>
      <span className="wk-note-src">
        {about && <span className="wk-note-about">supports: {about}</span>}
        {c.problem ? `entry ${c.seq}` : `${c.label} · entry ${c.seq}`}{more > 0 ? ` · +${more} more` : ""}
      </span>
      <span className={"wk-note-text" + (long && !open ? " wk-note-clamp" : "")}
            onClick={long ? () => { top.current = btn.current?.getBoundingClientRect().top ?? null; setOpen(!open); } : undefined}>{text}</span>
      {long && <button ref={btn} className="wk-link wk-note-more" aria-expanded={open}
                       aria-label={`${open ? "Show less" : "Show more"} of the evidence for “${about}”`}
                       onClick={() => { top.current = btn.current?.getBoundingClientRect().top ?? null; setOpen(!open); }}>{open ? "Less" : "More"}</button>}
    </div>
  );
}

/** One callout for inferred and superseded claims: the same indent, an eyebrow label. */
function Callout({ label, tone, children }: { label: string; tone?: "amber"; children: React.ReactNode }) {
  return (
    <div className={"wk-callout" + (tone ? ` wk-callout-${tone}` : "")}>
      <span className="eyebrow">{label}</span>
      {children}
    </div>
  );
}

function Claim({ block, cite, onCite }: {
  block: WikiBlock; cite?: { session: string; seq: number } | null; onCite: (c: WikiCite) => void;
}) {
  const [hot, setHot] = useState(false);
  const cls = "wk-text" + (block.bullet ? " wk-bullet" : "") + (block.state === "superseded" ? " wk-struck" : "");
  const text = (
    <div className={cls}>
      <Markdown text={literalUnderscores(block.text)} />
      <Cites block={block} cite={cite} onCite={onCite} onHot={setHot} />
    </div>
  );
  return (
    <div className="wk-claim" data-state={block.state}>
      <div>
        {block.state === "inferred" ? <Callout label="Inferred">{text}</Callout>
          : block.state === "superseded" ? (
            <Callout label="Superseded" tone="amber">
              {text}
              <span className="wk-facts">
                a later session replaced it
                {block.supersededBy && <>{" — "}<button className="wk-cite" title={block.supersededBy.excerpt}
                  onClick={() => onCite(block.supersededBy!)}>{citeName(block.supersededBy)}</button></>}
              </span>
            </Callout>
          ) : text}
      </div>
      <Note block={block} hot={hot} />
    </div>
  );
}

export function WikiSourcePane({ source, error, onClose, onOpenSession, onOpenPage, onRetry }: {
  source: WikiSourceData | null;
  error?: string;
  onClose: () => void;
  onRetry?: () => void;
  onOpenSession?: (id: string) => void;
  onOpenPage: (path: string) => void;
}) {
  // The cited entry sits among its neighbours; bring it into view, since
  // an entry before it can be a screen of tool output on its own.
  useEffect(() => {
    document.querySelector(".wk-src .wk-ent-on")?.scrollIntoView({ block: "center" });
  }, [source?.session.id, source?.seq]);
  // Escape closes the pane, unless a field or an open dialog owns the key.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented) return;
      const t = e.target as HTMLElement | null;
      if (t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName) || t.closest("[role=dialog]"))) return;
      if (document.querySelector("[role=dialog][open], .pal")) return;
      onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  return (
    <section className="wk-src" aria-label="Cited entry">
      <div className="wk-src-head">
        <div style={{ minWidth: 0 }}>
          <div className="wk-src-title">{source ? source.session.title || `Session ${shortId(source.session.id)}` : error ? "Entry not found" : "Cited entry"}</div>
          {source && (
            <div className="wk-src-meta">
              {[source.session.repo?.split("/").pop() || `session ${shortId(source.session.id)}`, source.at && stamp(source.at),
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
        {!source && <Pending what="The cited entry" err={error} lines={error ? 0 : 5} onRetry={onRetry} />}
        {source?.lines.map((l) => {
          const on = l.seq === source.seq;
          const code = l.label === "ran" || l.label === "output" || l.label === "error" || l.label === "cwd";
          return (
            <div key={l.seq} className={"wk-ent" + (on ? " wk-ent-on" : "")} aria-current={on ? "true" : undefined}>
              <span className="wk-ent-seq">{l.seq}</span>
              <span className="wk-ent-kind">{l.label}</span>
              <span className={"wk-ent-text" + (code ? " mono" : "")}>{code ? cleanExcerpt(l.text) : plainText(l.text)}</span>
            </div>
          );
        })}
      </div>
      {source && source.citedBy.length > 0 && (
        <div className="wk-src-foot">
          Cited by {plural(source.citedBy.length, "page")}
          {/* A list, not commas: titles contain commas. */}
          <ul className="wk-citedby">
            {source.citedBy.map((p) => (
              <li key={p.path}><button className="wk-link" onClick={() => onOpenPage(p.path)}>{humanTitle(p.title, p.path)}</button></li>
            ))}
          </ul>
        </div>
      )}
    </section>
  );
}

/** What WikiPageView is given: the route's reads and the ways out of the page. */
export type WikiPageProps = {
  /** Null while the page loads or when it failed: the header stays, only the body waits. */
  page: WikiPageData | null;
  /** The page being opened, for the header before its data arrives. */
  path?: string;
  /** A title already known from the link that opened it. */
  knownTitle?: string;
  pageError?: string;
  onRetry?: () => void;
  onRetrySource?: () => void;
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
  /** Opens the palette on the missing slug. */
  onSearch?: (text: string) => void;
};

/** Everything WikiPageView holds, so each of its states can be rendered on its own. */
export interface WikiPageState {
  /** The editor's text; null when the editor is closed. */
  editing: string | null;
  saving: boolean;
  /** The last Save's error. */
  err: string;
  history: WikiCommit[] | null;
  showHistory: boolean;
  histErr: string;
  /** The narrow "Page actions" menu. */
  menu: boolean;
}

/** What the page's own controls do; WikiPageView owns the state they change. */
export interface WikiPageActs {
  edit: () => void; type: (text: string) => void; save: () => void; cancel: () => void;
  toggleHistory: () => void; closeHistory: () => void; retryHistory: () => void;
  toggleMenu: () => void; menuEdit: () => void; menuHistory: () => void;
}

export function WikiPageView(props: WikiPageProps) {
  const { page, path, cite, onOpenPage, onSave, loadHistory } = props;
  const [editing, setEditing] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [err, setErr] = useState("");
  const [history, setHistory] = useState<WikiCommit[] | null>(null);
  const [showHistory, setShowHistory] = useState(false);
  const [histErr, setHistErr] = useState("");
  const [menu, setMenu] = useState(false);
  const at = page?.path ?? path ?? "";
  // The cite too: Back from the editor to this page's cited entry is a
  // move like any other, and left the editor open over the entry.
  const citeKey = cite ? `${cite.session}#${cite.seq}` : "";
  useEffect(() => { setEditing(null); setHistory(null); setShowHistory(false); setHistErr(""); }, [at, citeKey]);

  // Page-to-page links are relative markdown links; followed by the
  // browser they would leave the app. Listened for on the element, so the
  // article itself is not a clickable generic.
  const doc = useRef<HTMLDivElement>(null);
  const openPage = useRef(onOpenPage);
  openPage.current = onOpenPage;
  useEffect(() => {
    const el = doc.current;
    if (!el) return;
    const follow = (e: MouseEvent) => {
      const a = (e.target as HTMLElement).closest("a");
      const href = a?.getAttribute("href") ?? "";
      if (!a || /^[a-z]+:/i.test(href) || !href.split("#")[0].endsWith(".md")) return;
      e.preventDefault();
      openPage.current(resolveLink(at, href));
    };
    el.addEventListener("click", follow);
    return () => el.removeEventListener("click", follow);
  }, [at]);

  const fetchHistory = () => {
    if (!loadHistory) return;
    setHistErr("");
    loadHistory().then(setHistory).catch((e) => setHistErr(msg(e)));
  };
  const toggleHistory = () => {
    const next = !showHistory;
    setShowHistory(next);
    if (next && history === null) fetchHistory();
  };
  const edit = () => { setShowHistory(false); setEditing(page?.body ?? ""); };

  return (
    <WikiPageUi {...props} docRef={doc}
      state={{ editing, saving, err, history, showHistory, histErr, menu }}
      acts={{
        edit, toggleHistory, retryHistory: fetchHistory,
        type: setEditing,
        save: () => {
          if (!onSave || editing === null) return;
          setSaving(true); setErr("");
          onSave(editing).then(() => setEditing(null)).catch((e) => setErr(msg(e))).finally(() => setSaving(false));
        },
        cancel: () => setEditing(null),
        closeHistory: () => setShowHistory(false),
        toggleMenu: () => setMenu(!menu),
        menuEdit: () => { setMenu(false); edit(); },
        menuHistory: () => { setMenu(false); toggleHistory(); },
      }} />
  );
}

/** WikiPageView's markup as a function of its state; WikiPageView owns the state and the requests. */
export function WikiPageUi({ page, path, knownTitle, pageError, onRetry, onRetrySource, cite, source, sourceError, onCite, onCloseSource, onOpenPage, onIndex, onOpenSession, onSave, loadHistory, onBack, onSearch, state, acts, docRef }: WikiPageProps & {
  state: WikiPageState;
  acts: WikiPageActs;
  /** The doc, for WikiPageView's link listener. */
  docRef?: RefObject<HTMLDivElement | null>;
}) {
  const { editing, saving, err, history, showHistory, histErr, menu } = state;
  const at = page?.path ?? path ?? "";
  const open = Boolean(cite) && Boolean(page);
  const topic =page?.topic ?? (at.startsWith("topics/") ? at.split("/")[1] : "");
  const title = page ? humanTitle(page.title, page.path) : knownTitle ? humanTitle(knownTitle, at) : humanTitle("", at);
  const notFound = !page && pageMissing(pageError);
  const missingSlug = at.split("/").pop()?.replace(/\.md$/, "");
  // A page written from a numbered view parses into nothing useful: show its text without the gutter instead.
  const numbered = page ? page.body.split("\n").filter((l) => /^\s*\d+\|/.test(l)).length >= 2 : false;
  return (
    <div className="thread">
      <header className="thread-head page-head">
        <Back onBack={onBack} label="Back to wiki index" />
        <Crumbs onIndex={onIndex} trail={topic ? [topic] : []} title={notFound ? missingSlug || "Page not found" : title} muted={notFound}
                slug={missingSlug} quiet={Boolean(page) && editing === null && !numbered} />
        {page && editing === null && (onSave || loadHistory) && (
          <div className="hk-acts wk-page-acts">
            {onSave && <button className="btn wk-wide" onClick={acts.edit}>Edit</button>}
            {loadHistory && <button className="btn wk-wide" aria-expanded={showHistory} onClick={acts.toggleHistory}>History</button>}
            <div className="wk-narrow wk-more">
              <button className="wk-x" aria-label="Page actions" aria-haspopup="menu" aria-expanded={menu}
                      onClick={acts.toggleMenu}>
                <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
                  <circle cx="5" cy="12" r="1.8" /><circle cx="12" cy="12" r="1.8" /><circle cx="19" cy="12" r="1.8" /></svg>
              </button>
              {menu && (
                <div className="overflow-menu" role="menu">
                  {onSave && <button role="menuitem" onClick={acts.menuEdit}>Edit</button>}
                  {loadHistory && <button role="menuitem" onClick={acts.menuHistory}>{showHistory ? "Hide history" : "History"}</button>}
                </div>
              )}
            </div>
          </div>
        )}
      </header>

      <div className="wk-split" data-source={open ? "1" : "0"}>
        <div className="scroll wk-doc" ref={docRef}>
          {!page ? (
            notFound ? (
              <EmptyState glyph="missing" title="This page doesn’t exist" primary={Boolean(onSearch)}
                action={onSearch ? { label: "Search the wiki", onClick: () => onSearch(missingSlug ?? at) } : { label: "Back to wiki", onClick: onIndex }}
                secondary={onSearch ? { label: "Back to wiki", onClick: onIndex } : undefined}>
                Nothing lives at <code className="mono">{at}</code>. A later ingest may have renamed or merged it.
              </EmptyState>
            ) : <Pending what="The page" err={pageError} onRetry={onRetry} lines={pageError ? 0 : 6} />
          ) : (<>
          {err && <p className="hk2-alert">{err}</p>}
          {(malformedWhy(page) || numbered) && (
            <div className="hk2-alert wk-malformed" role="note">
              <strong>This page is malformed</strong>
              <p>{malformedWhy(page).trim() ? capitalize(malformedWhy(page)) + "." : ""}{" "}
                {numbered ? "Shown below with the line numbers removed; claims and citations are not checked until it is recompiled."
                  : "Claims and citations may be missing below; Edit shows the raw text."}</p>
            </div>
          )}
          {showHistory && (
            <section className={"wk-history" + (history?.length === 0 ? " wk-history-none" : "")} aria-labelledby="wk-history-h">
              <div className="wk-history-head">
                <h2 id="wk-history-h" className="wk-h">History</h2>
                {history?.length === 0 && <p className="wk-facts">No recorded changes: the wiki is not a git repo.</p>}
                <button className="wk-x" onClick={acts.closeHistory} aria-label="Close history">
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8"
                       strokeLinecap="round" aria-hidden="true"><path d="M6 6l12 12M18 6L6 18" /></svg>
                </button>
              </div>
              {history === null ? <Pending what="History" err={histErr} onRetry={acts.retryHistory} inline />
                : history.length === 0 ? null
                // Versions are listed, not opened: the API has no per-version diff.
                : <ol className="wk-history-list">{history.map((h) => (
                  <li key={h.hash}>
                    <time className="wk-history-when" dateTime={h.at} title={new Date(h.at).toLocaleString()}>{stamp(h.at)}</time>
                    <span className="wk-history-what">{shortIds(h.subject)}</span>
                    <span className="mono wk-facts" title="Commit">{h.hash}</span>
                  </li>
                ))}</ol>}
            </section>
          )}
          {editing !== null && onSave ? (
            <div className="hk-panel">
              <label className="visually-hidden" htmlFor="wk-body">Page text</label>
              <textarea id="wk-body" className="hk-edit mono" rows={24} spellCheck={false}
                        value={editing} onChange={(e) => acts.type(e.target.value)} />
              <div className="hk-acts">
                <button className="btn btn-primary" disabled={saving} onClick={acts.save}>Save</button>
                <button className="btn" onClick={acts.cancel}>Cancel</button>
                <span className="hk-note">Saving commits the change to the wiki’s history.</span>
              </div>
            </div>
          ) : numbered ? (
            <div className="wk-text wk-denumbered"><Markdown text={literalUnderscores(denumber(page.body))} /></div>
          ) : (
            <>
              <div className="wk-titleblock">
                <h1 className="wk-h1">{title}</h1>
                <p className="wk-meta">
                  {page.updated && <>Updated {stamp(page.updated)} · </>}
                  {page.sessions.length ? `compiled from ${plural(page.sessions.length, "session")}` : "not compiled from a session"} · {countsLine(page.counts)}
                </p>
              </div>
              {page.blocks.map((b) => {
                switch (b.kind) {
                  case "heading": return <h2 key={b.line} className="wk-h">{b.text}</h2>;
                  case "lede":
                    return (
                      <div key={b.line} className="wk-claim">
                        <div className="wk-text wk-lede"><Markdown text={literalUnderscores(b.text)} /><Cites block={b} cite={cite} onCite={onCite} /></div>
                        <div />
                      </div>
                    );
                  case "claim": return <Claim key={b.line} block={b} cite={cite} onCite={onCite} />;
                  case "links": return <div key={b.line} className="wk-links"><Markdown text={literalUnderscores("- " + b.text)} /></div>;
                  default: return <div key={b.line} className="wk-code"><Markdown text={b.text} /></div>;
                }
              })}
              {page.linkedFrom.length > 0 && (
                <div className="wk-pagefoot">
                  <details className="hk2-more">
                    <summary>Linked from {plural(page.linkedFrom.length, "page")}</summary>
                    <div className="hk2-more-body">
                      {page.linkedFrom.map((p) => (
                        <p key={p.path} className="hk2-note"><button className="wk-link" onClick={() => onOpenPage(p.path)}>{humanTitle(p.title, p.path)}</button></p>
                      ))}
                    </div>
                  </details>
                </div>
              )}
            </>
          )}
          </>)}
        </div>
        {open && (
          <WikiSourcePane source={source ?? null} error={sourceError} onClose={onCloseSource} onRetry={onRetrySource}
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

export type Filter = "all" | WikiFlag["kind"];

type WikiReviewProps = {
  data: WikiReviewData;
  onOpenPage: (path: string, cite?: WikiCite) => void;
  onAct: (f: WikiFlag, action: "inference" | "drop") => Promise<void>;
  onSearch?: (text: string) => void;
  onIngest?: (session: string) => Promise<void>;
  onIndex: () => void;
  onBack?: () => void;
};

const flagKey = (f: WikiFlag) => `${f.page}:${f.line}:${f.kind}`;

export function WikiReviewView(props: WikiReviewProps) {
  const { onAct } = props;
  const [filter, setFilter] = useState<Filter>("all");
  const [busy, setBusy] = useState<Record<string, string>>({});
  const act = (f: WikiFlag, action: "inference" | "drop") => {
    const key = flagKey(f);
    setBusy((b) => ({ ...b, [key]: "…" }));
    onAct(f, action)
      .then(() => setBusy((b) => { const n = { ...b }; delete n[key]; return n; }))
      .catch((e) => setBusy((b) => ({ ...b, [key]: msg(e) })));
  };
  return <WikiReviewUi {...props} filter={filter} busy={busy} onFilter={setFilter} onDecide={act} />;
}

/**
 * WikiReviewView's markup as a function of its state: the filter, and per
 * flag "…" while its decision is out or the error it came back with.
 */
export function WikiReviewUi({ data, onOpenPage, onSearch, onIngest, onIndex, onBack, filter, busy, onFilter, onDecide }: WikiReviewProps & {
  filter: Filter;
  busy: Record<string, string>;
  onFilter: (f: Filter) => void;
  onDecide: (f: WikiFlag, action: "inference" | "drop") => void;
}) {
  const counts = useMemo(() => {
    const m: Record<string, number> = {};
    for (const f of data.flags) m[f.kind] = (m[f.kind] ?? 0) + 1;
    return m;
  }, [data.flags]);
  const shown = filter === "all" ? data.flags : data.flags.filter((f) => f.kind === filter);

  return (
    <div className="thread">
      <header className="thread-head page-head">
        <Back onBack={onBack} />
        <Crumbs onIndex={onIndex} title="Review" />
        <span className="head-repo">
          {data.flags.length === 0 ? "nothing to look at" : `${plural(data.flags.length, "claim")} the last check could not stand behind`}
        </span>
      </header>
      <div className="scroll proj-body">
        {Object.keys(counts).length > 1 && (
          <div className="wk-filters" role="group" aria-label="Show">
            <button className="wk-filter" aria-pressed={filter === "all"} onClick={() => onFilter("all")}>All {data.flags.length}</button>
            {(["unsupported", "superseded", "uncited", "problem"] as const).filter((k) => counts[k]).map((k) => (
              <button key={k} className="wk-filter" aria-pressed={filter === k} onClick={() => onFilter(k)}>
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
            const k = flagKey(f);
            const state = busy[k];
            // Disabled only while the decision is in flight: after "Did not save" the person tries again.
            const inFlight = state === "…";
            return (
              <div key={k} className="wk-item">
                <div className="wk-title-line">
                  <span className={flagWord[f.kind].cls}>{flagWord[f.kind].word}</span>
                  <span className="wk-facts">{f.why}</span>
                  <button className={"wk-where" + (f.kind === "problem" ? " wk-where-inline" : "")} onClick={() => onOpenPage(f.page, f.kind === "superseded" ? f.cite : undefined)}>
                    {f.page}:{f.line}
                  </button>
                  {f.kind === "problem" && <button className="wk-link" onClick={() => onOpenPage(f.page)}>Open the page</button>}
                </div>
                {f.claim && <div className="wk-item-claim"><Markdown text={f.claim} /></div>}
                {f.evidence && <pre className={"wk-ev" + (f.kind === "unsupported" ? " wk-ev-bad" : "")}>{f.evidence}</pre>}
                {f.kind !== "problem" && <div className="wk-acts">
                  {f.kind === "superseded" && f.cite && (
                    <button className="btn" onClick={() => onOpenPage(f.page, f.cite)}>Open the newer entry</button>
                  )}
                  {(f.kind === "unsupported" || f.kind === "uncited") && onSearch && (
                    <button className="btn" onClick={() => onSearch(f.claim)}>Search history</button>
                  )}
                  {(f.kind === "unsupported" || f.kind === "uncited") && (
                    <button className="btn" disabled={inFlight} onClick={() => onDecide(f, "inference")}>Mark as inference</button>
                  )}
                  <button className="btn" disabled={inFlight} onClick={() => onDecide(f, "drop")}>
                    {f.kind === "superseded" ? "Drop the old claim" : "Drop the claim"}
                  </button>
                  {state && state !== "…" && <span className="hk-state hk-bad">Did not save — {state}</span>}
                </div>}
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
      <span className="wk-counts">session {shortId(p.id)} · {plural(p.entries, "entry", "entries")} · {stamp(p.last)}</span>
      {onIngest && (
        // Disabled only while the POST is out: a started run can meet the
        // lock or time out, and the session is then still waiting here.
        <button className="btn" disabled={state === "starting"} onClick={() => {
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
  // Neutral outcomes wear the same chip shape as the coloured ones.
  if (ds.length && ds.every((d) => d === "no material")) return { word: "No material", cls: "hk2-state wk-state-quiet" };
  return { word: r.command.startsWith("/llm-wiki ingest") ? "Unlogged" : "Run", cls: "hk2-state wk-state-quiet" };
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
      <header className="thread-head page-head">
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
        <div className="hk2-sum wk-stats">
          <span className="wk-stat-group">
            <span className="eyebrow">Today</span>
            <span><span className="hk2-sum-n">{t.runs}</span> <span className="hk2-sum-lab">{t.runs === 1 ? "run" : "runs"}</span></span>
            <span><span className="hk2-sum-n">{t.ingested}</span> <span className="hk2-sum-lab">compiled</span></span>
            <span><span className="hk2-sum-n">{t.noMaterial}</span> <span className="hk2-sum-lab">no material</span></span>
            <span><span className="hk2-sum-n">{money(t.spent)}</span> <span className="hk2-sum-lab">spent</span></span>
          </span>
          <span className="wk-stat-group">
            <span className="eyebrow">Queue</span>
            <span><span className={"hk2-sum-n" + (data.pending ? " is-waiting" : "")}>{data.pending}</span> <span className="hk2-sum-lab">waiting</span></span>
            <span><span className="hk2-sum-lab">{data.every ? `scheduler every ${duration(data.every)}` : "not scheduled"}</span></span>
            <span><span className="hk2-sum-n">{money(data.spent)}</span> <span className="hk2-sum-lab">spent all time</span></span>
          </span>
        </div>

        {data.runs.length === 0 ? (
          <EmptyState title="No ingest has run yet">
            A run starts when a finished session has been quiet for half an hour{onIngest ? ", or when you press Ingest now" : ""}.
          </EmptyState>
        ) : (
          <div className="wk-list">
            {data.runs.map((r, i) => {
              const w = runWord(r);
              const prev = data.runs[i - 1];
              const gap = prev ? Date.parse(prev.at) - Date.parse(r.done ?? r.at) : 0;
              // One line a page: a page both created and updated in one run reads as new.
              const touched = new Map<string, boolean>();
              for (const o of r.outcomes) for (const p of o.pages) touched.set(p, touched.get(p) || o.disposition.toLowerCase().startsWith("new"));
              const pages = [...touched.keys()];
              const other = r.files.filter((f) => f !== "log.md" && f !== "index.md" && !pages.includes(f));
              return (
                <div key={r.session} style={{ display: "contents" }}>
                  {gap > 60 * 60_000 && (
                    <div className="wk-quiet">{hhmm(r.done ?? r.at)} – {hhmm(prev!.at)} · nothing ingested</div>
                  )}
                  <div className="wk-run">
                    <div className="wk-run-head">
                      <span className="wk-run-when" title={new Date(r.at).toLocaleString()}>
                        {stamp(r.at)}
                      </span>
                      <span className={w.cls}>{w.word}</span>
                      <span className="wk-run-what">
                        {r.outcomes.length > 0
                          ? r.outcomes.map((o) => o.title || `session ${shortId(o.id)}`).join(" · ")
                          : r.command ? shortIds(r.command.replace(/^\/llm-wiki ingest\b/, "Ingest")).trim() || "Ingest of all waiting sessions"
                          : "An ingest"}
                      </span>
                      <span className="wk-run-meta">
                        {r.running ? "running" : took(r.ms)} · {money(r.cost)}
                        {!r.commit && !r.running && <> · not committed</>}
                        {onOpenSession ? <> · <button className="wk-link mono" title="Open the run’s transcript"
                            aria-label={`Open the transcript${r.commit ? ` of commit ${r.commit}` : ""}`}
                            onClick={() => onOpenSession(r.session)}>{r.commit || "transcript"}</button></>
                          : r.commit && <> · {r.commit}</>}
                      </span>
                    </div>
                    {(pages.length > 0 || other.length > 0) && (
                      <ul className="wk-diff">
                        {/* One legend: + new, ~ changed; a page links, any other file is plain text. */}
                        {[...touched].map(([p, added]) => (
                          <li key={p}>
                            <span className={added ? "wk-add" : "wk-mod"} title={added ? "New page" : "Changed page"}>{added ? "+ new" : "~ changed"}</span>{"  "}
                            <button className="wk-link mono" onClick={() => onOpenPage(p)}>{p}</button>
                          </li>
                        ))}
                        {other.map((f) => <li key={f}><span className="wk-mod" title="Changed file">~ changed</span>{"  "}<span className="wk-facts mono">{f}</span></li>)}
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

export function useLoad<T>(load: (() => Promise<T>) | null, key: string, poll = 0) {
  const [data, setData] = useState<T | null>(null);
  const [err, setErr] = useState("");
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const reload = useCallback(() => {
    if (!load) return Promise.resolve(true);
    return load().then((d) => { setData(d); setErr(""); return true; }).catch((e) => { setErr(msg(e)); return false; });
  }, [key]);
  const retry = () => { setErr(""); void reload(); };
  useEffect(() => {
    setData(null); setErr("");
    // The next poll is armed when this read settles, never on a clock:
    // a slow read cannot stack ticks behind it, and failures back off.
    let live = true;
    let t: ReturnType<typeof setTimeout> | undefined;
    let wait = poll;
    const tick = () => reload().then((ok) => {
      if (!live || !poll) return;
      wait = ok ? poll : Math.min(wait * 2, 5 * 60_000);
      t = setTimeout(tick, wait);
    });
    tick();
    return () => { live = false; clearTimeout(t); };
  }, [reload, poll]);
  return { data, err, reload, retry };
}

/** Every wiki screen keeps its head while its body loads: a wait is a state of the screen, not a blank page. */
export function Loading({ what, title, err, onBack, onIndex, onRetry }: {
  what: string; title: string; err: string; onBack?: () => void; onIndex?: () => void; onRetry: () => void;
}) {
  return (
    <div className="thread">
      <header className="thread-head page-head">
        <Back onBack={onBack} />
        {onIndex ? <Crumbs onIndex={onIndex} title={title} /> : <div className="head-main"><h1>{title}</h1></div>}
      </header>
      <div className="scroll proj-body">
        <Pending what={what} err={err} onRetry={onRetry} lines={err ? 0 : 5} />
      </div>
    </div>
  );
}

// Titles seen on the way to a page, so its header can name it before it loads.
const knownTitles = new Map<string, string>();
const learn = (refs: WikiPageRef[]) => { for (const p of refs) knownTitles.set(p.path, p.title); };

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
  const [ingestErr, setIngestErr] = useState("");
  // Activity's note is about the click that just happened: WikiPage stays
  // mounted across wiki routes, so without this a "Started" read as news
  // on every later visit to Activity.
  useEffect(() => setNote(""), [route.at]);
  // Stable, so the source pane's Escape listener is not rebound every render.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const closeSource = useCallback(() => onRoute({ at: "page", path }), [path]);
  if (index.data) for (const t of index.data.topics) learn(t.pages);
  if (page.data) { learn([page.data]); learn(page.data.linkedFrom); }
  if (source.data) learn(source.data.citedBy);

  switch (route.at) {
    case "index":
      return index.data
        ? <WikiIndexView data={index.data} onBack={onBack} onOpen={(p) => toPage(p)}
                         ingestErr={ingestErr}
                         onIngest={() => { setIngestErr(""); wikiApi.ingest().then(() => index.reload()).catch((e) => setIngestErr(msg(e))); }}
                         onReview={() => onRoute({ at: "review" })} onActivity={() => onRoute({ at: "activity" })}
                         check={wikiApi.check} />
        : <Loading what="The wiki" title="Wiki" err={index.err} onBack={onBack} onRetry={index.retry} />;
    case "review":
      return review.data
        ? <WikiReviewView data={review.data} onBack={onBack} onIndex={toIndex} onSearch={onSearch}
                          onOpenPage={(p, c) => toPage(p, c)}
                          onAct={(f, a) => wikiApi.claim(f, a).then(() => { review.reload(); })}
                          onIngest={(id) => wikiApi.ingest(id)} />
        : <Loading what="Review" title="Review" err={review.err} onBack={onBack} onIndex={toIndex} onRetry={review.retry} />;
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
        : <Loading what="Activity" title="Activity" err={activity.err} onBack={onBack} onIndex={toIndex} onRetry={activity.retry} />;
    case "page":
      return (
        <WikiPageView page={page.data} onSearch={onSearch}
                      path={route.path} knownTitle={knownTitles.get(route.path)}
                      pageError={page.err} onRetry={page.retry} onRetrySource={source.retry}
                      cite={cite ?? null} source={source.data} sourceError={source.err}
                      onBack={onBack} onIndex={toIndex} onOpenSession={onOpenSession}
                      onCite={(c) => toPage(route.path, { session: c.session, seq: c.seq })}
                      onCloseSource={closeSource}
                      onOpenPage={(p) => toPage(p)}
                      onSave={(body) => wikiApi.save(route.path, body).then(() => { page.reload(); })}
                      loadHistory={() => wikiApi.history(route.path)} />
      );
  }
}
