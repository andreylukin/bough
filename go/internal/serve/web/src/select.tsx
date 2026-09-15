import { Fragment, useCallback, useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useVisible, visible } from "./dialog";

/** One choosable value. Options sharing a `group` sit under one heading. */
export interface Option { value: string; label: string; detail?: string; group?: string; /** What the closed button shows, when shorter than the label. */ short?: string }

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
export function Select({ value, options: given, onChange, label, placeholder = "Choose", searchable = false, align = "start", note, detailHeading, footer, disabled = false }: {
  value: string;
  /** Shown but not choosable, when the setting does not apply yet. */
  disabled?: boolean;
  options: Option[];
  /** A promise that resolves false (or rejects) is a save that failed: the button says so and offers the retry. */
  onChange: (value: string) => void | Promise<unknown>;
  /** Names the control for assistive tech; the visible label sits beside it. */
  label: string;
  placeholder?: string;
  searchable?: boolean;
  align?: "start" | "end";
  /** A line above the options saying when the choice takes effect. */
  note?: string;
  /** Names the detail column, when options carry one. */
  detailHeading?: string;
  /** A fixed line under the list about the highlighted option. */
  footer?: (o: Option | undefined) => React.ReactNode;
}) {
  // A placeholder is what the button says with nothing chosen; it is never a checked option.
  const options = useMemo(() => given.filter((o) => !(o.value === "" && o.label === placeholder)), [given, placeholder]);
  const [open, setOpen] = useState(false);
  const [q, setQ] = useState("");
  const [at, setAt] = useState(0);
  const root = useRef<HTMLDivElement>(null);
  const btn = useRef<HTMLButtonElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const pop = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<React.CSSProperties>({});
  const current = options.find((o) => o.value === value);
  const id = useId();
  const listId = id + "-list";
  // Focus stays on the button (or the search field); the highlighted row
  // is announced through aria-activedescendant.
  const optId = (i: number) => `${id}-opt-${i}`;

  const shown = useMemo(() => {
    const t = q.trim().toLowerCase();
    // The current value leads, once, so it never hides mid-list.
    if (!t) {
      const cur = options.find((o) => o.value === value);
      return cur ? [{ ...cur, group: cur.group ? "Current" : undefined }, ...options.filter((o) => o !== cur)] : options;
    }
    return options.filter((o) => `${o.label} ${o.detail ?? ""} ${o.group ?? ""}`.toLowerCase().includes(t));
  }, [options, q, value]);

  const show = () => {
    setQ("");
    setAt(0);
    setOpen(true);
  };
  const hide = (refocus: boolean) => {
    setOpen(false);
    if (refocus) btn.current?.focus();
  };
  const [save, setSave] = useState<{ value: string; state: "saving" | "failed" } | null>(null);
  // The picker stays open while a save is in flight and closes only once
  // it lands; a failure is said in the picker, beside the choice.
  const commit = (v: string) => {
    const r = onChange(v);
    if (!(r instanceof Promise)) { hide(true); return; }
    setSave({ value: v, state: "saving" });
    r.then((ok) => {
      if (ok === false) { setSave({ value: v, state: "failed" }); return; }
      setSave(null);
      hide(true);
    }, () => setSave({ value: v, state: "failed" }));
  };
  const pick = (o: Option) => {
    if (o.value !== value) commit(o.value);
    else hide(true);
  };

  useEffect(() => {
    if (!open) return;
    // The first row sits under its group heading; "nearest" would scroll
    // the heading just out of view.
    if (at === 0) list.current?.scrollTo({ top: 0 });
    else list.current?.querySelector('[data-at="1"]')?.scrollIntoView({ block: "nearest" });
  }, [at, open, shown.length]);

  // Placed against the viewport: 4px from the button, 8px from every
  // edge, flipped above when there is more room there.
  const place = useCallback(() => {
      if (!btn.current || !pop.current) return;
      const b = btn.current.getBoundingClientRect();
      // Fixed, so a percentage min-width would mean the viewport's.
      const minWidth = Math.min(Math.max(b.width, 220), innerWidth - 16);
      pop.current.style.minWidth = `${minWidth}px`;
      const w = pop.current.offsetWidth;
      const h = pop.current.scrollHeight;
      // Against what is visible: a keyboard both shrinks and pans the page.
      const v = visible();
      const below = v.top + v.height - b.bottom - 12, above = b.top - v.top - 12;
      const up = h > below && above > below;
      let left = align === "end" ? b.right - w : b.left;
      left = Math.max(8, Math.min(left, innerWidth - 8 - w));
      setPos({ position: "fixed", minWidth, left, right: "auto", top: up ? Math.max(v.top + 8, b.top - 4 - Math.min(h, above)) : b.bottom + 4,
               maxHeight: up ? above : below });
  }, [align]);
  useVisible(open, place);
  useLayoutEffect(() => {
    if (!open) return;
    place();
    addEventListener("resize", place);
    addEventListener("scroll", place, true);
    return () => { removeEventListener("resize", place); removeEventListener("scroll", place, true); };
  }, [open, place, shown.length]);

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
    else if (e.key === "Enter" && !e.nativeEvent.isComposing) { e.preventDefault(); if (shown[at]) pick(shown[at]); }
    else if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); hide(true); }
    else if (e.key === "Tab") hide(false);
  };

  return (
    <div className={"sel" + (open ? " sel-open" : "")} ref={root} onKeyDown={onKey}>
      <button ref={btn} type="button" className="sel-btn" role="combobox" aria-haspopup="listbox" aria-expanded={open} disabled={disabled}
              aria-controls={listId} aria-activedescendant={open && !searchable && shown[at] ? optId(at) : undefined}
              aria-label={`${label}: ${current?.label ?? (value || placeholder)}`}
              onClick={() => (open ? hide(false) : show())}>
        {/* A value the options do not list yet (the catalogue still
            loading, a project since deleted) is shown as itself. */}
        <span className={"sel-value" + (current || value ? "" : " sel-placeholder")}>{current?.short ?? current?.label ?? (value ? value.split("/").pop() : placeholder)}</span>
        <svg className="sel-chevron" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
             strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <path d="M6 9l6 6 6-6" />
        </svg>
      </button>
      {save && (
        save.state === "saving" ? <span className="sel-save" role="status">Saving…</span>
          : <button type="button" className="sel-save sel-save-failed" onClick={() => commit(save.value)}>Couldn’t save · Retry</button>
      )}
      {open && (
        <div ref={pop} style={pos} className={"sel-pop sel-" + align}>
          {searchable && (
            <input className="sel-search" autoFocus value={q} placeholder={`Search ${label.toLowerCase()}`}
                   aria-label={`Search ${label.toLowerCase()}`} role="combobox" aria-expanded="true"
                   aria-controls={listId} aria-autocomplete="list" aria-activedescendant={shown[at] ? optId(at) : undefined}
                   onChange={(e) => { setQ(e.target.value); setAt(0); }} />
          )}
          {note && <p className="sel-note">{note}</p>}
          {detailHeading && <div className="sel-cols" aria-hidden="true"><span>{label}</span><span>{detailHeading}</span></div>}
          <div className="sel-list" role="listbox" id={listId} aria-label={label} ref={list}>
            {shown.map((o, i) => (
              <Fragment key={(o.group ?? "") + ":" + o.value}>
                {o.group && o.group !== shown[i - 1]?.group && <div className="sel-group">{o.group}</div>}
                <button type="button" role="option" id={optId(i)} tabIndex={-1} aria-selected={o.value === value}
                        data-at={i === at ? 1 : 0}
                        className={"sel-item" + (i === at ? " sel-on" : "")}
                        onMouseEnter={() => setAt(i)} onClick={() => pick(o)}>
                  <span className="sel-label">{o.label}</span>
                  {o.detail && <span className="num sel-detail" title={detailHeading}>{o.detail}</span>}
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
          {footer && <div className="sel-foot" role="status">{footer(shown[at])}</div>}
        </div>
      )}
    </div>
  );
}
