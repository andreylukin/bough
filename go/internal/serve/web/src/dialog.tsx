import { useEffect, useRef, useState, type RefObject } from "react";
import { createPortal } from "react-dom";

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
  | { kind: "text"; title: string; initial: string; placeholder?: string; action: string; allowEmpty: boolean;
      resolve: (v: string | null) => void }
  | { kind: "confirm"; title: string; body: string; action: string; danger: boolean;
      resolve: (v: boolean) => void };

let push: ((r: Req) => void) | null = null;

/** The trimmed text entered, or null if the dialog was dismissed. */
export function askText(title: string, opts: { initial?: string; placeholder?: string; action?: string; allowEmpty?: boolean } = {}): Promise<string | null> {
  return new Promise((resolve) => {
    // Rendered outside the app (a story, a test) there is no host.
    if (!push) { resolve(window.prompt(title, opts.initial ?? "")); return; }
    push({ kind: "text", title, initial: opts.initial ?? "", placeholder: opts.placeholder,
           action: opts.action ?? "Save", allowEmpty: opts.allowEmpty ?? false, resolve });
  });
}

/** True only when the action was chosen. */
export function askConfirm(title: string, body: string, opts: { action?: string; danger?: boolean } = {}): Promise<boolean> {
  return new Promise((resolve) => {
    if (!push) { resolve(window.confirm(title)); return; }
    push({ kind: "confirm", title, body, action: opts.action ?? "Confirm", danger: opts.danger ?? false, resolve });
  });
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
      const f = [...box.current.querySelectorAll<HTMLElement>("button:not([disabled]),input,textarea,select,[tabindex]:not([tabindex='-1'])")];
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
  const input = useRef<HTMLInputElement>(null);
  const ok = useRef<HTMLButtonElement>(null);
  const opener = useRef<HTMLElement | null>(null);
  const box = useRef<HTMLDivElement>(null);
  useModal(box, req !== null);

  useEffect(() => {
    push = (r) => {
      opener.current = document.activeElement as HTMLElement | null;
      setText(r.kind === "text" ? r.initial : "");
      setReq(r);
    };
    return () => { push = null; };
  }, []);

  useEffect(() => {
    if (!req) return;
    requestAnimationFrame(() => {
      if (req.kind === "text") { input.current?.focus(); input.current?.select(); }
      else ok.current?.focus();
    });
  }, [req]);

  if (!req) return null;

  const dismiss = () => finish(req.kind === "text" ? null : false);
  const finish = (result: string | null | boolean) => {
    const r = req;
    setReq(null);
    opener.current?.focus?.(); // back to whatever asked
    if (r.kind === "text") r.resolve(result as string | null);
    else r.resolve(result as boolean);
  };
  const empty = req.kind === "text" && !req.allowEmpty && !text.trim();
  const submit = () => {
    if (empty) return;
    finish(req.kind === "text" ? text.trim() : true);
  };

  return createPortal(
    <div className="dlg-scrim" onMouseDown={(e) => { if (e.target === e.currentTarget) dismiss(); }}>
      <div ref={box} className="dlg" role="dialog" aria-modal="true" aria-labelledby="dlg-title"
           onKeyDown={(e) => { if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); dismiss(); } }}>
        <h2 id="dlg-title" className="dlg-title">{req.title}</h2>
        {req.kind === "confirm" && <p className="dlg-body">{req.body}</p>}
        {req.kind === "text" && (
          <input ref={input} className="field dlg-input" value={text} placeholder={req.placeholder}
                 aria-labelledby="dlg-title"
                 onChange={(e) => setText(e.target.value)}
                 onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); submit(); } }} />
        )}
        <div className="dlg-actions">
          <button className="btn" onClick={dismiss}>Cancel</button>
          <button ref={ok} className={"btn " + (req.kind === "confirm" && req.danger ? "btn-danger" : "btn-primary")}
                  disabled={empty} onClick={submit}>{req.action}</button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
