import { useEffect, useMemo, useRef, useState } from "react";

// A skill runs by being the first thing in a message ("/grill-me the
// design"), so this picker does not execute anything: it puts the
// slash command in the composer and gets out of the way. That keeps
// one path to running a skill instead of a second one to keep in step.

export interface Skill { name: string; summary: string; manual: boolean }

export function SkillPicker({ onPick }: { onPick: (name: string) => void }) {
  const [open, setOpen] = useState(false);
  const [all, setAll] = useState<Skill[]>([]);
  const [q, setQ] = useState("");
  const [at, setAt] = useState(0);
  const trigger = useRef<HTMLButtonElement>(null);
  const field = useRef<HTMLInputElement>(null);
  const listbox = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open || all.length) return;
    fetch("/api/skills").then((r) => r.json())
      .then((d) => setAll(d.skills ?? [])).catch(() => setAll([]));
  }, [open, all.length]);

  useEffect(() => { if (open) field.current?.focus(); }, [open]);

  const hits = useMemo(() => {
    const t = q.trim().toLowerCase();
    if (!t) return all;
    return all.filter((s) => s.name.toLowerCase().includes(t) || s.summary.toLowerCase().includes(t));
  }, [all, q]);

  useEffect(() => { setAt(0); }, [q]);

  // Keep the active row in view as the arrows walk past the fold.
  useEffect(() => {
    listbox.current?.querySelector('[data-at="1"]')?.scrollIntoView({ block: "nearest" });
  }, [at, hits.length]);

  const close = () => { setOpen(false); setQ(""); trigger.current?.focus(); };

  const pick = (s: Skill) => { onPick(s.name); setOpen(false); setQ(""); };

  const keys = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") { e.preventDefault(); close(); return; }
    if (e.key === "ArrowDown") { e.preventDefault(); setAt((i) => Math.min(i + 1, hits.length - 1)); return; }
    if (e.key === "ArrowUp") { e.preventDefault(); setAt((i) => Math.max(i - 1, 0)); return; }
    if (e.key === "Enter") { e.preventDefault(); if (hits[at]) pick(hits[at]); }
  };

  return (
    <div className="skills-anchor">
      <button ref={trigger} className="btn" aria-expanded={open} aria-haspopup="dialog"
        onClick={() => setOpen((v) => !v)}>Skills</button>

      {open && (
        <>
          <div className="skills-scrim" onClick={close} />
          <div className="skills-pop" role="dialog" aria-label="Insert a skill">
            <input ref={field} className="skills-filter" value={q} placeholder="Filter skills"
              aria-label="Filter skills" aria-controls="skill-list" onChange={(e) => setQ(e.target.value)}
              onKeyDown={keys} />
            <div id="skill-list" ref={listbox} className="skills-list" role="listbox" aria-label="Skills">
              {hits.map((s, i) => (
                <button key={s.name} role="option" aria-selected={i === at} data-at={i === at ? 1 : 0}
                  className={"skill" + (i === at ? " skill-on" : "")}
                  onMouseEnter={() => setAt(i)} onClick={() => pick(s)}>
                  <span className="mono skill-name">/{s.name}</span>
                  {s.summary && <span className="skill-sum">{s.summary}</span>}
                </button>
              ))}
              {hits.length === 0 && (
                <p className="skills-empty">
                  {all.length === 0
                    ? "No skills found. Add one as a SKILL.md folder under ~/.claude/skills."
                    : `No skills match “${q.trim()}”.`}
                </p>
              )}
            </div>
          </div>
        </>
      )}
    </div>
  );
}
