import { useEffect, useMemo, useRef, useState } from "react";
import { rankSkills, useSkills } from "./mention";

// A skill runs by being the first thing in a message ("/grill-me the
// design"), so this picker does not execute anything: it puts the
// slash command in the composer and gets out of the way. That keeps
// one path to running a skill instead of a second one to keep in step.

export interface Skill { name: string; summary: string; manual: boolean }

export function SkillPicker({ onPick }: { onPick: (name: string, known: string[]) => void }) {
  const [open, setOpen] = useState(false);
  const { all, error, retry } = useSkills(open);
  const [q, setQ] = useState("");
  const [at, setAt] = useState(0);
  const trigger = useRef<HTMLButtonElement>(null);
  const field = useRef<HTMLInputElement>(null);
  const listbox = useRef<HTMLDivElement>(null);
  // A fast read shows nothing; only a slow one says it is loading.
  const [slow, setSlow] = useState(false);
  useEffect(() => {
    if (!open || all) { setSlow(false); return; }
    const t = setTimeout(() => setSlow(true), 200);
    return () => clearTimeout(t);
  }, [open, all]);

  useEffect(() => { if (open) field.current?.focus(); }, [open]);

  const hits = useMemo(() => rankSkills(all ?? [], q), [all, q]);

  useEffect(() => { setAt(0); }, [q]);
  const active = hits.length ? Math.min(at, hits.length - 1) : -1;

  // Keep the active row in view as the arrows walk past the fold.
  useEffect(() => {
    listbox.current?.querySelector('[data-at="1"]')?.scrollIntoView({ block: "nearest" });
  }, [at, hits.length]);

  const close = () => { setOpen(false); setQ(""); trigger.current?.focus(); };

  const pick = (s: { name: string }) => { onPick(s.name, (all ?? []).map((x) => x.name)); setOpen(false); setQ(""); };

  const keys = (e: React.KeyboardEvent) => {
    if (e.nativeEvent.isComposing) return;
    if (e.key === "Escape") { e.preventDefault(); close(); return; }
    if (e.key === "ArrowDown") { e.preventDefault(); setAt((i) => Math.max(0, Math.min(i + 1, hits.length - 1))); return; }
    if (e.key === "ArrowUp") { e.preventDefault(); setAt((i) => Math.max(i - 1, 0)); return; }
    if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); if (active >= 0) pick(hits[active]); }
  };

  return (
    <div className="skills-anchor">
      <button ref={trigger} className="btn" aria-expanded={open} aria-haspopup="dialog"
        onClick={() => setOpen((v) => !v)}>Skills</button>

      {open && (
        <>
          <div className="skills-scrim" onClick={close} />
          {/* Tabbing out closes it where focus went; only Escape returns to the button. */}
          <div className="skills-pop" role="dialog" aria-label="Insert a skill"
            onBlur={(e) => { if (!e.currentTarget.contains(e.relatedTarget as Node | null) && e.relatedTarget) { setOpen(false); setQ(""); } }}>
            <input ref={field} className="skills-filter" value={q} placeholder="Filter skills"
              aria-label="Filter skills" aria-controls="skill-list" role="combobox" aria-expanded="true"
              aria-activedescendant={active >= 0 ? "skill-" + active : undefined}
              onChange={(e) => setQ(e.target.value)} onKeyDown={keys} />
            <div id="skill-list" ref={listbox} className="skills-list" role="listbox" aria-label="Skills">
              {hits.map((s, i) => (
                <button key={s.name} id={"skill-" + i} role="option" aria-selected={i === active} data-at={i === active ? 1 : 0}
                  tabIndex={-1} className={"skill" + (i === active ? " skill-on" : "")}
                  onMouseEnter={() => setAt(i)} onClick={() => pick(s)}>
                  <span className="mono skill-name">/{s.name}</span>
                  {s.summary && <span className="skill-sum">{s.summary}</span>}
                </button>
              ))}
              {hits.length === 0 && (
                <p className="skills-empty" role="status">
                  {error ? <>Couldn’t load skills. <button className="link" onClick={retry}>Retry</button></>
                    : !all ? (slow ? "Loading skills…" : "")
                    : all.length === 0 ? "No skills installed. Create ~/.claude/skills/<name>/SKILL.md"
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
