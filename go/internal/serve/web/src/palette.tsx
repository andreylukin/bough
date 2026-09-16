import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useModal } from "./dialog";
import type { Row } from "./types";
import { hasOwnTitle, plainTitle, sessionTitle, titleKey } from "./render";
import { shownStatus, statusWord } from "./status";

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

/** The typed folder and the folders its path completes to, from the server. */
function useDirs(q: string, open: boolean, on: boolean): { folder: DirHit | null; dirs: DirHit[]; home: string } {
  const [res, setRes] = useState<{ folder: DirHit | null; dirs: DirHit[]; home: string }>({ folder: null, dirs: [], home: "" });
  useEffect(() => {
    const typed = q.trim();
    if (!on || !open || !isPathQuery(typed)) { setRes({ folder: null, dirs: [], home: "" }); return; }
    const ctl = new AbortController();
    const t = setTimeout(() => {
      getJSON<{ folder?: DirHit; dirs?: DirHit[] }>("/api/dirs?path=" + encodeURIComponent(typed), ctl.signal)
        .then((d) => {
          const folder = d.folder ?? null;
          // The server expands ~ by prefixing home, so home is what the typed rest does not account for.
          const home = folder && typed.startsWith("~") ? folder.path.slice(0, folder.path.length - (typed.length - 1)) : "";
          setRes({ folder, dirs: Array.isArray(d.dirs) ? d.dirs : [], home });
        })
        .catch(() => { if (!ctl.signal.aborted) setRes({ folder: null, dirs: [], home: "" }); });
    }, 120);
    return () => { ctl.abort(); clearTimeout(t); };
  }, [q, open, on]);
  return res;
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

export function Palette({ open, onClose, rows, commands, onOpenSession, onStart, onStartIn, onOpenWikiPage, initialQuery = "", current = null, startIn }: {
  open: boolean;
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
}) {
  const [q, setQ] = useState(initialQuery);
  // The selection is a result, not a position: results that land late
  // (full-text, wiki) never move Enter onto something else.
  const [atId, setAtId] = useState<string | null>(null);
  const field = useRef<HTMLInputElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const opener = useRef<Element | null>(null);
  const box = useRef<HTMLDivElement>(null);
  useModal(box, open);

  // Focus goes back to the opener only once the palette is gone and the
  // page behind it is no longer inert.
  useEffect(() => {
    if (open) return;
    const el = opener.current as HTMLElement | null;
    opener.current = null;
    if (el?.isConnected && !document.querySelector("[aria-modal='true']")) el.focus?.();
  }, [open]);

  useEffect(() => {
    if (!open) return;
    opener.current = document.activeElement;
    setQ(initialQuery);
    setAtId(null);
    field.current?.focus();
  }, [open, initialQuery]);

  const { hits: found, state: searching, retry } = useFullText(q, open);
  const pages = useWikiHits(q, open, Boolean(onOpenWikiPage));
  const places = useDirs(q, open, Boolean(onStartIn));

  const hits = useMemo(() => {
    const needle = q.trim().toLowerCase();
    // With no query: what you would do next, not the sitemap.
    const cmds = commands
      .filter((c) => needle || c.suggest)
      .map((c) => ({ c, s: needle ? score(c.label, needle, true) : 10 }))
      .filter((x) => x.s >= 0);
    const sessions = rows
      .map((r) => {
        const title = sessionTitle(r);
        const s = needle ? Math.max(score(title, needle, false), score(r.repo ?? "", needle, false)) : 0;
        // The one you are in is rarely where you want to go.
        return { r, title, s: s >= 0 && r.id === current ? Math.min(s, 51) : s };
      })
      // Best first, then the cap: slicing first dropped strong matches.
      .filter((x) => needle && x.s >= 0)
      .sort((a, b) => b.s - a.s);
    // With no query, the three sessions you touched last (not this one):
    // a list of 150 titles is the sidebar again.
    const recent = needle ? [] : rows
      .filter((r) => !r.archived && !r.empty && r.id !== current)
      .sort((a, b) => Date.parse(b.lastAt) - Date.parse(a.lastAt))
      .slice(0, 3);
    // Same-named sessions are told apart under the active row: branch,
    // age, and the line the full-text search matched, when it did.
    const foundBy = new Map(found.map((h) => [h.id, h]));
    // One candidate per session, whether its title or its text matched:
    // a text-only hit ranks below any title match but inside the same cap.
    const byId = new Map(sessions.map((x) => [x.r.id, x]));
    for (const h of found) {
      const r = rows.find((x) => x.id === h.id);
      if (!r || byId.has(h.id)) continue;
      byId.set(h.id, { r, title: plainTitle(r.title) || sessionTitle({ id: h.id, title: h.title }), s: h.id === current ? 49 : 50 });
    }
    const cands = [...byId.values()].sort((a, b) => b.s - a.s);
    // Titles that repeat carry the id's tail, the one thing always different.
    const twice = new Set<string>();
    const seenTitle = new Set<string>();
    const same = titleKey;
    for (const { title } of cands) (seenTitle.has(same(title)) ? twice : seenTitle).add(same(title));
    const detailFor = (r: Row, title: string) => {
      const h = foundBy.get(r.id);
      const line = evidence(h, title, needle);
      return [[r.branch || h?.branch, r.lastAt ? agoShort(r.lastAt) : ""].filter(Boolean).join(" · "), line]
        .filter(Boolean).join("\n") || undefined;
    };
    // One relevance order across commands and titles, so a weak match
    // in one kind never leapfrogs a strong one in the other.
    const ranked = [
      ...recent.map((r) => ({ s: 20, c: {
        id: "s:" + r.id,
        label: sessionTitle(r),
        hint: meta(r, r.repo, !hasOwnTitle(r)),
        group: "Recent sessions",
        run: () => onOpenSession(r.id),
      } as Command })),
      ...cmds.map((x) => ({ s: x.s, c: x.c })),
      ...cands.map(({ r, title, s }) => ({ s, c: {
        id: "s:" + r.id,
        label: title,
        hint: (r.id === current ? "Current · " : "") + meta(r, r.repo || foundBy.get(r.id)?.repo, twice.has(same(title)) || !hasOwnTitle(r)),
        group: s <= 50 ? "Found in the conversation" : "Sessions",
        detail: detailFor(r, title),
        run: () => onOpenSession(r.id),
      } as Command })),
    ].sort((a, b) => b.s - a.s);
    const all: Command[] = ranked.map((x) => x.c);
    // A wiki page carries the claim that matched: the point of the wiki
    // is the join between a page and the entry behind it, and a title
    // alone does not show which is which.
    if (onOpenWikiPage) {
      for (const p of pages) {
        all.push({
          id: "w:" + p.path,
          label: p.title,
          hint: [p.topic, p.counts.cited ? `${p.counts.cited} cited` : ""].filter(Boolean).join(" · "),
          group: "Wiki pages",
          detail: p.excerpt,
          run: () => onOpenWikiPage(p.path),
        });
      }
    }
    // A text hit on a session the list does not hold (archived, hidden)
    // still opens, showing the line that matched.
    const seen = new Set(all.map((c) => c.id));
    for (const h of found) {
      if (seen.has("s:" + h.id)) continue;
      const label = sessionTitle({ id: h.id, title: h.title });
      all.push({
        id: "s:" + h.id,
        label,
        hint: [h.repo?.split("/").pop(), h.id.slice(-6)].filter(Boolean).join(" · "),
        group: "Found in the conversation",
        detail: [h.branch, evidence(h, label, needle)].filter(Boolean).join("\n") || undefined,
        run: () => onOpenSession(h.id, h.lines[0]?.seq, q.trim()),
      });
    }
    // Capped first; Start is added after, so a long result list never hides it.
    const capped = all.slice(0, 40);
    // A typed path is a place to start, ahead of everything else: it is
    // never a first message, which is what a pasted ~/repos/x used to become.
    const typed = q.trim();
    if (onStartIn && isPathQuery(typed)) {
      const short = (p: string) => places.home && p.startsWith(places.home) ? "~" + p.slice(places.home.length) : p;
      const can = (d: DirHit) => d.checkout ? `can edit ${short(d.checkout)}` : "read-only";
      const folders = [...(places.folder?.exists ? [places.folder] : []), ...places.dirs];
      capped.unshift(...folders.map((d, i): Command => ({
        id: "dir:" + d.path,
        label: `New session in ${short(d.path).replace(/(.)\/+$/, "$1")}`,
        hint: i === 0 && places.folder?.exists ? can(d) : `${can(d)} · tab to complete`,
        group: "Start",
        // Nothing is created here: the palette stays, aimed at the folder, for the first prompt.
        run: () => { onStartIn(d.path); setQ(""); setAtId(null); },
      })));
    }
    // Last, always explicit: typing never starts anything by itself.
    if (onStart && typed.length >= 2 && !typed.includes(":") && !isPathQuery(typed)) {
      capped.push({
        id: "start:" + typed,
        label: startIn ? `Start a session in ${startIn}: “${typed}”` : `Start a session: “${typed}”`,
        hint: startIn ? `in ${startIn} · sends it as the first message` : "sends it as the first message",
        group: "Start",
        run: () => onStart(typed),
      });
    }
    return capped;
  }, [q, rows, commands, onOpenSession, found, onStart, onStartIn, places, pages, onOpenWikiPage, current, startIn]);

  // Nothing picked yet: the first result that is not destructive, so Enter never archives by default.
  const at = atId === null ? Math.max(0, hits.findIndex((c) => !c.destructive)) : Math.max(0, hits.findIndex((c) => c.id === atId));
  const setAt = (f: (i: number) => number) => { const c = hits[f(at)]; if (c) setAtId(c.id); };
  useEffect(() => {
    list.current?.querySelector('[data-at="1"]')?.scrollIntoView({ block: "nearest" });
  }, [at, hits.length]);

  if (!open) return null;

  const close = () => onClose();
  const pick = (c: Command) => { opener.current = null; onClose(); c.run(); };

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

  let lastGroup = "";
  let groupId = "";
  return createPortal(
    <div ref={box} style={{ display: "contents" }}>
      <div className="pal-scrim" onClick={close} />
      <div className="pal" role="dialog" aria-modal="true" aria-label="Quick access">
        <input ref={field} className="pal-field" value={q} onChange={(e) => { setQ(e.target.value); setAtId(null); }}
               onKeyDown={keys} placeholder="Search sessions or run a command…"
               aria-label="Search sessions or run a command"
               role="combobox" aria-expanded={hits.length > 0} aria-controls="pal-list" aria-autocomplete="list"
               aria-activedescendant={hits[at] ? "pal-" + hits[at].id : undefined} />
        <div id="pal-list" ref={list} className="pal-list" role="listbox" aria-label="Results">
          {hits.map((c, i) => {
            const head = c.group !== lastGroup ? (lastGroup = c.group) : "";
            if (head) groupId = "palg-" + c.id;
            return (
              <div key={c.id} role="presentation">
                {head && <p className="pal-group eyebrow" role="presentation" id={groupId}>{head}</p>}
                <button id={"pal-" + c.id} role="option" aria-selected={i === at} tabIndex={-1} aria-describedby={groupId}
                        data-at={i === at ? 1 : 0}
                        className={"pal-item" + (i === at ? " pal-on" : "")}
                        onMouseEnter={() => setAtId(c.id)} onClick={() => pick(c)}>
                  <span className="pal-label">{c.label}</span>
                  {c.hint && <span className="pal-hint">{c.hint}</span>}
                </button>
                {i === at && c.detail && <span className="pal-ev">{c.detail}</span>}
              </div>
            );
          })}
          {hits.length === 0 && (
            <p className="pal-none" role="status">
              {searching === "loading" ? "Searching…"
                : searching === "error" ? "Search failed. Titles only, and none match."
                : q.trim() ? "No matching sessions or commands." : "Type to search your sessions."}
            </p>
          )}
        </div>
        {/* Never scrolls away: a failed text search is not hidden under the list. */}
        <div className="pal-foot" role="status">
          <span className="num">{hits.length} shown</span>
          {searching === "loading" && <span>Searching text…</span>}
          {searching === "done" && found.length === 0 && <span>No text matches</span>}
          {searching === "error" && <span className="pal-foot-bad">Text search failed · titles only</span>}
          {searching === "error" && <button className="link" onClick={retry}>Retry</button>}
          <span className="pal-foot-keys"><span><kbd>↑↓</kbd> move</span><span><kbd>↵</kbd> open</span><span><kbd>esc</kbd> close</span></span>
        </div>
      </div>
    </div>
  , document.body);
}

/** A session's meta, one shape everywhere in the palette: repo · Status · age, and the id tail when the name alone does not tell it apart. */
function meta(r: Row, repo: string | undefined, withId: boolean): string {
  const status = r.testsFailed ? "Tests failed" : statusWord(shownStatus(r));
  return [repo?.split("/").pop(), status, r.lastAt ? agoShort(r.lastAt) : "", withId ? r.id.slice(-6) : ""].filter(Boolean).join(" · ");
}

function agoShort(iso: string): string {
  const m = Math.max(0, Math.round((Date.now() - new Date(iso).getTime()) / 60000));
  return m < 60 ? `${m}m ago` : m < 2880 ? `${Math.round(m / 60)}h ago` : `${Math.round(m / 1440)}d ago`;
}

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

/** ⌘K on a Mac, Ctrl+K elsewhere, and never inside a text field. */
export function usePaletteKey(onOpen: () => void) {
  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        onOpen();
      }
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, [onOpen]);
}
