import { useEffect, useMemo, useRef, useState } from "react";
import { Spinner } from "./loading";

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
      // Mid-word "/" is a path separator: keep walking, so "@go/internal/x"
      // is still the @ token it started as.
      if (before && !/\s/.test(before)) { if (ch === "/") continue; return null; }
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

/**
 * Skills are a small fixed list, so they load once and match locally.
 * One catalogue serves the / picker and the Skills button alike, and a
 * failed read is kept as a failure, not shown as "none installed".
 */
let catalogue: { session: string; p: Promise<SkillRow[]> } | null = null;
export function useSkills(on: boolean, session: string) {
  const [all, setAll] = useState<SkillRow[] | null>(null);
  const [error, setError] = useState(false);
  const [tries, setTries] = useState(0);
  useEffect(() => {
    if (!on) return;
    let live = true;
    // The session's skills: serve's own cwd is HOME, so without it the
    // repo's .claude/skills were missing and a switched-off skill was
    // offered. Another session's catalogue is not this one's.
    if (catalogue?.session !== session) {
      // Bounded: a read that never answers is shown as a failure with Retry, not "Loading" forever.
      const p: Promise<SkillRow[]> = fetch(`/api/skills?session=${encodeURIComponent(session)}`, { signal: AbortSignal.timeout(15_000) }).then((r) => {
        if (!r.ok) throw new Error(String(r.status));
        return r.json();
      // An empty answer is not cached: skills installed since show on the next open.
      }).then((d) => { const v = d.skills ?? []; if (!v.length && catalogue?.p === p) catalogue = null; return v; });
      catalogue = { session, p };
    }
    const p = catalogue.p;
    setError(false);
    p.then((v) => { if (live) setAll(v); })
      .catch(() => { if (catalogue?.p === p) catalogue = null; if (live) setError(true); });
    return () => { live = false; };
  }, [on, session, tries]);
  return { all, error, retry: () => setTries((n) => n + 1) };
}

/** Skill rows ranked for a query: the one ordering both pickers use. */
export function rankSkills<T extends SkillRow>(all: T[], q: string): T[] {
  const t = q.trim().toLowerCase();
  return all.map((s) => ({ s, r: Math.max(rank(s.name, t), s.summary.toLowerCase().includes(t) ? 10 : -1) }))
    .filter((x) => x.r >= 0).sort((a, b) => b.r - a.r).map((x) => x.s);
}

/**
 * Files cannot load once: the directory is a home with a thousand
 * repos in it. The server searches, bounded, and is asked only after
 * you stop typing.
 */
function useFiles(on: boolean, token: string, session: string) {
  // Results carry the query they answer, so a slow reply to an older
  // token is never offered (or picked) under a newer one.
  const [got, setGot] = useState<{ q: string; rows: FileRow[] } | null>(null);
  const hits = got && got.q === token ? got.rows : null;
  const [error, setError] = useState(false);
  const [tries, setTries] = useState(0);
  useEffect(() => {
    // A bare "@" asks too: the server answers it with the files nearest
    // the top of the project, so the picker opens the moment you type it.
    if (!on) { setGot(null); setError(false); return; }
    let live = true;
    // A retry (or a new token) is a fresh read: "Couldn't load" gives way to loading, not to nothing.
    setError(false);
    const t = setTimeout(() => {
      fetch(`/api/files?q=${encodeURIComponent(token)}&session=${encodeURIComponent(session)}`)
        .then((r) => { if (!r.ok) throw new Error(String(r.status)); return r.json(); })
        .then((d) => { if (live) { setGot({ q: token, rows: d.files ?? [] }); setError(false); } })
        .catch(() => { if (live) { setGot({ q: token, rows: [] }); setError(true); } });
    }, 140);
    return () => { live = false; clearTimeout(t); };
  }, [on, token, session, tries]);
  const files = useMemo<Choice[] | null>(() => hits && hits.map((f) => ({
    value: f.path, label: f.path, hint: f.dir ? "directory" : "",
  })), [hits]);
  // A retry is a new read: drop the failure, or the picker keeps saying
  // "Couldn’t load" while it is asking again.
  return { files, error, retry: () => { setGot(null); setError(false); setTries((n) => n + 1); } };
}

export function Mentions({ trigger, session, onPick, onClose, onOpen, onActive }: {
  trigger: Trigger | null;
  session: string;
  onPick: (t: Trigger, value: string) => void;
  onClose: () => void;
  /** Whether the picker is on screen and owns Enter, so the hint can step aside. */
  onOpen?: (open: boolean) => void;
  /** The highlighted option's id, for the composer's aria-activedescendant. */
  onActive?: (id: string | undefined) => void;
}) {
  const [at, setAt] = useState(0);
  const box = useRef<HTMLDivElement>(null);
  const { all: skills, error: skillsErr, retry: retrySkills } = useSkills(trigger?.kind === "/", session);
  const { files, error: filesErr, retry: retryFiles } = useFiles(trigger?.kind === "@", trigger?.token ?? "", session);

  const token = (trigger?.token ?? "").toLowerCase();
  const hits = useMemo(() => {
    if (!trigger) return [];
    // Files are already ranked by the server, which saw the whole tree;
    // re-ranking here would only throw that away.
    if (trigger.kind === "@") return (files ?? []).slice(0, 20);
    return rankSkills(skills ?? [], token).slice(0, 20)
      .map((s) => ({ value: s.name, label: "/" + s.name, hint: s.summary }));
  }, [trigger, token, skills, files]);

  useEffect(() => { setAt(0); }, [token, trigger?.kind]);
  // Results can shrink under the cursor without the token changing.
  useEffect(() => { setAt((i) => Math.min(i, Math.max(hits.length - 1, 0))); }, [hits.length]);

  useEffect(() => { onActive?.(trigger && hits.length ? "mention-" + at : undefined); }, [trigger, hits.length, at, onActive]);
  // A fast answer shows nothing; only a slow one says it is looking.
  const loaded = trigger?.kind === "@" ? files !== null : skills !== null;
  const [slow, setSlow] = useState(false);
  useEffect(() => {
    if (!trigger || loaded) { setSlow(false); return; }
    const t = setTimeout(() => setSlow(true), 200);
    return () => clearTimeout(t);
  }, [trigger, loaded]);
  useEffect(() => {
    box.current?.querySelector('[data-at="1"]')?.scrollIntoView({ block: "nearest" });
  }, [at, hits.length]);

  // The composer owns the keyboard; it forwards the keys this needs.
  useEffect(() => {
    if (!trigger) return;
    const h = (e: KeyboardEvent) => {
      // Shift+Enter is a newline and Shift+Tab leaves the field, open picker or not.
      const own = ["ArrowDown", "ArrowUp", "Enter", "Tab", "Escape"].includes(e.key) && !(e.shiftKey && e.key !== "Escape");
      if (!own || e.isComposing) return;
      if (!hits.length) {
        // Loading, empty or failed: Escape closes, and Enter never sends
        // a half-typed token while the picker is there.
        if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); onClose(); }
        else if (e.key === "Enter") { e.preventDefault(); e.stopPropagation(); }
        return;
      }
      // Stop the key here. Picking closes the picker synchronously, so by
      // the time the composer's own handler saw this Enter the trigger was
      // already gone and it sent the message: Enter picked AND sent.
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "ArrowDown") setAt((i) => Math.min(i + 1, hits.length - 1));
      else if (e.key === "ArrowUp") setAt((i) => Math.max(i - 1, 0));
      else if (e.key === "Enter" || e.key === "Tab") onPick(trigger, hits[at].value);
      else onClose();
    };
    const el = document.getElementById("composer");
    el?.addEventListener("keydown", h, true);
    return () => el?.removeEventListener("keydown", h, true);
  }, [trigger, hits, at, loaded, onPick, onClose]);

  const isFiles = trigger?.kind === "@";
  const failed = isFiles ? filesErr : skillsErr;
  const panel = Boolean(trigger && (hits.length || failed || loaded || slow));
  useEffect(() => { onOpen?.(panel); }, [panel, onOpen]);
  if (!trigger) return null;
  if (hits.length === 0) {
    // Nothing to pick is still an answer: say which one, never go blank.
    const q = trigger.token ? ` match “${trigger.token}”` : "";
    const msg = failed ? <>Couldn’t load {isFiles ? "files" : "skills"}. <button className="link" onMouseDown={(e) => e.preventDefault()}
        onClick={isFiles ? retryFiles : retrySkills}>Retry</button></>
      : !loaded ? (slow ? <><Spinner /> {isFiles ? "Finding files…" : "Loading skills…"}</> : null)
      : !isFiles && skills?.length === 0 ? "No skills installed"
      : `No ${isFiles ? "files" : "skills"}${q || " found"}`;
    return msg && (
      <div className="mention" ref={box} onKeyDown={(e) => {
        // Escape from Retry closes the picker and hands the keys back.
        if (e.key === "Escape") { e.preventDefault(); onClose(); document.getElementById("composer")?.focus(); }
      }}><p className="mention-state" role="status">{msg}</p></div>
    );
  }
  return (
    <div className="mention" ref={box}>
      <div className="mention-list" id="mention-list" role="listbox" aria-label={isFiles ? "Files" : "Skills"}>
        {hits.map((c, i) => {
          // A path scans by its name, not its directories: the name leads,
          // the folder it lives in follows, dimmed.
          const slash = c.value.lastIndexOf("/");
          const name = isFiles && slash >= 0 ? c.value.slice(slash + 1) : c.label;
          const dir = isFiles && slash >= 0 ? c.value.slice(0, slash) : "";
          const isDir = isFiles && c.hint === "directory";
          return (
            <button key={c.value} id={"mention-" + i} role="option" tabIndex={-1} aria-selected={i === at} data-at={i === at ? 1 : 0}
                    className={"mention-item" + (i === at ? " mention-on" : "")}
                    onMouseDown={(e) => e.preventDefault() /* keep the composer focused */}
                    onMouseEnter={() => setAt(i)} onClick={() => onPick(trigger, c.value)}>
              {isFiles && <MentionGlyph dir={isDir} />}
              <span className={"mention-name" + (isFiles ? "" : " mono")}>{name}{isDir ? "/" : ""}</span>
              {dir && <span className="mono mention-dir">{dir}</span>}
              {!isFiles && c.hint && <span className="mention-hint">{c.hint}</span>}
            </button>
          );
        })}
      </div>
      <div className="mention-foot" aria-hidden="true">
        <span className="ov-key"><span className="keys-combo"><kbd>↑</kbd><kbd>↓</kbd></span> navigate</span>
        <span className="ov-key"><kbd>↵</kbd> insert</span>
        <span className="ov-key"><kbd>Esc</kbd> close</span>
      </div>
    </div>
  );
}

/** File or folder, stroked in currentColor like the status glyphs. */
function MentionGlyph({ dir }: { dir: boolean }) {
  return (
    <svg className="mention-glyph" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
         strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      {dir
        ? <path d="M3 7.5A1.5 1.5 0 0 1 4.5 6h4.2l2 2.2h8.8A1.5 1.5 0 0 1 21 9.7v8.8a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 18.5z" />
        : <><path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z" /><path d="M14 3v5h5" /></>}
    </svg>
  );
}
