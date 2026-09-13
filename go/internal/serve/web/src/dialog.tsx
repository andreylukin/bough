import { useEffect, useRef, useState } from "react";

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

export function DialogHost() {
  const [req, setReq] = useState<Req | null>(null);
  const [text, setText] = useState("");
  const input = useRef<HTMLInputElement>(null);
  const ok = useRef<HTMLButtonElement>(null);
  const opener = useRef<HTMLElement | null>(null);

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

  return (
    <div className="dlg-scrim" onMouseDown={(e) => { if (e.target === e.currentTarget) dismiss(); }}>
      <div className="dlg" role="dialog" aria-modal="true" aria-labelledby="dlg-title"
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
    </div>
  );
}
