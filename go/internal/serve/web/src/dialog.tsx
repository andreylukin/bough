import { Fragment, useEffect, useRef, useState, type RefObject } from "react";
import { createPortal } from "react-dom";
import { BINDINGS, keyChips } from "./keys";

/**
 * Asking for a name, or whether to go ahead.
 *
 * window.prompt and window.confirm draw the browser's own sheet — a
 * grey system box that says "127.0.0.1:7703 says", in a font nothing
 * else on the page uses. These ask the same questions in the page's own
 * voice. One host is mounted in the app; askText and askConfirm return
 * promises, so a call site reads as plainly as prompt() did.
 */
type Req =
  | { kind: "text"; title: string; body?: string; initial: string; placeholder?: string; action: string; allowEmpty: boolean;
      danger?: boolean; onSubmit?: (v: string) => Promise<void>; resolve: (v: string | null) => void }
  | { kind: "confirm"; title: string; body: string; action: string; danger: boolean; safe: boolean;
      resolve: (v: boolean) => void }

  | { kind: "choice"; title: string; body: string; actions: string[];
      resolve: (v: string | null) => void }
  | { kind: "keys"; title: string; mod: string; resolve: () => void };

let push: ((r: Req) => void) | null = null;

/** The part of the page actually visible: a phone keyboard shrinks it and pans it. */
export interface Visible { top: number; left: number; width: number; height: number }

export function visible(): Visible {
  const vv = typeof window !== "undefined" ? window.visualViewport : null;
  return vv ? { top: vv.offsetTop, left: vv.offsetLeft, width: vv.width, height: vv.height }
    : { top: 0, left: 0, width: innerWidth, height: innerHeight };
}

/** Calls back whenever the visible area resizes or pans; one subscription per overlay. */
export function useVisible(on: boolean, changed: () => void) {
  useEffect(() => {
    const vv = window.visualViewport;
    if (!on || !vv) return;
    vv.addEventListener("resize", changed);
    vv.addEventListener("scroll", changed);
    return () => { vv.removeEventListener("resize", changed); vv.removeEventListener("scroll", changed); };
  }, [on, changed]);
}

// Overlays size to what is visible, so a phone keyboard never hides a
// dialog's buttons: --vvh/--vvt are the visible height and its offset.
if (typeof window !== "undefined" && window.visualViewport) {
  const vv = window.visualViewport;
  const set = () => {
    const v = visible();
    document.documentElement.style.setProperty("--vvh", `${Math.round(v.height)}px`);
    document.documentElement.style.setProperty("--vvt", `${Math.round(v.top)}px`);
  };
  set();
  vv.addEventListener("resize", set);
  vv.addEventListener("scroll", set);
}

/**
 * The trimmed text entered, or null if the dialog was dismissed. With
 * onSubmit the dialog stays open until it resolves, so closing means it
 * was saved; a rejection keeps the draft and shows why.
 */
export function askText(title: string, opts: { initial?: string; body?: string; placeholder?: string; action?: string; allowEmpty?: boolean;
  danger?: boolean; onSubmit?: (v: string) => Promise<void> } = {}): Promise<string | null> {
  return new Promise((resolve) => {
    // Rendered outside the app (a story, a test) there is no host.
    if (!push) { resolve(window.prompt([title, opts.body].filter(Boolean).join("\n\n"), opts.initial ?? "")); return; }
    push({ kind: "text", title, body: opts.body, initial: opts.initial ?? "", placeholder: opts.placeholder,
           action: opts.action ?? "Save", allowEmpty: opts.allowEmpty ?? false, danger: opts.danger, onSubmit: opts.onSubmit, resolve });
  });
}

/** True only when the action was chosen. */
export function askConfirm(title: string, body: string, opts: { action?: string; danger?: boolean; safe?: boolean } = {}): Promise<boolean> {
  return new Promise((resolve) => {
    if (!push) { resolve(window.confirm(title)); return; }
    push({ kind: "confirm", title, body, action: opts.action ?? "Confirm", danger: opts.danger ?? false, safe: opts.safe ?? false, resolve });
  });
}

/** The action chosen, or null when dismissed. The first action is the primary one. */
export function askChoice(title: string, body: string, actions: string[]): Promise<string | null> {
  return new Promise((resolve) => {
    if (!push) { resolve(window.confirm(title) ? actions[0] ?? null : null); return; }
    push({ kind: "choice", title, body, actions, resolve });
  });
}

/** The keyboard shortcut sheet, drawn from the one bindings table. */
export function showShortcuts(mod: string) {
  if (!push || document.querySelector("[aria-modal='true']")) return;
  push({ kind: "keys", title: "Keyboard shortcuts", mod, resolve: () => {} });
}

/**
 * What aria-modal promises but does not do: Tab cycles inside the box,
 * and everything else on the page is inert while it is up. The modal is
 * portalled to <body>, so every other child of <body> is the background.
 */
export function useModal(box: RefObject<HTMLElement | null>, active: boolean) {
  useEffect(() => {
    if (!active) return;
    const layer = [...document.body.children].find((c) => box.current && c.contains(box.current));
    const others = [...document.body.children].filter((c) => c !== layer && !(c as HTMLElement).inert) as HTMLElement[];
    for (const o of others) o.inert = true;
    const tab = (e: KeyboardEvent) => {
      if (e.key !== "Tab" || !box.current) return;
      // Only what Tab can actually reach: enabled, and not taken out of order.
      const f = [...box.current.querySelectorAll<HTMLElement>("button,input,textarea,select,a[href],[tabindex]")]
        .filter((el) => el.tabIndex >= 0 && !(el as HTMLButtonElement).disabled && el.getClientRects().length > 0);
      if (f.length === 0) return;
      const first = f[0], last = f[f.length - 1];
      if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
      else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
    };
    document.addEventListener("keydown", tab);
    return () => { for (const o of others) o.inert = false; document.removeEventListener("keydown", tab); };
  }, [box, active]);
}

export function DialogHost() {
  const [req, setReq] = useState<Req | null>(null);
  const [text, setText] = useState("");
  const [saving, setSaving] = useState(false);
  const [failed, setFailed] = useState("");
  const input = useRef<HTMLInputElement>(null);
  const ok = useRef<HTMLButtonElement>(null);
  const cancel = useRef<HTMLButtonElement>(null);
  const opener = useRef<HTMLElement | null>(null);
  const box = useRef<HTMLDivElement>(null);
  useModal(box, req !== null);

  useEffect(() => {
    push = (r) => {
      opener.current = document.activeElement as HTMLElement | null;
      setText(r.kind === "text" ? r.initial : "");
      setSaving(false); setFailed("");
      setReq(r);
    };
    return () => { push = null; };
  }, []);

  // Back to whatever asked, once the page behind is no longer inert
  // (useModal's cleanup runs on this same commit), and never away from
  // a dialog opened in the meantime.
  useEffect(() => {
    if (req) return;
    const el = opener.current;
    opener.current = null;
    if (el?.isConnected && !document.querySelector("[aria-modal='true']")) el.focus?.();
  }, [req]);

  useEffect(() => {
    if (!req) return;
    requestAnimationFrame(() => {
      if (req.kind === "text") { input.current?.focus(); input.current?.select(); }
      // A safe confirm starts on Cancel: Enter alone never archives.
      else (req.kind === "keys" || (req.kind === "confirm" && req.safe) ? cancel : ok).current?.focus();
    });
  }, [req]);

  if (!req) return null;

  const dismiss = () => { if (!saving) finish(req.kind === "confirm" ? false : null); };
  const finish = (result: string | null | boolean) => {
    const r = req;
    setReq(null);
    if (r.kind === "confirm") r.resolve(result as boolean);
    else if (r.kind === "keys") r.resolve();
    else r.resolve(result as string | null);
  };
  // Blank is allowed where it means something (a session title handed
  // back); the same text again is not a change worth a request.
  const blocked = req.kind === "text" && (
    (!req.allowEmpty && !text.trim()) || (text.trim() === req.initial.trim() && text.trim() !== ""));
  const submit = async () => {
    if (blocked || saving) return;
    if (req.kind === "confirm") { finish(true); return; }
    if (req.kind !== "text") return;
    const v = text.trim();
    if (req.onSubmit) {
      setSaving(true); setFailed("");
      try { await req.onSubmit(v); }
      catch (e) { setFailed(e instanceof Error ? e.message : String(e)); setSaving(false); return; }
      setSaving(false);
    }
    finish(v);
  };

  return createPortal(
    <div className="dlg-scrim" onMouseDown={(e) => { if (e.target === e.currentTarget) dismiss(); }}>
      <div ref={box} className="dlg" role="dialog" aria-modal="true" aria-labelledby="dlg-title" aria-busy={saving || undefined}
           onKeyDown={(e) => { if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); dismiss(); } }}>
        <h2 id="dlg-title" className="dlg-title">{req.title}</h2>
        {(req.kind === "confirm" || req.kind === "choice" || (req.kind === "text" && req.body)) && <p className="dlg-body">{req.body}</p>}
        {req.kind === "keys" && (["Global", "Session", "Composer"] as const).map((sec) => (
          <section key={sec} className="keys-sec" aria-labelledby={`keys-${sec}`}>
            <h3 id={`keys-${sec}`} className="keys-h">{sec}</h3>
            <dl className="keys-list">
              {BINDINGS.filter((b) => b.section === sec).map((b) => (
                <div key={b.label} className="keys-row">
                  <dt>{b.keys.map((alt, i) => (
                    <Fragment key={alt}>
                      {i > 0 && <span className="keys-or">or</span>}
                      {alt.split(" ").map((k) => {
                        const chips = keyChips(k, req.mod === "\u2318");
                        return chips.length > 1
                          ? <span key={k} className="keys-combo">{chips.map((c) => <kbd key={c}>{c}</kbd>)}</span>
                          : <kbd key={k}>{chips[0]}</kbd>;
                      })}
                    </Fragment>
                  ))}</dt>
                  <dd>{b.label}{b.note && <span className="keys-note">{b.note}</span>}</dd>
                </div>
              ))}
            </dl>
          </section>
        ))}
        {req.kind === "text" && (
          <input ref={input} className="field dlg-input" value={text} placeholder={req.placeholder}
                 aria-labelledby="dlg-title" readOnly={saving} aria-invalid={failed ? true : undefined}
                 aria-describedby={failed ? "dlg-err" : undefined}
                 onChange={(e) => setText(e.target.value)}
                 onKeyDown={(e) => { if (e.key === "Enter" && !e.nativeEvent.isComposing) { e.preventDefault(); submit(); } }} />
        )}
        {failed && <p id="dlg-err" className="dlg-err" role="alert">Not saved: {failed}</p>}
        <div className="dlg-actions">
          {req.kind === "keys" && <p className="keys-foot">Press <kbd>?</kbd> anywhere to open this</p>}
          <button ref={cancel} className={req.kind === "keys" ? "btn btn-ghost" : "btn"} onClick={dismiss} disabled={saving}>{req.kind === "keys" ? "Close" : "Cancel"}</button>
          {req.kind === "keys" ? null : req.kind === "choice" ? [...req.actions].reverse().map((a, i, all) => (
            <button key={a} ref={i === all.length - 1 ? ok : undefined} className={"btn" + (i === all.length - 1 ? " btn-primary" : "")}
                    onClick={() => finish(a)}>{a}</button>
          )) : <button ref={ok} className={"btn " + (req.danger ? "btn-danger" : "btn-primary")}
                  disabled={blocked || saving} onClick={submit}>
            {saving ? req.action.replace(/e?$/, "ing…") : req.action}
          </button>}
        </div>
      </div>
    </div>,
    document.body,
  );
}
