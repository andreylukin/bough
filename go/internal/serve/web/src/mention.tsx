import { useEffect, useMemo, useRef, useState } from "react";

/**
 * The composer's / and @ pickers.
 *
 * Both do the same job: you are typing a token, and the thing you mean
 * has a name you half-remember. Matching is a subsequence over the
 * whole candidate, so "aptsx" finds "app/palette.tsx" and "grl" finds
 * "grill-me" — the point is to stop typing early, not to spell it.
 */
export interface Choice { value: string; label: string; hint?: string }

/** What is being typed at the caret, if it is a / or @ token. */
export interface Trigger { kind: "/" | "@"; token: string; from: number; to: number }

/**
 * A trigger is only live at the caret and only when the sigil starts a
 * word — "/" after a word is a path separator, and "@" inside one is
 * an email, so every address would otherwise open a picker.
 *
 * Starting a word is the whole rule. "/" used to have to lead the
 * message as well, on the theory that a skill only runs as the first
 * thing said; but a name is half-remembered wherever you are in a
 * sentence, and refusing to complete it there is just a picker that
 * does not work.
 */
export function triggerAt(text: string, caret: number): Trigger | null {
  for (let i = caret - 1; i >= 0; i--) {
    const ch = text[i];
    if (ch === " " || ch === "\n" || ch === "\t") return null;
    if (ch === "/" || ch === "@") {
      const before = i === 0 ? "" : text[i - 1];
      if (before && !/\s/.test(before)) return null;
      return { kind: ch, token: text.slice(i + 1, caret), from: i, to: caret };
    }
  }
  return null;
}

/** Subsequence rank, or -1. Prefix beats contains beats scattered. */
export function rank(text: string, q: string): number {
  if (!q) return 1;
  const t = text.toLowerCase();
  const i = t.indexOf(q);
  if (i === 0) return 100;
  if (i > 0) return 80;
  let at = 0;
  for (const ch of q) {
    const j = t.indexOf(ch, at);
    if (j < 0) return -1;
    at = j + 1;
  }
  return 20;
}

interface SkillRow { name: string; summary: string }
interface FileRow { path: string; dir: boolean }

/** Skills are a small fixed list, so they load once and match locally. */
function useSkills(on: boolean): Choice[] {
  const [all, setAll] = useState<SkillRow[]>([]);
  useEffect(() => {
    if (!on || all.length) return;
    fetch("/api/skills").then((r) => r.json())
      .then((d) => setAll(d.skills ?? [])).catch(() => setAll([]));
  }, [on, all.length]);
  return useMemo(() => all.map((s) => ({ value: s.name, label: "/" + s.name, hint: s.summary })), [all]);
}

/**
 * Files cannot load once: the directory is a home with a thousand
 * repos in it. The server searches, bounded, and is asked only after
 * you stop typing.
 */
function useFiles(on: boolean, token: string, session: string): Choice[] {
  const [hits, setHits] = useState<FileRow[]>([]);
  useEffect(() => {
    if (!on || token.length < 1) { setHits([]); return; }
    let live = true;
    const t = setTimeout(() => {
      fetch(`/api/files?q=${encodeURIComponent(token)}&session=${encodeURIComponent(session)}`)
        .then((r) => r.json())
        .then((d) => { if (live) setHits(d.files ?? []); })
        .catch(() => { if (live) setHits([]); });
    }, 140);
    return () => { live = false; clearTimeout(t); };
  }, [on, token, session]);
  return useMemo(() => hits.map((f) => ({
    value: f.path, label: f.path, hint: f.dir ? "directory" : "",
  })), [hits]);
}

export function Mentions({ trigger, session, onPick, onClose }: {
  trigger: Trigger | null;
  session: string;
  onPick: (t: Trigger, value: string) => void;
  onClose: () => void;
}) {
  const [at, setAt] = useState(0);
  const box = useRef<HTMLDivElement>(null);
  const skills = useSkills(trigger?.kind === "/");
  const files = useFiles(trigger?.kind === "@", trigger?.token ?? "", session);

  const token = (trigger?.token ?? "").toLowerCase();
  const hits = useMemo(() => {
    if (!trigger) return [];
    // Files are already ranked by the server, which saw the whole tree;
    // re-ranking here would only throw that away.
    if (trigger.kind === "@") return files.slice(0, 20);
    return skills
      .map((c) => ({ c, r: rank(c.value, token) }))
      .filter((x) => x.r >= 0)
      .sort((a, b) => b.r - a.r)
      .map((x) => x.c)
      .slice(0, 20);
  }, [trigger, token, skills, files]);

  useEffect(() => { setAt(0); }, [token, trigger?.kind]);
  useEffect(() => {
    box.current?.querySelector('[data-at="1"]')?.scrollIntoView({ block: "nearest" });
  }, [at, hits.length]);

  // The composer owns the keyboard; it forwards the keys this needs.
  useEffect(() => {
    if (!trigger) return;
    const h = (e: KeyboardEvent) => {
      if (!hits.length) return;
      if (e.key === "ArrowDown") { e.preventDefault(); setAt((i) => Math.min(i + 1, hits.length - 1)); }
      else if (e.key === "ArrowUp") { e.preventDefault(); setAt((i) => Math.max(i - 1, 0)); }
      else if (e.key === "Enter" || e.key === "Tab") { e.preventDefault(); onPick(trigger, hits[at].value); }
      else if (e.key === "Escape") { e.preventDefault(); onClose(); }
    };
    const el = document.getElementById("composer");
    el?.addEventListener("keydown", h, true);
    return () => el?.removeEventListener("keydown", h, true);
  }, [trigger, hits, at, onPick, onClose]);

  if (!trigger || hits.length === 0) return null;
  return (
    <div className="mention" role="listbox" aria-label={trigger.kind === "/" ? "Skills" : "Files"} ref={box}>
      {hits.map((c, i) => (
        <button key={c.value} role="option" aria-selected={i === at} data-at={i === at ? 1 : 0}
                className={"mention-item" + (i === at ? " mention-on" : "")}
                onMouseEnter={() => setAt(i)} onClick={() => onPick(trigger, c.value)}>
          <span className="mono mention-name">{c.label}</span>
          {c.hint && <span className="mention-hint">{c.hint}</span>}
        </button>
      ))}
    </div>
  );
}
