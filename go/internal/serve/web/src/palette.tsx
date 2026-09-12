import { useEffect, useMemo, useRef, useState } from "react";
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
}

/** Case-insensitive subsequence: "fmd" finds "Fix FMDS deploy". */
function score(text: string, q: string): number {
  if (!q) return 0;
  const t = text.toLowerCase();
  const i = t.indexOf(q);
  if (i === 0) return 1000;       // prefix
  if (i > 0) return 500 - i;      // substring, earlier is better
  let at = 0;
  for (const ch of q) {
    at = t.indexOf(ch, at) + 1;
    if (at === 0) return -1;      // not a subsequence either
  }
  return 100;
}

export function Palette({ open, onClose, rows, commands, onOpenSession }: {
  open: boolean;
  onClose: () => void;
  rows: Row[];
  commands: Command[];
  onOpenSession: (id: string) => void;
}) {
  const [q, setQ] = useState("");
  const [at, setAt] = useState(0);
  const field = useRef<HTMLInputElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const opener = useRef<Element | null>(null);

  useEffect(() => {
    if (!open) return;
    opener.current = document.activeElement;
    setQ("");
    setAt(0);
    field.current?.focus();
  }, [open]);

  const hits = useMemo(() => {
    const needle = q.trim().toLowerCase();
    const cmds = commands
      .map((c) => ({ c, s: needle ? score(c.label, needle) : 10 }))
      .filter((x) => x.s >= 0);
    const sessions = rows
      .map((r) => {
        const title = plainTitle(r.title) || "Untitled session";
        return { r, title, s: needle ? Math.max(score(title, needle), score(r.repo ?? "", needle)) : 0 };
      })
      // With no query, commands only: a list of 150 titles is the
      // sidebar again, and the palette is for aiming at one.
      .filter((x) => (needle ? x.s >= 0 : false));
    const all: Command[] = [
      ...cmds.sort((a, b) => b.s - a.s).map((x) => x.c),
      ...sessions.sort((a, b) => b.s - a.s).slice(0, 30).map(({ r, title }) => ({
        id: "s:" + r.id,
        label: title,
        hint: [r.repo?.split("/").pop(), r.status].filter(Boolean).join(" · "),
        group: "Conversations",
        run: () => onOpenSession(r.id),
      })),
    ];
    return all.slice(0, 40);
  }, [q, rows, commands, onOpenSession]);

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
  return (
    <>
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
              </div>
            );
          })}
          {hits.length === 0 && (
            <p className="pal-none">
              {q.trim() ? `Nothing matches “${q.trim()}”.` : "Type to search your conversations."}
            </p>
          )}
        </div>
      </div>
    </>
  );
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
