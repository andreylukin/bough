// Centered new-session dialog — the CommandPalette's quieter sibling. One path
// input with fzf-style directory autocomplete (server-side subsequence match over
// dirs near the query plus every workspace a session has used): ↓/↑ move, tab
// completes the input to the selected dir, ↵ creates in it (⌘↵ uses the typed
// text verbatim), empty ↵ starts a chat-only session, esc closes. Creation errors
// (a path that doesn't exist, a file, …) show inline and keep the dialog open.
import { useEffect, useRef, useState } from "react";
import { c, mono, sans } from "../theme";
import { api, type DirHit } from "../api";

export function NewSessionDialog({
  open,
  onClose,
  onCreate,
}: {
  open: boolean;
  onClose: () => void;
  /** Resolves when the session exists; a rejection keeps the dialog open. */
  onCreate: (workspace: string | undefined) => Promise<unknown>;
}) {
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<DirHit[]>([]);
  const [active, setActive] = useState(0);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const seq = useRef(0); // drop out-of-order fetch responses

  useEffect(() => {
    if (open) {
      setQuery("");
      setErr(null);
      setActive(0);
      inputRef.current?.focus();
    }
  }, [open]);

  // Debounced fetch; the empty query already lists known workspaces + dirs near ~.
  useEffect(() => {
    if (!open) return;
    const id = ++seq.current;
    const t = setTimeout(() => {
      api.searchDirs(query)
        .then((dirs) => {
          if (seq.current !== id) return;
          setHits(dirs);
          setActive((a) => Math.min(a, Math.max(0, dirs.length - 1)));
        })
        .catch(() => seq.current === id && setHits([]));
    }, 120);
    return () => clearTimeout(t);
  }, [open, query]);

  if (!open) return null;

  const create = async (workspace: string | undefined) => {
    setBusy(true);
    setErr(null);
    try {
      await onCreate(workspace);
      onClose();
    } catch (e) {
      setErr((e as Error).message || "could not create the session");
    } finally {
      setBusy(false);
    }
  };

  const submit = (verbatim: boolean) => {
    if (busy) return;
    const typed = query.trim();
    const sel = hits[active];
    if (!verbatim && sel) return create(sel.path);
    return create(typed || undefined); // empty → chat-only session
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") return onClose();
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive((a) => Math.min(a + 1, hits.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((a) => Math.max(a - 1, 0));
    } else if (e.key === "Tab") {
      e.preventDefault();
      const sel = hits[active];
      if (sel) setQuery(sel.display + "/");
    } else if (e.key === "Enter") {
      e.preventDefault();
      submit(e.metaKey || e.ctrlKey);
    }
  };

  return (
    <div
      onMouseDown={onClose}
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(6,7,9,.55)",
        display: "flex",
        justifyContent: "center",
        alignItems: "flex-start",
        paddingTop: "12vh",
        zIndex: 50,
      }}
    >
      <div
        onMouseDown={(e) => e.stopPropagation()}
        style={{
          width: 560,
          maxWidth: "90vw",
          background: c.panel2,
          border: `1px solid ${c.border}`,
          borderRadius: 12,
          boxShadow: "0 24px 70px rgba(0,0,0,.5)",
          overflow: "hidden",
        }}
      >
        <input
          ref={inputRef}
          value={query}
          onChange={(e) => {
            setQuery(e.target.value);
            setErr(null);
          }}
          onKeyDown={onKeyDown}
          placeholder="Workspace path for the new session — e.g. ~/repos/app"
          spellCheck={false}
          style={{
            width: "100%",
            border: "none",
            outline: "none",
            background: "transparent",
            color: c.text,
            fontFamily: mono,
            fontSize: 14,
            padding: "16px 18px",
            borderBottom: `1px solid ${c.border}`,
          }}
        />
        <div style={{ maxHeight: 320, overflowY: "auto", padding: 6 }}>
          {hits.length === 0 && (
            <div style={{ padding: "12px 14px", color: c.muted2, fontSize: 13, fontFamily: sans }}>
              No matching directories{query.trim() ? " — ↵ still tries the path as typed." : "."}
            </div>
          )}
          {hits.map((h, i) => (
            <div
              key={h.path}
              onMouseEnter={() => setActive(i)}
              onMouseDown={(e) => {
                e.preventDefault();
                create(h.path);
              }}
              style={{
                display: "flex",
                alignItems: "center",
                gap: 8,
                padding: "8px 12px",
                borderRadius: 7,
                background: i === active ? c.panelInset : "transparent",
                cursor: "pointer",
              }}
            >
              <span style={{ fontFamily: mono, fontSize: 12.5, color: i === active ? c.text : c.text2, wordBreak: "break-all" }}>
                {h.display}
              </span>
              {h.repo && (
                <span style={{ fontFamily: mono, fontSize: 10.5, color: c.green, flex: "none" }}>⎇ repo</span>
              )}
            </div>
          ))}
        </div>
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: 12,
            padding: "9px 14px",
            borderTop: `1px solid ${c.border}`,
            fontFamily: mono,
            fontSize: 10.5,
            color: c.muted2,
          }}
        >
          <span>↵ {hits.length ? "open selected" : "use typed path"}</span>
          <span>⇥ complete</span>
          <span>⌘↵ typed as-is</span>
          <span>empty ↵ chat session</span>
          {busy && <span style={{ marginLeft: "auto", color: c.muted }}>creating…</span>}
        </div>
        {err && (
          <div style={{ padding: "8px 14px 12px", fontSize: 11.5, color: c.red, fontFamily: sans, wordBreak: "break-word" }}>
            {err}
          </div>
        )}
      </div>
    </div>
  );
}
