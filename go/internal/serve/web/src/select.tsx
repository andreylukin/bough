import { Fragment, useEffect, useMemo, useRef, useState } from "react";

/** One choosable value. Options sharing a `group` sit under one heading. */
export interface Option { value: string; label: string; detail?: string; group?: string }

/**
 * The control room's dropdown.
 *
 * A native <select> draws the operating system's menu: a white sheet
 * of system font on a dark forest page, with no search, no way to show
 * a context size beside a model, and optgroups the browser styles as
 * it likes. This is the same popover the @ picker uses — raised
 * surface, 36px rows, the current value checked — with a search field
 * where the list is long enough to need one.
 *
 * Keyboard: ↓/↑/Enter/Space open it; inside, ↓↑ move, Enter picks,
 * Esc closes and gives focus back to the button.
 */
export function Select({ value, options, onChange, label, placeholder = "Choose", searchable = false, align = "start" }: {
  value: string;
  options: Option[];
  onChange: (value: string) => void;
  /** Names the control for assistive tech; the visible label sits beside it. */
  label: string;
  placeholder?: string;
  searchable?: boolean;
  align?: "start" | "end";
}) {
  const [open, setOpen] = useState(false);
  const [q, setQ] = useState("");
  const [at, setAt] = useState(0);
  const root = useRef<HTMLDivElement>(null);
  const btn = useRef<HTMLButtonElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const current = options.find((o) => o.value === value);

  const shown = useMemo(() => {
    const t = q.trim().toLowerCase();
    if (!t) return options;
    return options.filter((o) => `${o.label} ${o.detail ?? ""} ${o.group ?? ""}`.toLowerCase().includes(t));
  }, [options, q]);

  const show = () => {
    setQ("");
    setAt(Math.max(0, options.findIndex((o) => o.value === value)));
    setOpen(true);
  };
  const hide = (refocus: boolean) => {
    setOpen(false);
    if (refocus) btn.current?.focus();
  };
  const pick = (o: Option) => {
    hide(true);
    if (o.value !== value) onChange(o.value);
  };

  useEffect(() => {
    if (!open) return;
    // The first row sits under its group heading; "nearest" would scroll
    // the heading just out of view.
    if (at === 0) list.current?.scrollTo({ top: 0 });
    else list.current?.querySelector('[data-at="1"]')?.scrollIntoView({ block: "nearest" });
  }, [at, open, shown.length]);

  // A click anywhere else closes it, as a menu does.
  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) => {
      if (!root.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", away);
    return () => document.removeEventListener("mousedown", away);
  }, [open]);

  const onKey = (e: React.KeyboardEvent) => {
    if (!open) {
      if (["ArrowDown", "ArrowUp", "Enter", " "].includes(e.key)) { e.preventDefault(); show(); }
      return;
    }
    if (e.key === "ArrowDown") { e.preventDefault(); setAt((i) => Math.min(i + 1, shown.length - 1)); }
    else if (e.key === "ArrowUp") { e.preventDefault(); setAt((i) => Math.max(i - 1, 0)); }
    else if (e.key === "Enter") { e.preventDefault(); if (shown[at]) pick(shown[at]); }
    else if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); hide(true); }
    else if (e.key === "Tab") hide(false);
  };

  return (
    <div className={"sel" + (open ? " sel-open" : "")} ref={root} onKeyDown={onKey}>
      <button ref={btn} type="button" className="sel-btn" aria-haspopup="listbox" aria-expanded={open}
              aria-label={`${label}: ${current?.label ?? placeholder}`}
              onClick={() => (open ? hide(false) : show())}>
        {/* A value the options do not list yet (the catalogue still
            loading, a project since deleted) is shown as itself. */}
        <span className={"sel-value" + (current || value ? "" : " sel-placeholder")}>{current?.label ?? (value || placeholder)}</span>
        <svg className="sel-chevron" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
             strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <path d="M6 9l6 6 6-6" />
        </svg>
      </button>
      {open && (
        <div className={"sel-pop sel-" + align}>
          {searchable && (
            <input className="sel-search" autoFocus value={q} placeholder={`Search ${label.toLowerCase()}`}
                   aria-label={`Search ${label.toLowerCase()}`}
                   onChange={(e) => { setQ(e.target.value); setAt(0); }} />
          )}
          <div className="sel-list" role="listbox" aria-label={label} ref={list}>
            {shown.map((o, i) => (
              <Fragment key={(o.group ?? "") + ":" + o.value}>
                {o.group && o.group !== shown[i - 1]?.group && <div className="sel-group">{o.group}</div>}
                <button type="button" role="option" aria-selected={o.value === value}
                        data-at={i === at ? 1 : 0}
                        className={"sel-item" + (i === at ? " sel-on" : "")}
                        onMouseEnter={() => setAt(i)} onClick={() => pick(o)}>
                  <span className="sel-label">{o.label}</span>
                  {o.detail && <span className="num sel-detail">{o.detail}</span>}
                  <svg className="sel-check" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                       strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"
                       style={{ visibility: o.value === value ? "visible" : "hidden" }}>
                    <path d="M5 12.5l4.5 4.5L19 7.5" />
                  </svg>
                </button>
              </Fragment>
            ))}
            {shown.length === 0 && <p className="sel-empty">Nothing matches “{q}”</p>}
          </div>
        </div>
      )}
    </div>
  );
}
