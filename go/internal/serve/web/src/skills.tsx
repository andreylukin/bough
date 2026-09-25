import { useEffect, useMemo, useRef, useState } from "react";
import { rankSkills, useSkills } from "./mention";
import { Spinner } from "./loading";

// A skill runs by being the first thing in a message ("/grill-me the
// design"), so this picker does not execute anything: it puts the
// slash command in the composer and gets out of the way. That keeps
// one path to running a skill instead of a second one to keep in step.

export interface Skill { name: string; summary: string; manual: boolean }

export function SkillPicker({ session, onPick, disabled = false }: { session: string; onPick: (name: string, known: string[]) => void; disabled?: boolean }) {
  const [open, setOpen] = useState(false);
  const { all, error, retry } = useSkills(open, session);
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

  return <SkillPickerView open={open} disabled={disabled} hits={hits} active={active} error={error} all={all} slow={slow} q={q}
    trigger={trigger} field={field} listbox={listbox} onToggle={() => setOpen((v) => !v)} onClose={close}
    onLeave={() => { setOpen(false); setQ(""); }} onQ={setQ} onKeys={keys} onHover={setAt} onPick={pick}
    onRetry={() => { retry(); field.current?.focus(); }} />;
}

/** The Skills button and, open, its dialog as its state says: a status line (loading, failed, empty) or the skills. */
export function SkillPickerView({ open, disabled, hits, active, error, all, slow, q, trigger, field, listbox, onToggle, onClose, onLeave, onQ, onKeys, onHover, onPick, onRetry }: {
  open: boolean; disabled: boolean; hits: { name: string; summary: string }[]; active: number; error: boolean;
  all: unknown[] | null;
  /** Past the 200 ms a fast read takes: only then does loading say so. */
  slow: boolean; q: string;
  trigger?: React.Ref<HTMLButtonElement>; field?: React.Ref<HTMLInputElement>; listbox?: React.Ref<HTMLDivElement>;
  onToggle: () => void; onClose: () => void; onLeave: () => void; onQ: (q: string) => void; onKeys: (e: React.KeyboardEvent) => void;
  onHover: (i: number) => void; onPick: (s: { name: string }) => void; onRetry: () => void;
}) {
  return (
    <div className="skills-anchor">
      <button ref={trigger} className="btn skills-btn" aria-expanded={open} aria-haspopup="dialog" disabled={disabled} aria-label="Skills"
        title={disabled ? "Transcript didn’t load" : undefined} onClick={onToggle}><span className="skills-glyph" aria-hidden="true">/</span><span className="skills-word">Skills</span></button>

      {open && (
        <>
          <div className="skills-scrim" onClick={onClose} />
          {/* Tabbing out closes it where focus went; only Escape returns to the button. */}
          <div className="skills-pop" role="dialog" aria-label="Insert a skill"
            onBlur={(e) => { if (!e.currentTarget.contains(e.relatedTarget as Node | null) && e.relatedTarget) onLeave(); }}>
            <input ref={field} className="skills-filter" value={q} placeholder="Filter skills"
              aria-label="Filter skills" aria-controls="skill-list" role="combobox" aria-expanded="true"
              aria-activedescendant={active >= 0 ? "skill-" + active : undefined}
              onChange={(e) => onQ(e.target.value)} onKeyDown={onKeys} />
            {/* A listbox only once there are options: loading, failed or empty, it holds a status line, which a listbox may not. */}
            <div id="skill-list" ref={listbox} className="skills-list" role={hits.length ? "listbox" : undefined} aria-label={hits.length ? "Skills" : undefined}>
              {hits.map((s, i) => (
                <button key={s.name} id={"skill-" + i} role="option" aria-selected={i === active} data-at={i === active ? 1 : 0}
                  tabIndex={-1} className={"skill" + (i === active ? " skill-on" : "")}
                  onMouseEnter={() => onHover(i)} onClick={() => onPick(s)}>
                  <span className="mono skill-name">/{s.name}</span>
                  {s.summary && <span className="skill-sum">{s.summary}</span>}
                </button>
              ))}
              {/* Retry goes with the line it is on: focus returns to the filter, or it fell out of the dialog. */}
              {hits.length === 0 && (
                <p className="skills-empty" role="status">
                  {error ? <>Couldn’t load skills. <button className="link" onClick={onRetry}>Retry</button></>
                    : !all ? (slow ? <><Spinner /> Loading skills…</> : "")
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
