// ⌘K / ⌘P command palette — the keyboard spine. Two kinds of entries: actions
// (new session, map, bundles, stop) and one "switch to head" per session, so you
// can jump between many heads by typing instead of hunting the list. Arrow keys
// move, Enter runs, Esc closes. Deliberately quiet: one panel, no chrome.
import { useEffect, useMemo, useRef, useState } from "react";
import { c, mono, sans } from "../theme";
import type { Session } from "../types";

export interface Command {
  id: string;
  label: string;
  hint?: string;
  run: () => void;
}

export function CommandPalette({
  open,
  onClose,
  sessions,
  currentId,
  busy,
  onOpenSession,
  onInterrupt,
  onNewSession,
  onMap,
  onBundles,
}: {
  open: boolean;
  onClose: () => void;
  sessions: Session[];
  currentId: string | null;
  busy: boolean;
  onOpenSession: (id: string) => void;
  onInterrupt: () => void;
  onNewSession: () => void;
  onMap: () => void;
  onBundles: () => void;
}) {
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  const commands: Command[] = useMemo(() => {
    const actions: Command[] = [
      { id: "new", label: "New session", hint: "create", run: onNewSession },
      { id: "map", label: "Open map", hint: "all heads", run: onMap },
      { id: "bundles", label: "Configure gate bundles", hint: "network", run: onBundles },
    ];
    if (busy) actions.push({ id: "stop", label: "Stop the running turn", hint: "esc", run: onInterrupt });
    const heads: Command[] = sessions.map((s) => ({
      id: `head:${s.id}`,
      label: `Switch to ${s.title}`,
      hint: s.id === currentId ? "current" : s.kind,
      run: () => onOpenSession(s.id),
    }));
    return [...actions, ...heads];
  }, [sessions, currentId, busy, onNewSession, onMap, onBundles, onInterrupt, onOpenSession]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return commands;
    return commands.filter((cmd) => cmd.label.toLowerCase().includes(q));
  }, [commands, query]);

  // Reset on open; keep the active row in range as the filter narrows.
  useEffect(() => {
    if (open) {
      setQuery("");
      setActive(0);
      inputRef.current?.focus();
    }
  }, [open]);
  useEffect(() => {
    setActive((a) => Math.min(a, Math.max(0, filtered.length - 1)));
  }, [filtered.length]);

  if (!open) return null;

  function run(cmd: Command | undefined) {
    if (!cmd) return;
    cmd.run();
    onClose();
  }

  function onKeyDown(e: React.KeyboardEvent) {
    if (e.key === "Escape") return onClose();
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive((a) => Math.min(a + 1, filtered.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((a) => Math.max(a - 1, 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      run(filtered[active]);
    }
  }

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
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={onKeyDown}
          placeholder="Switch head or run a command…"
          style={{
            width: "100%",
            border: "none",
            outline: "none",
            background: "transparent",
            color: c.text,
            fontFamily: sans,
            fontSize: 15,
            padding: "16px 18px",
            borderBottom: `1px solid ${c.border}`,
          }}
        />
        <div style={{ maxHeight: 360, overflowY: "auto", padding: 6 }}>
          {filtered.length === 0 && (
            <div style={{ padding: "14px 14px", color: c.muted2, fontSize: 13 }}>No matches.</div>
          )}
          {filtered.map((cmd, i) => (
            <div
              key={cmd.id}
              onMouseEnter={() => setActive(i)}
              onMouseDown={(e) => {
                e.preventDefault();
                run(cmd);
              }}
              style={{
                display: "flex",
                alignItems: "center",
                justifyContent: "space-between",
                padding: "9px 12px",
                borderRadius: 7,
                background: i === active ? c.panelInset : "transparent",
                cursor: "pointer",
              }}
            >
              <span style={{ fontSize: 13.5, color: i === active ? c.text : c.text2 }}>{cmd.label}</span>
              {cmd.hint && (
                <span style={{ fontFamily: mono, fontSize: 11, color: c.muted2 }}>{cmd.hint}</span>
              )}
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
