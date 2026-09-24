import { useEffect, useMemo, useRef, useState, type RefObject } from "react";
import { createPortal } from "react-dom";
import { useModal } from "./dialog";
import type { Row, Status } from "./types";
import { ago } from "./loading";
import { hasOwnTitle, plainTitle, sessionTitle, titleKey } from "./render";
import { StatusMark, shownStatus, statusWord } from "./status";
import { isMac, paletteKeyOpens } from "./keys";

/**
 * ⌘K. With 150 conversations the sidebar is a scroll, not an index —
 * the fastest way to a session is to name it. Everything reachable
 * from the chrome is reachable here too, so the keyboard never has to
 * hand back to the mouse.
 */
export interface Command {
  id: string;
  label: string;
  hint?: string;
  group: string;
  run: () => void;
  /** Shown under the row while it is selected: why this result is here. */
  detail?: string;
  /** Offered before anything is typed; the rest wait to be searched for. */
  suggest?: boolean;
  /** Never the default selection: it has to be chosen on purpose. */
  destructive?: boolean;
  /** A session's status, drawn as its glyph ahead of the label. */
  mark?: Status;
}

interface WikiHit { path: string; title: string; topic: string; excerpt: string; counts: { cited: number } }

/** Pages whose claims mention the query, from the wiki's own search. */
function useWikiHits(q: string, open: boolean, on: boolean): WikiHit[] {
  const [hits, setHits] = useState<WikiHit[]>([]);
  useEffect(() => {
    const needle = q.trim();
    if (!on || !open || needle.length < 2) { setHits([]); return; }
    const ctl = new AbortController();
    const t = setTimeout(() => {
      getJSON<{ hits?: WikiHit[] }>("/api/wiki/search?q=" + encodeURIComponent(needle), ctl.signal)
        .then((d) => setHits(Array.isArray(d.hits) ? d.hits : []))
        .catch(() => { if (!ctl.signal.aborted) setHits([]); });
    }, 180);
    return () => { ctl.abort(); clearTimeout(t); };
  }, [q, open, on]);
  return hits;
}

interface DirHit { path: string; exists: boolean; checkout?: string }

/** A query that names a folder, not a message: `/x`, `~` or `~/x`. */
export function isPathQuery(q: string): boolean {
  return /^(\/|~(\/|$))/.test(q.trim());
}

export { isTypingTarget } from "./keys";

/**
 * A typed path is a place to start, ahead of everything else: it is never
 * silently a first message, which is what a pasted ~/repos/x used to become.
 */
export function pathRows(typed: string, places: { folder: DirHit | null; dirs: DirHit[]; home: string }, startIn?: (path: string) => void): Command[] {
  if (!startIn || !isPathQuery(typed)) return [];
  const short = (p: string) => places.home && p.startsWith(places.home) ? "~" + p.slice(places.home.length) : p;
  const can = (d: DirHit) => d.checkout ? `can edit ${short(d.checkout)}` : "read-only";
  const folders = [...(places.folder?.exists ? [places.folder] : []), ...places.dirs];
  const rows = folders.map((d, i): Command => ({
    id: "dir:" + d.path,
    // The leaf names the row; the path, cut from its start, tells same-named folders apart.
    label: `New session in ${short(d.path).replace(/(.)\/+$/, "$1").split("/").filter(Boolean).pop() ?? short(d.path)}`,
    hint: short(d.path).replace(/(.)\/+$/, "$1"),
    detail: can(d),
    group: "Start",
    // Nothing is created here: the palette stays, aimed at the folder, for the first prompt.
    run: () => startIn(d.path),
  }));
  // Nothing matched and the server says the folder is missing: say so rather than show nothing.
  if (!rows.length && places.folder && !places.folder.exists) {
    rows.push({ id: "dir:" + places.folder.path, label: `New session in ${typed}`, hint: "Folder not found", group: "Start", run: () => {} });
  }
  return rows;
}

const NO_DIRS = { folder: null, dirs: [] as DirHit[], home: "" };

/** The typed folder and the folders its path completes to, from the server. */
function useDirs(q: string, open: boolean, on: boolean): { folder: DirHit | null; dirs: DirHit[]; home: string } {
  // An answer belongs to the path that asked for it: after Tab wrote
  // `~/work/` the rows for `~/wor` stayed up until the new answer came,
  // and Enter there started the folder of a path no longer typed.
  const [res, setRes] = useState<{ q: string; folder: DirHit | null; dirs: DirHit[]; home: string }>({ q: "", ...NO_DIRS });
  useEffect(() => {
    const typed = q.trim();
    if (!on || !open || !isPathQuery(typed)) { setRes({ q: "", ...NO_DIRS }); return; }
    const ctl = new AbortController();
    const t = setTimeout(() => {
      getJSON<{ folder?: DirHit; dirs?: DirHit[] }>("/api/dirs?path=" + encodeURIComponent(typed), ctl.signal)
        .then((d) => {
          const folder = d.folder ?? null;
          // The server expands ~ by prefixing home, so home is what the typed rest does not account for.
          const home = folder && typed.startsWith("~") ? folder.path.slice(0, folder.path.length - (typed.length - 1)) : "";
          setRes({ q: typed, folder, dirs: Array.isArray(d.dirs) ? d.dirs : [], home });
        })
        .catch(() => { if (!ctl.signal.aborted) setRes({ q: typed, ...NO_DIRS }); });
    }, 120);
    return () => { ctl.abort(); clearTimeout(t); };
  }, [q, open, on]);
  return res.q === q.trim() ? res : NO_DIRS;
}

/**
 * A search read that always settles: a bad status, a body that is not
 * JSON, or a server that never answers (15s) rejects, and an abort from
 * a newer query rejects too, so no footer waits forever.
 */
function getJSON<T>(url: string, signal: AbortSignal): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const slow = setTimeout(() => reject(new Error("timed out")), 15_000);
    fetch(url, { signal })
      .then((r) => { if (!r.ok) throw new Error(String(r.status)); return r.json() as Promise<T>; })
      .then(resolve, reject)
      .finally(() => clearTimeout(slow));
  });
}

/**
 * How well text answers q. Prefix beats substring beats scattered.
 *
 * `loose` allows a subsequence match, which is right for a short
 * command label ("arch" → "Archive this conversation") and wrong for a
 * session title: a prompt two sentences long contains almost any short
 * query's letters in order, so "grafana" matched paragraphs about
 * gitops. Titles are matched strictly and the full-text search finds
 * what a title cannot.
 */
function score(text: string, q: string, loose: boolean): number {
  if (!q) return 0;
  const t = text.toLowerCase();
  const i = t.indexOf(q);
  if (i === 0) return 1000;       // prefix
  if (i > 0) return 500 - i;      // substring, earlier is better
  if (!loose) return -1;
  let at = 0;
  for (const ch of q) {
    at = t.indexOf(ch, at) + 1;
    if (at === 0) return -1;      // not a subsequence either
  }
  return 100;
}

/**
 * Close first, run once the close has committed: a command that opens a
 * dialog (Keyboard shortcuts) found the palette still aria-modal and no-opped.
 */
export function afterClose(close: () => void, run: () => void) {
  close();
  setTimeout(run, 0);
}

/** The palette is over a route: a hash change, Back, or a layout switch closes it. Returns the unsubscribe. */
export function closeOnNavigate(win: Pick<Window, "addEventListener" | "removeEventListener">, close: () => void, mq: MediaQueryList | null): () => void {
  win.addEventListener("hashchange", close);
  win.addEventListener("popstate", close);
  mq?.addEventListener("change", close);
  return () => {
    win.removeEventListener("hashchange", close);
    win.removeEventListener("popstate", close);
    mq?.removeEventListener("change", close);
  };
}

/** A session or wiki page, as opposed to a command or a place to start. */
export const isResult = (c: Command) => c.id.startsWith("s:") || c.id.startsWith("w:");

/** Results counted in the footer: the Start action is an offer, not a match. */
export const shownCount = (hits: Command[]) => hits.filter((c) => !c.id.startsWith("start:")).length;

/** A project the operator hint can name: the newest session's repo, or none. */
export function syntaxProject(rows: Row[]): string | null {
  const r = [...rows].filter((x) => x.repo).sort((a, b) => Date.parse(b.lastAt) - Date.parse(a.lastAt))[0];
  return r?.repo?.split("/").filter(Boolean).pop() ?? null;
}

/** Visit order, most recent first, each session once. */
export function visit(visited: string[], id: string): string[] {
  return [id, ...visited.filter((x) => x !== id)].slice(0, 50);
}

/** The sessions the sidebar lists as rows: a background agent whose parent is listed is not one. */
export function listable(rows: Row[]): Row[] {
  const ids = new Set(rows.map((r) => r.id));
  return rows.filter((r) => !(r.spawnedBy && ids.has(r.spawnedBy)));
}

/** A leading ">" asks for commands only, as in an editor's palette. */
export function commandQuery(q: string): { only: boolean; text: string } {
  const t = q.trimStart();
  return t.startsWith(">") ? { only: true, text: t.slice(1).trim() } : { only: false, text: q };
}

/** Sessions to switch to: the ones visited, last first (so Enter goes back), then the rest by activity. */
export function recentSessions(rows: Row[], current: string | null, visited: string[], n: number): Row[] {
  const at = (id: string) => { const i = visited.indexOf(id); return i < 0 ? Infinity : i; };
  // Empty sessions the sidebar shows (live, or a failed orb) are reachable here too.
  return listable(rows)
    .filter((r) => !r.archived && !(r.empty && !r.live && r.orb?.status !== "failed") && r.id !== current)
    .sort((a, b) => at(a.id) - at(b.id) || Date.parse(b.lastAt) - Date.parse(a.lastAt))
    .slice(0, n);
}

/** The folders sessions ran in, newest first, each once; home has its own entry. */
export function startFolders(rows: Row[], home: string, n = 8): string[] {
  const seen = new Set<string>();
  for (const r of [...rows].sort((a, b) => Date.parse(b.lastAt) - Date.parse(a.lastAt))) {
    if (r.cwd && r.cwd !== home && !r.orb) seen.add(r.cwd);
  }
  return [...seen].slice(0, n);
}

/** The operators the palette reads out of a query; the rest is text. */
export interface Ops { text: string; project?: string; after?: number; status?: string }

const OP = /^(project|after|status):(\S+)$/i;

export function parseOps(q: string): Ops {
  const o: Ops = { text: "" };
  const words: string[] = [];
  for (const w of q.trim().split(/\s+/).filter(Boolean)) {
    const m = OP.exec(w);
    if (!m) { words.push(w); continue; }
    const name = m[1].toLowerCase(), val = m[2].toLowerCase();
    if (name === "project") o.project = val;
    else if (name === "status") o.status = val;
    else {
      const d = /^(\d+)([hdw])$/.exec(val);
      const at = d ? Date.now() - Number(d[1]) * { h: 3_600_000, d: 86_400_000, w: 604_800_000 }[d[2] as "h" | "d" | "w"] : Date.parse(val);
      if (Number.isFinite(at)) o.after = at; else words.push(w);
    }
  }
  o.text = words.join(" ");
  return o;
}

export const hasOps = (o: Ops) => o.project !== undefined || o.after !== undefined || o.status !== undefined;

/** A row passes the operators: status matches the API word or the one the UI shows. */
export function matchesOps(r: Row, o: Ops): boolean {
  if (o.project && !(r.repo ?? "").toLowerCase().includes(o.project)) return false;
  if (o.after !== undefined && !(Date.parse(r.lastAt) >= o.after)) return false;
  if (o.status) {
    const s = shownStatus(r);
    const alias: Record<string, string> = { failed: "error", waiting: "needs-you" };
    if (s !== (alias[o.status] ?? o.status) && statusWord(s).toLowerCase() !== o.status) return false;
  }
  return true;
}

/** text cut into runs, the ones matching a term flagged for <mark>. */
export function markParts(text: string, terms: string[]): { t: string; m: boolean }[] {
  const ts = terms.filter(Boolean).map((t) => t.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
  if (!ts.length) return [{ t: text, m: false }];
  const re = new RegExp(`(${ts.join("|")})`, "gi");
  return text.split(re).filter(Boolean).map((t) => ({ t, m: ts.some((x) => new RegExp(`^${x}$`, "i").test(t)) }));
}

function Marked({ text, terms }: { text: string; terms: string[] }) {
  return <>{markParts(text, terms).map((p, i) => p.m ? <mark key={i}>{p.t}</mark> : p.t)}</>;
}

interface SearchLine { seq: number; kind: string; text: string }
interface SearchHit { id: string; title: string; repo: string; branch: string; hits: number; lines: SearchLine[] }

/**
 * Titles are written by a small model after the first turn. What you
 * actually remember is something that was SAID — a file name, an error,
 * a command — so the palette asks the server to search the bodies too.
 * Local matching answers instantly and this fills in behind it.
 */
type SearchState = "idle" | "loading" | "done" | "error";

export function useFullText(q: string, open: boolean): { hits: SearchHit[]; state: SearchState; retry: () => void } {
  const [hits, setHits] = useState<SearchHit[]>([]);
  const [tries, setTries] = useState(0);
  const [state, setState] = useState<SearchState>("idle");
  // Hits belong to the query that asked for them; an older query's
  // answer never sits under a newer one while it is still loading.
  const [forQ, setForQ] = useState("");
  useEffect(() => {
    const needle = q.trim();
    if (!open || needle.length < 2) { setHits([]); setForQ(""); setState("idle"); return; }
    setState("loading");
    // Debounced: this reads every transcript, and the box is typed into
    // one character at a time. A newer query aborts the older read.
    const ctl = new AbortController();
    const t = setTimeout(() => {
      getJSON<{ hits?: SearchHit[] }>("/api/search?q=" + encodeURIComponent(needle), ctl.signal)
        .then((d) => { if (!ctl.signal.aborted) { setHits(Array.isArray(d.hits) ? d.hits : []); setForQ(needle); setState("done"); } })
        // A failed search is not "no matches": say so.
        .catch(() => { if (!ctl.signal.aborted) { setHits([]); setForQ(needle); setState("error"); } });
    }, 180);
    return () => { ctl.abort(); clearTimeout(t); };
  }, [q, open, tries]);
  const current = forQ === q.trim();
  // An answer for an older query is still loading for this one.
  return { hits: current ? hits : [], state: state === "loading" || current || state === "idle" ? state : "loading", retry: () => setTries((n) => n + 1) };
}

interface PaletteProps {
  open: boolean;
  /** "switch" is ⌘P: sessions only, in visit order. "new" is ⌥N: only the places to start. */
  mode?: "all" | "switch" | "new";
  /** Sessions opened, most recent first. */
  visited?: string[];
  /** Folder name the Start entry starts in, when New aimed the palette at one. */
  startIn?: string;
  onClose: () => void;
  rows: Row[];
  commands: Command[];
  /** seq and q, when given, land on the transcript line that matched and mark it. */
  onOpenSession: (id: string, seq?: number, q?: string) => void;
  /** Start a conversation with what was typed as its first message. */
  onStart?: (text: string) => void;
  /** Aim Start at a folder; when set, a typed path offers its folders. */
  onStartIn?: (path: string) => void;
  /** Open a wiki page; when set, the wiki's pages are searched too. */
  onOpenWikiPage?: (path: string) => void;
  /** Seeds the box. Only a story uses it: nothing can type for us there. */
  initialQuery?: string;
  /** The open session: it ranks below other matches and says it is the one you are in. */
  current?: string | null;
}

export function Palette(props: PaletteProps) {
  const { open, onClose, onStartIn, onOpenWikiPage, initialQuery = "", mode = "all" } = props;
  const [q, setQ] = useState(initialQuery);
  // The selection is a result, not a position: results that land late
  // (full-text, wiki) never move Enter onto something else.
  const [atId, setAtId] = useState<string | null>(null);
  const field = useRef<HTMLInputElement>(null);
  const opener = useRef<Element | null>(null);

  // Focus goes back to the opener only once the palette is gone and the
  // page behind it is no longer inert.
  useEffect(() => {
    if (open) return;
    const el = opener.current as HTMLElement | null;
    opener.current = null;
    if (el?.isConnected && !document.querySelector("[aria-modal='true']")) el.focus?.();
  }, [open]);

  // Reset while rendering the open, not in the effect below: the effect
  // focused a field still showing the last query, and keys typed before
  // its deferred reset render were appended to that stale text.
  const [shown, setShown] = useState({ open, initialQuery });
  if (shown.open !== open || shown.initialQuery !== initialQuery) {
    setShown({ open, initialQuery });
    if (open) { setQ(initialQuery); setAtId(null); }
  }

  useEffect(() => {
    if (!open) return;
    opener.current = document.activeElement;
    field.current?.focus();
  }, [open, initialQuery]);

  useEffect(() => {
    if (!open) return;
    return closeOnNavigate(window, onClose, window.matchMedia?.("(max-width:720px)") ?? null);
  }, [open, onClose]);

  const search = useFullText(q, open && mode !== "new" && !commandQuery(q).only);
  const pages = useWikiHits(q, open, Boolean(onOpenWikiPage) && mode === "all" && !commandQuery(q).only);
  const places = useDirs(q, open, Boolean(onStartIn));
  return <PaletteView {...props} q={q} setQ={setQ} atId={atId} setAtId={setAtId} search={search} pages={pages} places={places} field={field} opener={opener} />;
}

/**
 * The palette drawn from its state alone: what is typed, the row picked,
 * and what the server has answered. Palette owns that state and its
 * effects, so a test can render any state the palette reaches from here.
 */
export function PaletteView({ open, onClose, rows, commands, onOpenSession, onStart, onStartIn, onOpenWikiPage, current = null, startIn, mode = "all", visited = [],
  q, setQ, atId, setAtId, search, pages, places, field, opener }: PaletteProps & {
  q: string;
  setQ: (q: string) => void;
  atId: string | null;
  setAtId: (id: string | null) => void;
  search: { hits: SearchHit[]; state: SearchState; retry: () => void };
  pages: WikiHit[];
  places: { folder: DirHit | null; dirs: DirHit[]; home: string };
  field: RefObject<HTMLInputElement | null>;
  opener: RefObject<Element | null>;
}) {
  const list = useRef<HTMLDivElement>(null);
  const box = useRef<HTMLDivElement>(null);
  useModal(box, open);
  const { hits: found, state: searching, retry } = search;

  const hits = useMemo(() => {
    // project:, after: and status: narrow the sessions; the rest is matched.
    const cq = commandQuery(q);
    const ops = cq.only ? { text: cq.text } : parseOps(q);
    const narrowed = hasOps(ops);
    const needle = ops.text.toLowerCase();
    // With no query: what you would do next, not the sitemap.
    const cmds = commands
      .filter((c) => mode === "new" ? c.id.startsWith("new:") && !narrowed : mode === "all" && !narrowed && (needle || c.suggest || cq.only))
      .map((c) => ({ c, s: needle ? score(c.label, needle, true) : 10 }))
      .filter((x) => x.s >= 0);
    const listed = listable(rows);
    const sessions = listed
      .filter((r) => !cq.only && matchesOps(r, ops))
      .map((r) => {
        const title = sessionTitle(r);
        const s = needle ? Math.max(score(title, needle, false), score(r.repo ?? "", needle, false)) : 0;
        // The one you are in is rarely where you want to go.
        return { r, title, s: s >= 0 && r.id === current ? Math.min(s, 51) : s };
      })
      // Best first, then the cap: slicing first dropped strong matches.
      .filter((x) => mode !== "new" && (needle || narrowed) && x.s >= 0)
      .sort((a, b) => b.s - a.s);
    // With no query, the three sessions you touched last (not this one):
    // a list of 150 titles is the sidebar again.
    // ⌘P lists more of them, in the order you visited them.
    const recent = needle || narrowed || cq.only || mode === "new" ? [] : recentSessions(rows, current, visited, mode === "switch" ? 12 : 3);
    // Same-named sessions are told apart under the active row: branch,
    // age, and the line the full-text search matched, when it did.
    const foundBy = new Map(found.map((h) => [h.id, h]));
    // One candidate per session, whether its title or its text matched:
    // a text-only hit ranks below any title match but inside the same cap.
    const byId = new Map(sessions.map((x) => [x.r.id, x]));
    const texts = cq.only ? [] : found.filter((h) => !rows.some((x) => x.id === h.id && x.spawnedBy && rows.some((p) => p.id === x.spawnedBy)));
    for (const h of texts) {
      const r = listed.find((x) => x.id === h.id);
      if (!r || byId.has(h.id)) continue;
      byId.set(h.id, { r, title: sessionTitle({ ...r, title: plainTitle(r.title) || h.title }), s: h.id === current ? 49 : 50 });
    }
    const cands = [...byId.values()].sort((a, b) => b.s - a.s);
    // Titles that repeat carry the id's tail, the one thing always different.
    const twice = new Set<string>();
    const seenTitle = new Set<string>();
    const same = titleKey;
    for (const { title } of cands) (seenTitle.has(same(title)) ? twice : seenTitle).add(same(title));
    // The hint already says repo, status and age: the detail adds only what it does not.
    const detailFor = (r: Row, title: string) => {
      const h = foundBy.get(r.id);
      return [twice.has(same(title)) ? r.branch || h?.branch : "", evidence(h, title, needle)].filter(Boolean).join("\n") || undefined;
    };
    // One relevance order across commands and titles, so a weak match
    // in one kind never leapfrogs a strong one in the other.
    const ranked = [
      ...recent.map((r) => ({ s: 20, c: {
        id: "s:" + r.id,
        label: sessionTitle(r),
        hint: meta(r, r.repo, !hasOwnTitle(r)),
        mark: shownStatus(r),
        group: "Recent",
        run: () => onOpenSession(r.id),
      } as Command })),
      ...cmds.map((x) => ({ s: x.s, c: x.c })),
      ...cands.map(({ r, title, s }) => ({ s, c: {
        id: "s:" + r.id,
        label: title,
        hint: (r.id === current ? "Current · " : "") + meta(r, r.repo || foundBy.get(r.id)?.repo, twice.has(same(title)) || !hasOwnTitle(r)),
        mark: shownStatus(r),
        // A query that is only filters matched no text: its sessions are not "mentioned in" anything.
        group: s <= 50 && ops.text ? "Mentioned in" : "Sessions",
        detail: detailFor(r, title),
        run: () => onOpenSession(r.id),
      } as Command })),
    ];
    // Equal scores keep a group together, so ">" never repeats a heading.
    const firstOf = new Map<string, number>();
    ranked.forEach((x, i) => { if (!firstOf.has(x.c.group)) firstOf.set(x.c.group, i); });
    ranked.sort((a, b) => b.s - a.s || firstOf.get(a.c.group)! - firstOf.get(b.c.group)!);
    const all: Command[] = ranked.map((x) => x.c);
    // A wiki page carries the claim that matched: the point of the wiki
    // is the join between a page and the entry behind it, and a title
    // alone does not show which is which.
    if (onOpenWikiPage && mode === "all" && !cq.only) {
      for (const p of pages) {
        all.push({
          id: "w:" + p.path,
          label: p.title,
          hint: [p.topic, p.counts.cited ? `${p.counts.cited} cited` : ""].filter(Boolean).join(" · "),
          group: "Wiki",
          detail: p.excerpt,
          run: () => onOpenWikiPage(p.path),
        });
      }
    }
    // A text hit on a session the list does not hold (archived, hidden)
    // still opens, showing the line that matched.
    const seen = new Set(all.map((c) => c.id));
    for (const h of texts) {
      if (seen.has("s:" + h.id)) continue;
      const r = rows.find((x) => x.id === h.id);
      const label = sessionTitle({ id: h.id, title: h.title, lastAt: r?.lastAt } as Row);
      all.push({
        id: "s:" + h.id,
        label,
        hint: [h.repo?.split("/").pop(), idTail(h.id)].filter(Boolean).join(" · "),
        group: "Mentioned in",
        detail: [h.branch, evidence(h, label, needle)].filter(Boolean).join("\n") || undefined,
        run: () => onOpenSession(h.id, h.lines[0]?.seq, q.trim()),
      });
    }
    // Capped first; Start is added after, so a long result list never hides it.
    const capped = all.slice(0, 40);
    // A typed path is a place to start, ahead of everything else: it is
    // never a first message, which is what a pasted ~/repos/x used to become.
    const typed = q.trim();
    const placeRows = cq.only ? [] : pathRows(typed, places, onStartIn && ((path) => { onStartIn(path); setQ(""); setAtId(null); }));
    capped.unshift(...placeRows);
    // Last, always explicit: typing never starts anything by itself. A path
    // offers it only below its folder rows, so Enter never sends the path.
    if (onStart && !cq.only && typed.length >= 2 && !typed.includes(":") && (!isPathQuery(typed) || placeRows.length)) {
      capped.push({
        id: "start:" + typed,
        label: startIn ? `Start a session in ${startIn}: “${typed}”` : `Start a session: “${typed}”`,
        hint: startIn ? `in ${startIn} · sends it as the first message` : "sends it as the first message",
        group: "Start",
        run: () => onStart(typed),
      });
    }
    return capped;
  }, [q, rows, commands, onOpenSession, found, onStart, onStartIn, places, pages, onOpenWikiPage, current, startIn, mode, visited]);

  // Nothing picked yet: the first result that is not destructive, so Enter never archives by default.
  // When every result is destructive nothing is highlighted (-1): Math.max(0, …) put Enter on the first one.
  const at = atId === null ? hits.findIndex((c) => !c.destructive) : Math.max(0, hits.findIndex((c) => c.id === atId));
  const setAt = (f: (i: number) => number) => { const c = hits[f(at)]; if (c) setAtId(c.id); };
  useEffect(() => {
    list.current?.querySelector('[data-at="1"]')?.scrollIntoView({ block: "nearest" });
  }, [at, hits.length]);

  if (!open) return null;

  const close = () => onClose();
  const pick = (c: Command) => { opener.current = null; afterClose(onClose, c.run); };

  const keys = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") { e.preventDefault(); close(); return; }
    if (e.key === "ArrowDown") { e.preventDefault(); setAt((i) => Math.min(i + 1, hits.length - 1)); return; }
    if (e.key === "ArrowUp") { e.preventDefault(); setAt((i) => Math.max(i - 1, 0)); return; }
    // Tab walks into the selected folder, so a path is typed a segment at a time.
    const sel = hits[at];
    if (e.key === "Tab" && sel?.id.startsWith("dir:")) {
      e.preventDefault();
      const p = sel.id.slice(4).replace(/\/+$/, "") + "/";
      setQ(places.home && p.startsWith(places.home) ? "~" + p.slice(places.home.length) : p);
      setAtId(null);
      return;
    }
    if (e.key === "Enter" && !e.nativeEvent.isComposing) { e.preventDefault(); if (hits[at]) pick(hits[at]); }
  };

  const terms = parseOps(q).text.toLowerCase().split(/\s+/).filter((t) => t.length >= 2);
  const cq = commandQuery(q);
  const ops = cq.only ? { text: cq.text } : parseOps(q);
  const typed = q.trim();
  // Recognised filters, as typed: an after: value that did not parse is text, so it gets no chip.
  const opWords = cq.only ? [] : typed.split(/\s+/).filter((w) => /^(project|after|status):\S+$/i.test(w) && hasOps(parseOps(w)));
  const offers = (c: Command) => c.id.startsWith("start:") || c.id.startsWith("dir:");
  const onlyStart = hits.length > 0 && hits.every(offers) && !isPathQuery(typed) && searching !== "loading";
  const cut = typed.length > 40 ? typed.slice(0, 40) + "…" : typed;
  const aimed = Boolean(startIn) && mode !== "switch";
  const sel = hits[at];
  const enter = !sel ? "Open" : offers(sel) ? "Start" : isResult(sel) ? "Open" : "Run";
  const insert = (token: string) => { setQ(token + " "); setAtId(null); field.current?.focus(); };
  const noMatch = (line2: React.ReactNode, cls = "") => (
    <div className={"pal-none" + cls} role="status">
      <p className="pal-none-1">No results for “{cut}”</p>
      <p className="pal-none-2">{line2}</p>
    </div>
  );
  let lastGroup = "";
  let groupId = "";
  return createPortal(
    <div ref={box} style={{ display: "contents" }}>
      <div className="pal-scrim" onClick={close} />
      <div className="pal" role="dialog" aria-modal="true" aria-label="Quick access">
        <div className="pal-top">
          <svg className="pal-glass" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" aria-hidden="true">
            <circle cx="11" cy="11" r="6.5" /><path d="M16 16l4.5 4.5" />
          </svg>
          <input ref={field} className={"pal-field" + (mode === "switch" || aimed ? " pal-field-tag" : "")} value={q} onChange={(e) => { setQ(e.target.value); setAtId(null); }}
                 onKeyDown={keys} placeholder={mode === "switch" ? "Switch to a session…" : mode === "new" ? "Start in a folder, or type a first message…" : "Search sessions or run a command…"}
                 aria-label="Search sessions or run a command"
                 role="combobox" aria-expanded={hits.length > 0} aria-controls="pal-list" aria-autocomplete="list"
                 aria-activedescendant={hits[at] ? "pal-" + hits[at].id : undefined}
                 aria-describedby={aimed ? "pal-aim" : undefined} />
          {mode === "switch" && <span className="pal-mode" aria-hidden="true">Sessions</span>}
          {/* Picking a folder clears what was typed: without this nothing
              on screen said the palette now starts there. */}
          {aimed && <span id="pal-aim" className="pal-mode pal-aim" title={`Starts in ${startIn}`}>in {startIn}</span>}
        </div>
        {opWords.length > 0 && (
          <div className="pal-ops" aria-label="Filters">
            {opWords.map((w) => { const i = w.indexOf(":"); return <code key={w} className="pal-op"><span className="pal-op-k">{w.slice(0, i + 1)}</span><span className="pal-op-v">{w.slice(i + 1)}</span></code>; })}
          </div>
        )}
        {/* The listbox holds only its options: the status lines and the tip
            beside them made it a listbox with children it may not own. */}
        <div ref={list} className="pal-list">
          {searching === "error" && (
            <div className="pal-none" role="status">
              <p className="pal-none-1">
                <svg className="pal-warn" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M12 4.5L21 19H3z" /><path d="M12 10v4" /><path d="M12 16.5h.01" /></svg>
                Text search failed
              </p>
              <p className="pal-none-2">Showing title matches only. <button className="btn btn-ghost btn-sm" onClick={() => { retry(); field.current?.focus(); }}>Retry</button></p>
            </div>
          )}
          {onlyStart && searching !== "error" && noMatch(<>Press <kbd>↵</kbd> to start a session with it.</>, " pal-none-lead")}
          <div id="pal-list" role="listbox" aria-label="Results">
          {hits.map((c, i) => {
            const head = c.group !== lastGroup ? (lastGroup = c.group) : "";
            if (head) groupId = "palg-" + c.id;
            const n = head && isResult(c) ? hits.filter((x) => x.group === head).length : 0;
            const dir = c.id.startsWith("dir:") && c.hint !== "Folder not found";
            return (
              <div key={c.id} role="presentation">
                {head && <p className="pal-group eyebrow" role="presentation" id={groupId}>{head}{n >= 4 && <span className="pal-group-n num">{n}</span>}</p>}
                <button id={"pal-" + c.id} role="option" aria-selected={i === at} tabIndex={-1} aria-describedby={groupId}
                        data-at={i === at ? 1 : 0}
                        className={"pal-item" + (i === at ? " pal-on" : "") + (c.destructive ? " pal-bad-item" : "")}
                        onMouseEnter={() => setAtId(c.id)} onClick={() => pick(c)}>
                  {c.mark && <span className="pal-glyph"><StatusMark status={c.mark} size={14} bare /></span>}
                  <span className="pal-label"><Marked text={c.label} terms={terms} /></span>
                  {c.hint && (dir
                    ? <span className="pal-hint pal-path"><bdi>{c.hint}</bdi></span>
                    : <span className="pal-hint">{c.hint.split(" · ").map((part, k) => <span key={k}>{k > 0 && <span className="pal-sep"> · </span>}{part === "Tests failed" ? <span className="pal-bad">{part}</span> : part}</span>)}</span>)}
                  {i === at && <kbd className="pal-enter" aria-hidden="true">↵</kbd>}
                </button>
                {i === at && c.detail && <span className={"pal-ev" + (/^(\$|bash|tool|\/|~)/.test(c.detail) ? " pal-ev-code" : "")}><Marked text={c.detail} terms={terms} /></span>}
              </div>
            );
          })}
          </div>
          {hits.length === 0 && searching === "loading" && (
            <p className="pal-none pal-none-2" role="status"><StatusMark status="running" size={12} bare /> Searching…</p>
          )}
          {hits.length === 0 && searching !== "loading" && searching !== "error" && typed && (
            noMatch(hasOps(ops) ? "Check the filter: project, after and status." : "Try fewer words, or search a phrase from the conversation.")
          )}
          {!typed && mode !== "new" && (
            <p className="pal-tip">Filter with
              {[`project:${syntaxProject(rows) ?? "name"}`, "after:7d", "status:failed"].map((t) => (
                <button key={t} className="pal-op" tabIndex={-1} onMouseDown={(e) => e.preventDefault()} onClick={() => insert(t)}>
                  <span className="pal-op-k">{t.slice(0, t.indexOf(":") + 1)}</span><span className="pal-op-v">{t.slice(t.indexOf(":") + 1)}</span>
                </button>
              ))}
            </p>
          )}
        </div>
        <div className="pal-foot">
          <span className="pal-k"><kbd>↑</kbd><kbd>↓</kbd> Navigate</span>
          <span className="pal-k"><kbd>↵</kbd> {enter}</span>
          {sel?.id.startsWith("dir:") && <span className="pal-k"><kbd>Tab</kbd> Complete</span>}
          <span className="pal-k"><kbd>Esc</kbd> Close</span>
          <span className="pal-count num" role="status">
            {searching === "loading" ? <><StatusMark status="running" size={12} bare /> Searching…</>
              : hits.some(isResult) ? `${shownCount(hits)} ${shownCount(hits) === 1 ? "result" : "results"}` : ""}
          </span>
        </div>
      </div>
    </div>
  , document.body);
}

/** A session's meta, one shape everywhere in the palette: repo · Status · age, and the id tail when the name alone does not tell it apart. */
function meta(r: Row, repo: string | undefined, withId: boolean): string {
  // The glyph carries the status; only a test failure stays a word.
  const status = r.testsFailed ? "Tests failed" : "";
  return [repo?.split("/").pop(), status, r.lastAt ? `${ago(r.lastAt)} ago` : "", withId ? idTail(r.id) : ""].filter(Boolean).join(" · ");
}

/** The last six of an id, without a leading "-" from a legacy "<time>-<pid>" id. */
export const idTail = (id: string) => id.slice(-6).replace(/^-/, "");

/** One matching line, short enough to sit on a row, cut around the match. */
function trimLine(text: string, needle = ""): string {
  const t = text.replace(/\s+/g, " ").trim();
  if (t.length <= 90) return t;
  const i = needle ? t.toLowerCase().indexOf(needle) : -1;
  const from = i < 0 ? 0 : Math.max(0, Math.min(i - 40, t.length - 90));
  return (from > 0 ? "…" : "") + t.slice(from, from + 90) + (from + 90 < t.length ? "…" : "");
}

/** The first matched line that says something the title does not. */
function evidence(h: SearchHit | undefined, title: string, needle: string): string {
  // Titles drop markdown and punctuation; compare letters and digits only.
  const norm = (x: string) => x.toLowerCase().replace(/[^\p{L}\p{N}]+/gu, "");
  const t = norm(title);
  const line = h?.lines.find((l) => { const n = norm(l.text); return n && !n.startsWith(t) && !t.startsWith(n); });
  return line ? trimLine(line.text, needle) : "";
}

/** ⌘K on a Mac, Ctrl+K elsewhere; Ctrl+K in a Mac text field stays kill-line. */
export function usePaletteKey(onOpen: () => void) {
  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      if (paletteKeyOpens(e, isMac())) {
        e.preventDefault();
        onOpen();
      }
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, [onOpen]);
}
