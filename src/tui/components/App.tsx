// Top-level TUI shell — a full-screen (alternate buffer) app. The conversation is
// a virtualized viewport over pre-wrapped lines (lines.ts): scrolling is an index
// offset, mouse clicks map row → line → expandable tool group. The bottom chrome
// (composer/approval card + status bar) is pinned to the terminal's last rows; its
// height is measured after render so the viewport always fits exactly.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Box, type DOMElement, measureElement, Text, useApp, useInput, useStdout } from "ink";
import type { Session } from "../../schema/parts.ts";
import {
  api,
  type BoughConfig,
  type DirHit,
  type KeyProvider,
  type McpStatus,
  type NetConfig,
  type NetStatus,
  type SkillInfo,
} from "../api.ts";
import { useStore } from "../store.ts";
import { buildLines, type LiveBranch } from "../lines.ts";
import { fuzzyScore, segmentParts, wordLeft, wordRight } from "../format.ts";
import { type MouseEvent, onMouse, onPaste } from "../mouse.ts";
import { Composer } from "./Composer.tsx";
import { StatusBar } from "./StatusBar.tsx";
import { flattenTree, SessionPicker, type TreeRow } from "./SessionPicker.tsx";
import { NetApproval } from "./NetApproval.tsx";
import { NewSession } from "./NewSession.tsx";
import { ForkPicker } from "./ForkPicker.tsx";
import { DiffView, flattenDiffs } from "./DiffView.tsx";
import { modelEntries, ModelPicker } from "./ModelPicker.tsx";
import { Panel, PANEL_TABS, type PanelTab } from "./Panel.tsx";
import { Help } from "./Help.tsx";
import { appendHistory, loadHistory, saveLastSession } from "../state.ts";

type Mode = "chat" | "picker" | "new" | "fork" | "diff" | "model" | "panel" | "help";

export function App(
  { initialSessions, defaultWorkspace }: { initialSessions: Session[]; defaultWorkspace: string },
) {
  const { exit } = useApp();
  const { stdout } = useStdout();
  const store = useStore(initialSessions);
  const [mode, setMode] = useState<Mode>("chat");
  // The composer: text plus a cursor for real line editing (arrows, ctrl+a/e/w/k,
  // word jumps). `set` clamps; helpers keep every mutation cursor-correct.
  const [comp, setComp] = useState({ text: "", cursor: 0 });
  const input = comp.text;
  const setInput = useCallback((text: string) => setComp({ text, cursor: text.length }), []);
  const insertAtCursor = useCallback((chunk: string) => {
    setComp((c) => ({
      text: c.text.slice(0, c.cursor) + chunk + c.text.slice(c.cursor),
      cursor: c.cursor + chunk.length,
    }));
  }, []);
  const moveCursor = useCallback((to: (c: { text: string; cursor: number }) => number) => {
    setComp((c) => ({ ...c, cursor: Math.max(0, Math.min(c.text.length, to(c))) }));
  }, []);
  const [quitHint, setQuitHint] = useState(false);
  const lastCtrlC = useRef(0);
  const [err, setErr] = useState<string | null>(null);
  // viewport state
  const [scrollOff, setScrollOff] = useState(0); // lines up from the bottom; 0 = follow
  const [expandAll, setExpandAll] = useState(false);
  const [toggled, setToggled] = useState<Set<string>>(new Set()); // per-group overrides
  const [, setTick] = useState(0); // resize repaint
  // picker state
  const [pickSel, setPickSel] = useState(0);
  const [filter, setFilter] = useState("");
  const [filterActive, setFilterActive] = useState(false);
  // composer history (sent messages, oldest first, persisted across runs);
  // null idx = editing the draft
  const history = useRef<string[]>(loadHistory());
  const [histIdx, setHistIdx] = useState<number | null>(null);
  const draft = useRef("");
  // panel state
  const [panelTab, setPanelTab] = useState<PanelTab>("net");
  const [mcpSel, setMcpSel] = useState(0);
  const [panelMsg, setPanelMsg] = useState<string | null>(null);
  const [netStat, setNetStat] = useState<NetStatus | null>(null);
  const [policy, setPolicy] = useState<NetConfig | null>(null);
  const [mcpStat, setMcpStat] = useState<McpStatus | null>(null);
  const [skillsList, setSkillsList] = useState<SkillInfo[] | null>(null);
  // new-session state
  const [newQuery, setNewQuery] = useState("");
  const [newSel, setNewSel] = useState(0);
  const [dirHits, setDirHits] = useState<DirHit[]>([]);
  // fork state
  const [forkSel, setForkSel] = useState(0);
  // diff state
  const [fileSel, setFileSel] = useState(0);
  const [diffScroll, setDiffScroll] = useState(0);
  // model state
  const [cfg, setCfg] = useState<BoughConfig | null>(null);
  const [modelSel, setModelSel] = useState(0);
  const [keyInput, setKeyInput] = useState<string | null>(null); // masked API-key entry

  const { open } = store;
  const openSession = useCallback((s: Session) => {
    setErr(null);
    setMode("chat");
    setFilter("");
    setFilterActive(false);
    setScrollOff(0);
    setToggled(new Set());
    setExpandAll(false);
    saveLastSession(s.id);
    open(s.id).catch((e) => setErr(String(e)));
  }, [open]);

  // Launch = a fresh draft targeting the caller's cwd; the session is created on
  // the first send. ^p resumes existing sessions.

  // Config once at startup so the status bar can name the active model (^o keeps
  // it fresh after switches; there's no config event to subscribe to).
  useEffect(() => {
    api.getConfig().then(setCfg, () => {});
  }, []);

  // Repaint on terminal resize (SIGWINCH fallback: the node-compat resize event
  // doesn't always fire under Deno).
  useEffect(() => {
    const bump = () => setTick((t) => t + 1);
    stdout?.on("resize", bump);
    try {
      Deno.addSignalListener("SIGWINCH", bump);
    } catch {
      // not available on this platform — resize event alone will have to do
    }
    return () => {
      stdout?.off("resize", bump);
      try {
        Deno.removeSignalListener("SIGWINCH", bump);
      } catch {
        // mirror of the add above
      }
    };
  }, [stdout]);

  // Fetch panel data when the panel opens or its tab switches.
  const { currentId } = store;
  const refreshPanel = useCallback((tab: PanelTab) => {
    if (tab === "net") {
      api.netStatus().then(setNetStat, () => setNetStat(null));
      api.getPolicy().then(setPolicy, () => setPolicy(null));
    } else if (tab === "mcp") {
      api.mcpStatus(currentId).then(setMcpStat, (e) => {
        setMcpStat(null);
        setPanelMsg(String(e));
      });
    } else {
      api.skills().then(setSkillsList, () => setSkillsList([]));
    }
  }, [currentId]);
  useEffect(() => {
    if (mode === "panel") refreshPanel(panelTab);
  }, [mode, panelTab, refreshPanel]);

  // Debounced workspace autocomplete while the new-session dialog is open.
  useEffect(() => {
    if (mode !== "new") return;
    const t = setTimeout(() => {
      api.searchDirs(newQuery).then(setDirHits, () => setDirHits([]));
    }, 120);
    return () => clearTimeout(t);
  }, [mode, newQuery]);

  const rows = stdout?.rows || 24;
  const width = stdout?.columns || 80;

  // ---- the conversation viewport ------------------------------------------
  const isExpanded = useCallback(
    (key: string) => (expandAll !== toggled.has(key)),
    [expandAll, toggled],
  );
  // Subagents spawned by the open session (live branch cards). A finished one that
  // already posted its [subagent finished] note is rendered inline by that note, so
  // only surface branches with no completion note in the thread yet.
  const notedIds = useMemo(() => {
    const ids = new Set<string>();
    for (const m of store.thread) {
      if (m.role !== "system") continue;
      const t = m.parts.filter((p) => p.type === "text").map((p) => (p as { text: string }).text)
        .join("\n");
      const id = t.match(/\[subagent finished\] ".*" \(([^)]+)\)/)?.[1];
      if (id) ids.add(id);
    }
    return ids;
  }, [store.thread]);
  const liveBranches = useMemo<LiveBranch[]>(
    () =>
      store.sessions
        .filter((s) => s.kind === "subagent" && s.originId === currentId && !notedIds.has(s.id))
        .map((s) => ({ id: s.id, title: s.title || "(untitled)", busy: !!s.busy })),
    [store.sessions, currentId, notedIds],
  );
  const lines = useMemo(
    () => buildLines(store.thread, store.streaming, isExpanded, width, liveBranches),
    [store.thread, store.streaming, isExpanded, width, liveBranches],
  );
  const toggleGroup = useCallback((key: string) => {
    setToggled((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }, []);

  // A turn that ends on a tool group has its whole answer inside the fold —
  // auto-expand it when the message finishes (user-testing finding). Only for
  // messages watched live (seen pending), so opening an old session doesn't
  // pop open its history.
  const isExpandedRef = useRef(isExpanded);
  isExpandedRef.current = isExpanded;
  const watchedPending = useRef(new Set<string>());
  useEffect(() => {
    for (const m of store.thread) {
      if (m.pending) {
        watchedPending.current.add(m.id);
        continue;
      }
      if (!watchedPending.current.delete(m.id)) continue;
      const segs = segmentParts(m.parts);
      const last = segs[segs.length - 1];
      if (last?.kind === "tools") {
        const key = `${m.id}:${segs.length - 1}`;
        if (!isExpandedRef.current(key)) toggleGroup(key);
      }
    }
  }, [store.thread, toggleGroup]);

  // Bottom-chrome height is measured post-render (the approval card and queued
  // rows vary); one frame of lag at most.
  const chromeRef = useRef<DOMElement | null>(null);
  const [chromeH, setChromeH] = useState(4);
  useEffect(() => {
    if (!chromeRef.current) return;
    const { height } = measureElement(chromeRef.current);
    if (height > 0 && height !== chromeH) setChromeH(height);
  });
  const viewH = Math.max(3, rows - chromeH);
  // The "N more lines below" indicator takes a viewport row while scrolled up.
  const bodyH = Math.max(2, viewH - (scrollOff > 0 ? 1 : 0));
  const maxOff = Math.max(0, lines.length - bodyH);
  const off = Math.min(scrollOff, maxOff);
  const start = Math.max(0, lines.length - bodyH - off);
  const visible = lines.slice(start, start + bodyH);
  const padTop = bodyH - visible.length;

  // Everything a mouse event needs from the current layout. Synced in an effect
  // (post-commit) rather than during render, so a click always maps against the
  // frame actually ON SCREEN — chromeH is measured a frame late, so the padTop
  // used during an in-flight render can differ from what's painted by one row.
  const layout = useRef({ mode, start, padTop, maxOff, lines });
  useEffect(() => {
    layout.current = { mode, start, padTop, maxOff, lines };
  });
  // Composer autocomplete: "/" at the start completes skills, "@" completes
  // workspace files (needs a live session — drafts have no workspace yet).
  interface Popup {
    kind: "skill" | "file";
    items: { label: string; detail: string; insert: string }[];
    sel: number;
    tokenStart: number;
    tokenEnd: number;
  }
  const [popup, setPopup] = useState<Popup | null>(null);
  const skillsCache = useRef<SkillInfo[] | null>(null);
  useEffect(() => {
    if (mode !== "chat") {
      setPopup(null);
      return;
    }
    const { text, cursor } = comp;
    const end = (() => {
      const ws = text.slice(cursor).search(/\s/);
      return ws < 0 ? text.length : cursor + ws;
    })();
    if (text.startsWith("/") && cursor >= 1 && !/\s/.test(text.slice(0, cursor))) {
      const q = text.slice(1, cursor);
      const apply = (skills: SkillInfo[]) => {
        const items = skills
          .map((s) => ({ s, score: fuzzyScore(s.name, q) }))
          .filter((x) => x.score > 0)
          .sort((a, b) =>
            b.score - a.score || a.s.name.length - b.s.name.length ||
            a.s.name.localeCompare(b.s.name)
          )
          .slice(0, 6)
          .map(({ s }) => ({ label: `/${s.name}`, detail: s.description, insert: `/${s.name} ` }));
        setPopup(
          items.length ? { kind: "skill", items, sel: 0, tokenStart: 0, tokenEnd: end } : null,
        );
      };
      if (skillsCache.current) apply(skillsCache.current);
      else api.skills().then((s) => (skillsCache.current = s, apply(s)), () => {});
      return;
    }
    const at = text.lastIndexOf("@", cursor - 1);
    if (
      at >= 0 && currentId && !/\s/.test(text.slice(at + 1, cursor)) &&
      (at === 0 || /\s/.test(text[at - 1]))
    ) {
      const q = text.slice(at + 1, cursor);
      const t = setTimeout(() => {
        api.searchFiles(currentId, q).then((files) => {
          const items = files.slice(0, 6).map((f) => ({
            label: `@${f}`,
            detail: "",
            insert: `@${f} `,
          }));
          setPopup(
            items.length ? { kind: "file", items, sel: 0, tokenStart: at, tokenEnd: end } : null,
          );
        }, () => setPopup(null));
      }, 120);
      return () => clearTimeout(t);
    }
    setPopup(null);
  }, [comp, mode, currentId]);

  // Bracketed pastes land whole in the composer (chat mode only), newlines intact.
  const modeRef = useRef(mode);
  modeRef.current = mode;
  useEffect(() => {
    onPaste((text) => {
      if (modeRef.current === "chat") insertAtCursor(text);
    });
    return () => onPaste(null);
  }, [insertAtCursor]);

  // A click key is either a tool-group fold or "open:<sessionId>" (descend into a
  // subagent branch). Kept in a ref so the mouse subscription stays stable.
  const onClickRef = useRef<(key: string) => void>(() => {});
  onClickRef.current = (key) => {
    if (key.startsWith("open:")) {
      const s = store.sessions.find((x) => x.id === key.slice(5));
      if (s) openSession(s);
      return;
    }
    toggleGroup(key);
  };
  useEffect(() => {
    onMouse((ev: MouseEvent) => {
      const l = layout.current;
      if (ev.kind === "wheel-up") {
        setScrollOff((o) => Math.min(l.maxOff, o + 3));
        return;
      }
      if (ev.kind === "wheel-down") {
        setScrollOff((o) => Math.max(0, o - 3));
        return;
      }
      if (l.mode !== "chat") return;
      const idx = l.start + (ev.y - 1) - l.padTop;
      const line = l.lines[idx];
      if (line?.click) onClickRef.current(line.click);
    });
    return () => onMouse(null);
  }, []);

  const treeRows: TreeRow[] = filter
    ? flattenTree(store.sessions)
      .filter(({ s }) => (s.title || "").toLowerCase().includes(filter.toLowerCase()))
      .map(({ s }) => ({ s, depth: 0 }))
    : flattenTree(store.sessions);

  const forkMsgs = [...store.thread].reverse();
  const diffEntries = flattenDiffs(store.changes);
  const cfgEntries = cfg ? modelEntries(cfg) : [];

  // Send, creating the draft's session on first use.
  const submit = useCallback((text: string, queue: boolean) => {
    setScrollOff(0);
    if (store.currentId) {
      store.send(text, queue).catch((e) => setErr(String(e)));
      return;
    }
    store.newSession(defaultWorkspace).then(
      (s) => {
        saveLastSession(s.id);
        return store.send(text, false, s.id);
      },
      (e) => setErr(String(e)),
    );
  }, [store.currentId, store.send, store.newSession, defaultWorkspace]);

  useInput((ch, key) => {
    // Quit: double ctrl+c.
    if (key.ctrl && ch === "c") {
      const now = Date.now();
      if (now - lastCtrlC.current < 2000) exit();
      lastCtrlC.current = now;
      setQuitHint(true);
      setTimeout(() => setQuitHint(false), 2000);
      return;
    }

    if (mode === "picker") {
      // Filter-entry sub-mode (`/`): printable keys type; enter/esc drop back to nav.
      if (filterActive) {
        if (key.escape) {
          setFilterActive(false);
          setFilter("");
          return;
        }
        if (key.return) {
          setFilterActive(false);
          return;
        }
        if (key.backspace || key.delete) {
          setPickSel(0);
          return setFilter((f) => f.slice(0, -1));
        }
        if (ch && !key.ctrl && !key.meta && !key.upArrow && !key.downArrow) {
          setPickSel(0);
          setFilter((f) => f + ch);
          return;
        }
      }
      if (key.escape) {
        setMode("chat");
        return;
      }
      if (key.return) {
        const sel = treeRows[pickSel];
        if (sel) openSession(sel.s);
        return;
      }
      if (key.upArrow || (!filterActive && ch === "k")) {
        return setPickSel((i) => Math.max(0, i - 1));
      }
      if (key.downArrow || (!filterActive && ch === "j")) {
        return setPickSel((i) => Math.min(treeRows.length - 1, i + 1));
      }
      if (!filterActive && ch === "g") return setPickSel(0);
      if (!filterActive && ch === "G") return setPickSel(Math.max(0, treeRows.length - 1));
      if (!filterActive && ch === "/") {
        setFilterActive(true);
        return;
      }
      if (key.ctrl && ch === "t") {
        store.newSession().then((s) => openSession(s), (e) => setErr(String(e)));
        return;
      }
      if (key.ctrl && ch === "x") {
        const sel = treeRows[pickSel];
        if (sel) store.archive(sel.s.id); // session.archived prunes the list
        return;
      }
      if (key.backspace || key.delete) {
        setPickSel(0);
        return setFilter((f) => f.slice(0, -1));
      }
      return;
    }

    if (mode === "panel") {
      if (key.escape) return setMode("chat");
      if (key.tab) {
        setPanelMsg(null);
        setMcpSel(0);
        setPanelTab((t) => PANEL_TABS[(PANEL_TABS.indexOf(t) + 1) % PANEL_TABS.length]);
        return;
      }
      if (panelTab === "net" && ch === "y") {
        const on = policy?.mode !== "yolo";
        api.setYolo(on).then(({ config }) => setPolicy(config), (e) => setPanelMsg(String(e)));
        return;
      }
      if (panelTab === "mcp") {
        const names = mcpStat ? Object.keys(mcpStat.registry.servers).sort() : [];
        if (key.upArrow || ch === "k") return setMcpSel((i) => Math.max(0, i - 1));
        if (key.downArrow || ch === "j") {
          return setMcpSel((i) => Math.min(Math.max(0, names.length - 1), i + 1));
        }
        const name = names[mcpSel];
        if (!name) return;
        if (ch === "c" && store.currentId) {
          setPanelMsg(`connecting ${name}…`);
          api.connectMcp(name, store.currentId).then((r) => {
            setPanelMsg(
              r.connected
                ? `${name}: connected (${r.tools?.length ?? 0} tools)`
                : `${name}: ${r.error ?? "connect failed"}`,
            );
            refreshPanel("mcp");
          }, (e) => setPanelMsg(String(e)));
          return;
        }
        if (ch === "r") {
          api.restartMcp(name).then(() => {
            setPanelMsg(`${name}: restarted`);
            refreshPanel("mcp");
          }, (e) => setPanelMsg(String(e)));
          return;
        }
        if (ch === "e") {
          const on = !mcpStat?.active.includes(name);
          api.setMcpEnabled(name, on, store.currentId)
            .then(() => refreshPanel("mcp"), (e) => setPanelMsg(String(e)));
          return;
        }
        if (ch === "a") {
          api.mcpAuth(name).then((r) =>
            setPanelMsg(
              r.status === "authorized"
                ? `${name}: authorized ✓`
                : `open in browser: ${r.authorizationUrl}`,
            ), (e) => setPanelMsg(String(e)));
          return;
        }
      }
      return;
    }

    if (mode === "help") {
      setMode("chat"); // any key closes
      // …but a ctrl-chord typed right behind the closing key shouldn't be
      // swallowed — fall through to the chat handlers below (Esc+^p chording).
      if (!(key.ctrl && ch)) return;
    }

    if (mode === "new") {
      if (key.escape) return setMode("chat");
      if (key.return) {
        const hit = dirHits[newSel];
        // A typed query with no hit does nothing — a silent workspace-less session
        // would run turns in the server's cwd (the live repo). Clear the query to
        // create one deliberately.
        if (!hit && newQuery.trim() !== "") return;
        store.newSession(hit?.path).then((s) => openSession(s), (e) => setErr(String(e)));
        return;
      }
      if (key.upArrow) return setNewSel((i) => Math.max(0, i - 1));
      if (key.downArrow) return setNewSel((i) => Math.min(dirHits.length - 1, i + 1));
      if (key.backspace || key.delete) {
        setNewSel(0);
        return setNewQuery((q) => q.slice(0, -1));
      }
      if (ch && !key.ctrl && !key.meta) {
        setNewSel(0);
        setNewQuery((q) => q + ch);
      }
      return;
    }

    if (mode === "fork") {
      if (key.escape) return setMode("chat");
      if (key.return) {
        const msg = forkMsgs[forkSel];
        if (!msg) return;
        setMode("chat");
        store.fork(msg.id).then((s) => s && openSession(s));
        return;
      }
      if (key.upArrow) return setForkSel((i) => Math.max(0, i - 1));
      if (key.downArrow) return setForkSel((i) => Math.min(forkMsgs.length - 1, i + 1));
      return;
    }

    if (mode === "diff") {
      if (key.escape) return setMode("chat");
      if (key.upArrow) {
        setDiffScroll(0);
        return setFileSel((i) => Math.max(0, i - 1));
      }
      if (key.downArrow) {
        setDiffScroll(0);
        return setFileSel((i) => Math.min(diffEntries.length - 1, i + 1));
      }
      if (ch === "j") return setDiffScroll((s) => s + 3);
      if (ch === "k") return setDiffScroll((s) => Math.max(0, s - 3));
      if (ch === "a") {
        const e = diffEntries[fileSel];
        if (e) store.applyChanges(e.source, [e.file.path]);
        return;
      }
      if (ch === "R") {
        store.revertChanges();
        return;
      }
      return;
    }

    if (mode === "model") {
      // Masked key entry for the selected provider row.
      if (keyInput !== null) {
        if (key.escape) return setKeyInput(null);
        if (key.return) {
          const e = cfgEntries[modelSel];
          const k = keyInput.trim();
          setKeyInput(null);
          if (!e || e.kind !== "key" || !k) return;
          api.putKey(e.id as KeyProvider, k)
            .then(() => api.getConfig().then(setCfg))
            .catch((err) => setErr(String(err)));
          return;
        }
        if (key.backspace || key.delete) return setKeyInput((v) => (v ?? "").slice(0, -1));
        if (ch && !key.ctrl && !key.meta) setKeyInput((v) => (v ?? "") + ch);
        return;
      }
      if (key.escape) return setMode("chat");
      if (key.upArrow) return setModelSel((i) => Math.max(0, i - 1));
      if (key.downArrow) return setModelSel((i) => Math.min(cfgEntries.length - 1, i + 1));
      if (key.return) {
        const e = cfgEntries[modelSel];
        if (!e) return;
        if (e.kind === "key") {
          setKeyInput("");
          return;
        }
        (e.kind === "model" ? api.setModel(e.id) : api.setWorker(e.id))
          .then(() => api.getConfig().then(setCfg))
          .catch((err) => setErr(String(err)));
        return;
      }
      return;
    }

    // chat mode
    if (key.ctrl && ch === "p") {
      setPickSel(0);
      setFilter("");
      setMode("picker");
      return;
    }
    if (key.ctrl && ch === "n") {
      setNewQuery("");
      setNewSel(0);
      setDirHits([]);
      setMode("new");
      return;
    }
    if (key.ctrl && ch === "f") {
      if (store.thread.length === 0) return;
      setForkSel(0);
      setMode("fork");
      return;
    }
    if (key.ctrl && ch === "d") {
      setFileSel(0);
      setDiffScroll(0);
      setMode("diff");
      return;
    }
    if (key.ctrl && ch === "o") {
      setCfg(null);
      setModelSel(0);
      api.getConfig().then(setCfg, (e) => setErr(String(e)));
      setMode("model");
      return;
    }
    if (key.ctrl && ch === "k") {
      store.compact().then((s) => s && openSession(s));
      return;
    }
    if (key.ctrl && ch === "t") {
      setPanelMsg(null);
      setMcpSel(0);
      setMode("panel");
      return;
    }
    if (key.ctrl && ch === "e") {
      setToggled(new Set());
      setExpandAll((v) => !v);
      return;
    }
    if (ch === "?" && !key.ctrl && !key.meta && input === "" && !store.pending) {
      setMode("help");
      return;
    }
    // Autocomplete popup owns navigation + enter while open.
    if (popup) {
      if (key.escape) return setPopup(null);
      if (key.upArrow) {
        return setPopup((p) => p && { ...p, sel: (p.sel - 1 + p.items.length) % p.items.length });
      }
      if (key.downArrow || key.tab) {
        return setPopup((p) => p && { ...p, sel: (p.sel + 1) % p.items.length });
      }
      if (key.return) {
        const it = popup.items[popup.sel];
        setComp((c) => {
          const text = c.text.slice(0, popup.tokenStart) + it.insert + c.text.slice(popup.tokenEnd);
          return { text, cursor: popup.tokenStart + it.insert.length };
        });
        setPopup(null);
        return;
      }
    }
    if (key.pageUp) return setScrollOff((o) => Math.min(maxOff, o + Math.max(1, viewH - 2)));
    if (key.pageDown) return setScrollOff((o) => Math.max(0, o - Math.max(1, viewH - 2)));
    if (store.pending) {
      // The approval card replaces the composer; plain keys act on the hold.
      if (ch === "a") return store.resolvePending(true, "once");
      if (ch === "A") return store.resolvePending(true, "session");
      if (ch === "d") return store.resolvePending(false);
      return;
    }
    if (key.escape) {
      if (store.busy) store.interrupt();
      return;
    }
    // Send resolves the text inside the updater: an Enter that lands in the same
    // React batch as just-typed/pasted text would otherwise read a stale (empty)
    // closure and silently drop the send.
    const sendNow = (queue: boolean) => {
      setComp((c) => {
        const text = c.text.trim();
        if (!text) return c;
        queueMicrotask(() => {
          history.current.push(text);
          appendHistory(text);
          setHistIdx(null);
          draft.current = "";
          // alt+enter stages the message until the turn finishes; plain enter steers.
          submit(text, queue);
        });
        return { text: "", cursor: 0 };
      });
    };
    if (key.return) {
      sendNow(key.meta);
      return;
    }
    // ↑/↓: history recall on a single-line draft; cursor movement across lines
    // once the input is multiline (pasted blocks, ctrl+j).
    const multiline = input.includes("\n");
    if (key.upArrow) {
      if (multiline) {
        return moveCursor((c) => {
          const prevNl = c.text.lastIndexOf("\n", Math.max(0, c.cursor - 1));
          if (prevNl < 0) return c.cursor;
          const col = c.cursor - prevNl - 1;
          const prevPrevNl = c.text.lastIndexOf("\n", prevNl - 1);
          return Math.min(prevPrevNl + 1 + col, prevNl);
        });
      }
      const h = history.current;
      if (h.length === 0) return;
      if (histIdx === null) draft.current = input;
      const ni = histIdx === null ? h.length - 1 : Math.max(0, histIdx - 1);
      setHistIdx(ni);
      setInput(h[ni]);
      return;
    }
    if (key.downArrow) {
      if (multiline) {
        return moveCursor((c) => {
          const nextNl = c.text.indexOf("\n", c.cursor);
          if (nextNl < 0) return c.cursor;
          const prevNl = c.text.lastIndexOf("\n", Math.max(0, c.cursor - 1));
          const col = c.cursor - prevNl - 1;
          const lineEnd = c.text.indexOf("\n", nextNl + 1);
          return Math.min(nextNl + 1 + col, lineEnd < 0 ? c.text.length : lineEnd);
        });
      }
      if (histIdx === null) return;
      if (histIdx >= history.current.length - 1) {
        setHistIdx(null);
        setInput(draft.current);
        return;
      }
      const ni = histIdx + 1;
      setHistIdx(ni);
      setInput(history.current[ni]);
      return;
    }
    // Line editing (readline muscle memory). Word jumps first — ⌥← arrives as
    // meta+leftArrow in some terminals and would match the plain-arrow branch.
    if (key.meta && (ch === "b" || key.leftArrow)) {
      return moveCursor((c) => wordLeft(c.text, c.cursor));
    }
    if (key.meta && (ch === "f" || key.rightArrow)) {
      return moveCursor((c) => wordRight(c.text, c.cursor));
    }
    if (key.leftArrow) return moveCursor((c) => c.cursor - 1);
    if (key.rightArrow) return moveCursor((c) => c.cursor + 1);
    if (key.ctrl && ch === "a") return moveCursor(() => 0);
    if (key.ctrl && ch === "e") return moveCursor((c) => c.text.length);
    if (key.ctrl && ch === "w") {
      return setComp((c) => {
        const from = wordLeft(c.text, c.cursor);
        return { text: c.text.slice(0, from) + c.text.slice(c.cursor), cursor: from };
      });
    }
    if (key.ctrl && ch === "k") {
      return setComp((c) => ({ text: c.text.slice(0, c.cursor), cursor: c.cursor }));
    }
    if (key.ctrl && ch === "u") return setInput("");
    if (key.ctrl && ch === "j") return insertAtCursor("\n");
    if (key.backspace || key.delete) {
      return setComp((c) =>
        c.cursor === 0 ? c : {
          text: c.text.slice(0, c.cursor - 1) + c.text.slice(c.cursor),
          cursor: c.cursor - 1,
        }
      );
    }
    if (ch && !key.ctrl && !key.meta) {
      // Stream reads can coalesce fast input into one chunk ("text\r"), so a
      // newline can arrive as DATA rather than a return keypress. Normalize CRs
      // (a raw \r in the composer corrupts the render) and honor a trailing
      // newline as "…then send" — that's what the sender meant.
      const norm = ch.replace(/\r\n?/g, "\n");
      if (norm.includes("\n")) {
        const sendAfter = norm.endsWith("\n");
        const body = sendAfter ? norm.slice(0, -1) : norm;
        if (body) insertAtCursor(body);
        if (sendAfter) sendNow(key.meta);
        return;
      }
      insertAtCursor(ch);
    }
  });

  const shortWs = defaultWorkspace.replace(new RegExp(`^${Deno.env.get("HOME")}`), "~");
  const isDraft = !store.currentId;

  const modal = mode === "picker"
    ? (
      <SessionPicker
        rowsList={treeRows}
        selected={pickSel}
        filter={filter}
        filterActive={filterActive}
        rows={rows}
      />
    )
    : mode === "panel"
    ? (
      <Panel
        tab={panelTab}
        status={netStat}
        policy={policy}
        feed={store.feed}
        mcp={mcpStat}
        mcpSel={mcpSel}
        mcpMsg={panelMsg}
        skills={skillsList}
        rows={rows}
      />
    )
    : mode === "help"
    ? <Help />
    : mode === "new"
    ? <NewSession query={newQuery} hits={dirHits} selected={newSel} />
    : mode === "fork"
    ? <ForkPicker messages={forkMsgs} selected={forkSel} rows={rows} />
    : mode === "diff"
    ? <DiffView entries={diffEntries} fileSel={fileSel} scroll={diffScroll} rows={rows} />
    : mode === "model"
    ? (cfg
      ? <ModelPicker cfg={cfg} entries={cfgEntries} selected={modelSel} keyInput={keyInput} />
      : <Text dimColor>loading config…</Text>)
    : null;

  return (
    <Box flexDirection="column" height={rows} width={width}>
      <Box flexDirection="column" flexGrow={1} overflow="hidden">
        {modal ?? (isDraft && store.thread.length === 0
          ? (
            <>
              {Array.from(
                { length: Math.max(0, Math.floor((bodyH - 5) / 2)) },
                (_, i) => <Text key={`pad-${i}`}>{" "}</Text>,
              )}
              <Box flexDirection="column" alignItems="center">
                <Text>
                  <Text color="green">●</Text> <Text bold>bough</Text>
                </Text>
                <Text dimColor>new session in {shortWs}</Text>
                <Text>{" "}</Text>
                <Text dimColor>type to start · ^p resume · ? help</Text>
              </Box>
            </>
          )
          : (
            <>
              {Array.from({ length: padTop }, (_, i) => <Text key={`pad-${i}`}>{" "}</Text>)}
              {visible.map((l, i) => (
                <Text key={`l-${start + i}`} wrap="truncate">{l.text || " "}</Text>
              ))}
              {off > 0 ? <Text dimColor>↓ {off} more line{off === 1 ? "" : "s"} below</Text> : null}
            </>
          ))}
      </Box>
      <Box ref={chromeRef} flexDirection="column">
        {mode === "chat"
          ? (
            <>
              {store.queued.map((q, i) => <Text key={i} dimColor>⧖ queued: {q}</Text>)}
              {err ? <Text color="red">{err}</Text> : null}
              {store.notice ? <Text color="yellow">{store.notice}</Text> : null}
              {popup && !store.pending
                ? (
                  <Box flexDirection="column" borderStyle="round" borderColor="gray" paddingX={1}>
                    {popup.items.map((it, i) => (
                      <Text key={it.label} inverse={i === popup.sel} wrap="truncate">
                        {it.label}
                        {it.detail ? <Text dimColor>{"  "}{it.detail}</Text> : null}
                      </Text>
                    ))}
                  </Box>
                )
                : null}
              {store.pending
                ? <NetApproval req={store.pending} count={store.pendingCount} />
                : <Composer input={input} cursor={comp.cursor} busy={store.busy} />}
            </>
          )
          : null}
        <StatusBar
          connected={store.connected}
          busy={store.busy}
          session={store.session}
          pendingCount={store.pendingCount}
          quitHint={quitHint}
          mode={mode === "chat" && store.pending ? "approval" : mode}
          usage={store.usage}
          draftLabel={isDraft ? `new · ${shortWs}` : null}
          model={cfg ? (cfg.models.find((m) => m.id === cfg.model)?.label ?? cfg.model) : null}
          parentTitle={store.session?.kind === "subagent" && store.session.originId
            ? (store.sessions.find((s) => s.id === store.session!.originId)?.title ?? "parent")
              .replace(/^subagent · /, "")
            : null}
        />
      </Box>
    </Box>
  );
}
