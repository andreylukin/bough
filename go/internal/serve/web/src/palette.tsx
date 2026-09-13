import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useModal } from "./dialog";
import type { Row } from "./types";
import { plainTitle } from "./render";

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
}

interface WikiHit { path: string; title: string; topic: string; excerpt: string; counts: { cited: number } }

/** Pages whose claims mention the query, from the wiki's own search. */
function useWikiHits(q: string, open: boolean, on: boolean): WikiHit[] {
  const [hits, setHits] = useState<WikiHit[]>([]);
  useEffect(() => {
    const needle = q.trim();
    if (!on || !open || needle.length < 2 || isSentence(needle)) { setHits([]); return; }
    let live = true;
    const t = setTimeout(() => {
      fetch("/api/wiki/search?q=" + encodeURIComponent(needle))
        .then((r) => r.json())
        .then((d) => { if (live) setHits(d.hits ?? []); })
        .catch(() => { if (live) setHits([]); });
    }, 180);
    return () => { live = false; clearTimeout(t); };
  }, [q, open, on]);
  return hits;
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
type SearchState = "idle" | "loading" | "error";

function useFullText(q: string, open: boolean): { hits: SearchHit[]; state: SearchState } {
  const [hits, setHits] = useState<SearchHit[]>([]);
  const [state, setState] = useState<SearchState>("idle");
  useEffect(() => {
    const needle = q.trim();
    // A sentence is not a search. Every term has to appear somewhere in
    // a session, and "to" and "the" appear in all of them — so asking
    // for one matched everything and buried the thing you were plainly
    // trying to do, which was start it.
    if (!open || needle.length < 2 || isSentence(needle)) { setHits([]); setState("idle"); return; }
    setState("loading");
    // Debounced: this reads every transcript, and the box is typed into
    // one character at a time.
    let live = true;
    const t = setTimeout(() => {
      fetch("/api/search?q=" + encodeURIComponent(needle))
        .then((r) => { if (!r.ok) throw new Error(String(r.status)); return r.json(); })
        .then((d) => { if (live) { setHits(d.hits ?? []); setState("idle"); } })
        // A failed search is not "no matches": say so.
        .catch(() => { if (live) { setHits([]); setState("error"); } });
    }, 180);
    return () => { live = false; clearTimeout(t); };
  }, [q, open]);
  return { hits, state };
}

/** Four words or more reads as something to say, not something to find. */
function isSentence(q: string): boolean {
  return q.trim().split(/\s+/).length >= 4;
}

export function Palette({ open, onClose, rows, commands, onOpenSession, onStart, onOpenWikiPage, initialQuery = "" }: {
  open: boolean;
  onClose: () => void;
  rows: Row[];
  commands: Command[];
  onOpenSession: (id: string) => void;
  /** Start a conversation with what was typed as its first message. */
  onStart?: (text: string) => void;
  /** Open a wiki page; when set, the wiki's pages are searched too. */
  onOpenWikiPage?: (path: string) => void;
  /** Seeds the box. Only a story uses it: nothing can type for us there. */
  initialQuery?: string;
}) {
  const [q, setQ] = useState(initialQuery);
  const [at, setAt] = useState(0);
  const field = useRef<HTMLInputElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const opener = useRef<Element | null>(null);
  const box = useRef<HTMLDivElement>(null);
  useModal(box, open);

  useEffect(() => {
    if (!open) return;
    opener.current = document.activeElement;
    setQ(initialQuery);
    setAt(0);
    field.current?.focus();
  }, [open, initialQuery]);

  const { hits: found, state: searching } = useFullText(q, open);
  const pages = useWikiHits(q, open, Boolean(onOpenWikiPage));

  const hits = useMemo(() => {
    const needle = q.trim().toLowerCase();
    const cmds = commands
      .map((c) => ({ c, s: needle ? score(c.label, needle, true) : 10 }))
      .filter((x) => x.s >= 0);
    const sessions = rows
      .map((r) => {
        const title = plainTitle(r.title) || "Untitled session";
        return { r, title, s: needle ? Math.max(score(title, needle, false), score(r.repo ?? "", needle, false)) : 0 };
      })
      // With no query, commands only: a list of 150 titles is the
      // sidebar again, and the palette is for aiming at one.
      .filter((x) => (needle ? x.s >= 0 : false));
    cmds.sort((a, b) => b.s - a.s);
    sessions.sort((a, b) => b.s - a.s);
    const cmdList = cmds.map((x) => x.c);
    const sessionList: Command[] = sessions.slice(0, 30).map(({ r, title }) => ({
        id: "s:" + r.id,
        label: title,
        hint: [r.repo?.split("/").pop(), r.status].filter(Boolean).join(" · "),
        group: "Conversations",
        run: () => onOpenSession(r.id),
      }));
    // Whichever block holds the better match leads: a title containing
    // "test" beats a command whose letters merely occur in order.
    const all: Command[] = (sessions[0]?.s ?? -1) > (cmds[0]?.s ?? -1)
      ? [...sessionList, ...cmdList]
      : [...cmdList, ...sessionList];
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
    // Full-text results for sessions the title match already found are
    // not new information; the rest come after, each showing the line
    // that matched so you can tell why it is here.
    const seen = new Set(all.map((c) => c.id));
    for (const h of found) {
      if (seen.has("s:" + h.id)) continue;
      const line = h.lines[0];
      all.push({
        id: "s:" + h.id,
        label: plainTitle(h.title) || "Untitled session",
        hint: line ? trimLine(line.text) : `${h.hits} matches`,
        group: "Found in the conversation",
        run: () => onOpenSession(h.id),
      });
    }
    // Last, never first: what you typed is usually the name of
    // something that exists. It is offered once nothing obvious
    // matched, or at the end when something did.
    const typed = q.trim();
    if (onStart && typed.length >= 2 && !typed.includes(":")) {
      const startHere: Command = {
        id: "start:" + typed,
        label: `Start a conversation: “${typed}”`,
        hint: "sends it as the first message",
        group: "Start",
        run: () => onStart(typed),
      };
      // A sentence leads; a word or two follows whatever it matched,
      // because a short query is usually the name of something that
      // already exists.
      if (isSentence(typed)) all.unshift(startHere);
      else all.push(startHere);
    }
    return all.slice(0, 40);
  }, [q, rows, commands, onOpenSession, found, onStart, pages, onOpenWikiPage]);

  useEffect(() => { setAt(0); }, [q]);
  useEffect(() => {
    list.current?.querySelector('[data-at="1"]')?.scrollIntoView({ block: "nearest" });
  }, [at, hits.length]);

  if (!open) return null;

  const close = () => {
    onClose();
    (opener.current as HTMLElement | null)?.focus?.();
  };
  const pick = (c: Command) => { onClose(); c.run(); };

  const keys = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") { e.preventDefault(); close(); return; }
    if (e.key === "ArrowDown") { e.preventDefault(); setAt((i) => Math.min(i + 1, hits.length - 1)); return; }
    if (e.key === "ArrowUp") { e.preventDefault(); setAt((i) => Math.max(i - 1, 0)); return; }
    if (e.key === "Enter") { e.preventDefault(); if (hits[at]) pick(hits[at]); }
  };

  let lastGroup = "";
  return createPortal(
    <div ref={box} style={{ display: "contents" }}>
      <div className="pal-scrim" onClick={close} />
      <div className="pal" role="dialog" aria-modal="true" aria-label="Quick access">
        <input ref={field} className="pal-field" value={q} onChange={(e) => setQ(e.target.value)}
               onKeyDown={keys} placeholder="Find a conversation, or type a command"
               aria-label="Find a conversation, or type a command"
               role="combobox" aria-expanded aria-controls="pal-list"
               aria-activedescendant={hits[at] ? "pal-" + hits[at].id : undefined} />
        <div id="pal-list" ref={list} className="pal-list" role="listbox" aria-label="Results">
          {hits.map((c, i) => {
            const head = c.group !== lastGroup ? (lastGroup = c.group) : "";
            return (
              <div key={c.id}>
                {head && <p className="pal-group">{head}</p>}
                <button id={"pal-" + c.id} role="option" aria-selected={i === at}
                        data-at={i === at ? 1 : 0}
                        className={"pal-item" + (i === at ? " pal-on" : "")}
                        onMouseEnter={() => setAt(i)} onClick={() => pick(c)}>
                  <span className="pal-label">{c.label}</span>
                  {c.hint && <span className="pal-hint">{c.hint}</span>}
                </button>
                {i === at && c.detail && <span className="pal-ev">{c.detail}</span>}
              </div>
            );
          })}
          {searching === "error" && <p className="pal-none">Searching conversation text failed; only titles are matched.</p>}
          {searching === "loading" && hits.length === 0 && <p className="pal-none">Searching conversation text…</p>}
          {hits.length === 0 && searching === "idle" && (
            <p className="pal-none">
              {q.trim() ? `Nothing matches “${q.trim()}”.` : "Type to search your conversations."}
            </p>
          )}
        </div>
      </div>
    </div>
  , document.body);
}

/** One matching line, short enough to sit on a row. */
function trimLine(text: string): string {
  const t = text.replace(/\s+/g, " ").trim();
  return t.length > 90 ? t.slice(0, 90) + "…" : t;
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
