import { Fragment, useCallback, useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useVisible, visible } from "./dialog";
import { anchorPlace } from "./popover";

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
 *
 * `collapsible` folds each group under its heading: the model picker
 * lists every model of every provider (hundreds on OpenRouter), so it
 * opens as one heading per provider, and a search shows every match.
 */
export function Select({ value, options: given, onChange, label, placeholder = "Choose", searchable = false, align = "start", note, detailHeading, footer, currentGroup = "Current", suffix, disabled = false, collapsible = false }: {
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
  /** The heading over the current value when the list is grouped. */
  currentGroup?: string;
  /** Muted text after the value in the button; shown only where CSS asks for it. */
  suffix?: string;
  /** Group headings fold their options until clicked. */
  collapsible?: boolean;
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

  const all = useMemo(() => {
    const t = q.trim().toLowerCase();
    // The current value leads, once, so it never hides mid-list.
    if (!t) {
      const cur = options.find((o) => o.value === value);
      return cur ? [{ ...cur, group: cur.group ? currentGroup : undefined }, ...options.filter((o) => o !== cur)] : options;
    }
    return options.filter((o) => `${o.label} ${o.detail ?? ""} ${o.group ?? ""}`.toLowerCase().includes(t));
  }, [options, q, value, currentGroup]);

  // Groups opened by a click this time the list is open; the rest fold.
  const [unfolded, setUnfolded] = useState<ReadonlySet<string>>(new Set());
  const fold = useMemo(() => foldState(all, collapsible && !q.trim(), currentGroup, unfolded), [all, collapsible, q, currentGroup, unfolded]);
  const shown = useMemo(() => all.filter((o) => !fold.folded(o.group)), [all, fold]);
  const toggle = (g: string) => setUnfolded((u) => {
    const n = new Set(u);
    if (!n.delete(g)) n.add(g);
    return n;
  });

  const show = () => {
    setQ("");
    setAt(0);
    setUnfolded(new Set());
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
      // Sideways, within the pane it sits in: an end-aligned composer picker
      // opened leftward over the sidebar.
      const v = visible(), pane = btn.current.closest(".thread")?.getBoundingClientRect();
      const p = anchorPlace(b, w, h, { left: Math.max(0, pane?.left ?? 0), right: Math.min(innerWidth, pane?.right ?? innerWidth), top: v.top, bottom: v.top + v.height }, align);
      setPos({ position: "fixed", minWidth, left: p.left, right: "auto", top: Math.max(v.top + 8, p.top), maxHeight: p.maxHeight });
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

  return <SelectView open={open} save={save} shown={shown} all={all} fold={fold} onFold={toggle} at={at} value={value} current={current} listId={listId} optId={optId}
    label={label} placeholder={placeholder} searchable={searchable} align={align} note={note} detailHeading={detailHeading}
    footer={footer} suffix={suffix} disabled={disabled} pos={pos} q={q} root={root} btn={btn} list={list} pop={pop}
    onKey={onKey} onButton={() => (open ? hide(false) : show())} onRetrySave={() => save && commit(save.value)}
    onQ={(v) => { setQ(v); setAt(0); }} onHover={setAt} onPick={pick} />;
}

/** Which groups fold: none while a search is typed or unless asked; never
 * the current value's, one of a single row, or the only group there is. */
export interface Fold { foldable: (g?: string) => boolean; folded: (g?: string) => boolean; count: (g: string) => number }

export function foldState(all: Option[], on: boolean, currentGroup: string, unfolded: ReadonlySet<string>): Fold {
  const counts = new Map<string, number>();
  for (const o of all) if (o.group) counts.set(o.group, (counts.get(o.group) ?? 0) + 1);
  const can = (g?: string) => on && !!g && g !== currentGroup && (counts.get(g) ?? 0) > 1;
  const many = [...counts.keys()].filter(can).length > 1;
  const foldable = (g?: string) => many && can(g);
  return { foldable, folded: (g) => foldable(g) && !unfolded.has(g!), count: (g) => counts.get(g) ?? 0 };
}

/** The Select as its state says: the button, a save in flight or refused, and, open, the options. */
export function SelectView({ open, save, shown, all = shown, fold, onFold, at, value, current, listId, optId, label, placeholder, searchable, align, note, detailHeading, footer, suffix, disabled, pos, q, root, btn, list, pop, onKey, onButton, onRetrySave, onQ, onHover, onPick }: {
  open: boolean; save: { value: string; state: "saving" | "failed" } | null; shown: Option[]; at: number; value: string; current?: Option;
  /** Every row before folding, so a folded group still shows its heading. */
  all?: Option[]; fold?: Fold; onFold?: (group: string) => void;
  listId: string; optId: (i: number) => string; label: string; placeholder: string; searchable: boolean; align: "start" | "end";
  note?: string; detailHeading?: string; footer?: (o: Option | undefined) => React.ReactNode; suffix?: string; disabled: boolean;
  pos: React.CSSProperties; q: string;
  root?: React.Ref<HTMLDivElement>; btn?: React.Ref<HTMLButtonElement>; list?: React.Ref<HTMLDivElement>; pop?: React.Ref<HTMLDivElement>;
  onKey: (e: React.KeyboardEvent) => void; onButton: () => void; onRetrySave: () => void; onQ: (q: string) => void;
  onHover: (i: number) => void; onPick: (o: Option) => void;
}) {
  return (
    <div className={"sel" + (open ? " sel-open" : "")} ref={root} onKeyDown={onKey}>
      <button ref={btn} type="button" className="sel-btn" role="combobox" aria-haspopup="listbox" aria-expanded={open} disabled={disabled}
              aria-controls={listId} aria-activedescendant={open && !searchable && shown[at] ? optId(at) : undefined}
              aria-label={`${label}: ${current?.label ?? (value || placeholder)}`}
              onClick={onButton}>
        {/* A value the options do not list yet (the catalogue still
            loading, a project since deleted) is shown as itself. */}
        <span className={"sel-value" + (current || value ? "" : " sel-placeholder")}>{current?.short ?? current?.label ?? (value ? value.split("/").pop() : placeholder)}</span>
        {suffix && <span className="sel-suffix">· {suffix}</span>}
        <svg className="sel-chevron" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
             strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <path d="M6 9l6 6 6-6" />
        </svg>
      </button>
      {save && (
        save.state === "saving" ? <span className="sel-save" role="status">Saving…</span>
          : <button type="button" className="sel-save sel-save-failed" onClick={onRetrySave}>Couldn’t save · Retry</button>
      )}
      {open && (
        <div ref={pop} style={pos} className={"sel-pop sel-" + align}>
          {searchable && (
            <input className="sel-search" autoFocus value={q} placeholder={`Search ${label.toLowerCase()}`}
                   aria-label={`Search ${label.toLowerCase()}`} role="combobox" aria-expanded="true"
                   aria-controls={listId} aria-autocomplete="list" aria-activedescendant={shown[at] ? optId(at) : undefined}
                   onChange={(e) => onQ(e.target.value)} />
          )}
          {note && <p className="sel-note">{note}</p>}
          {detailHeading && <div className="sel-cols" aria-hidden="true"><span>{label}</span><span>{detailHeading}</span></div>}
          <div className="sel-list" role="listbox" id={listId} aria-label={label} ref={list}>
            {(() => { let n = -1; return all.map((o, k) => {
              const head = o.group && o.group !== all[k - 1]?.group;
              const hidden = !!fold?.folded(o.group);
              const i = hidden ? -1 : ++n;
              return (
              <Fragment key={(o.group ?? "") + ":" + o.value}>
                {head && (fold?.foldable(o.group)
                  // mousedown would take focus from the search field.
                  ? <button type="button" className="sel-group sel-fold" aria-expanded={!hidden} tabIndex={-1}
                            onMouseDown={(e) => e.preventDefault()} onClick={() => onFold?.(o.group!)}>
                      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
                           strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M6 9l6 6 6-6" /></svg>
                      <span>{o.group}</span>
                      <span className="num sel-count">{fold.count(o.group!)}</span>
                    </button>
                  : <div className="sel-group">{o.group}</div>)}
                {!hidden && <button type="button" role="option" id={optId(i)} tabIndex={-1} aria-selected={o.value === value}
                        data-at={i === at ? 1 : 0}
                        className={"sel-item" + (i === at ? " sel-on" : "")}
                        onMouseEnter={() => onHover(i)} onClick={() => onPick(o)}>
                  <span className="sel-label">{o.label}</span>
                  {o.detail && <span className="num sel-detail" title={detailHeading}>{o.detail}</span>}
                  <svg className="sel-check" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                       strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"
                       style={{ visibility: o.value === value ? "visible" : "hidden" }}>
                    <path d="M5 12.5l4.5 4.5L19 7.5" />
                  </svg>
                </button>}
              </Fragment>
              ); }); })()}
            {shown.length === 0 && <p className="sel-empty">Nothing matches “{q}”</p>}
          </div>
          {footer && <div className="sel-foot" role="status">{footer(shown[at])}</div>}
        </div>
      )}
    </div>
  );
}
