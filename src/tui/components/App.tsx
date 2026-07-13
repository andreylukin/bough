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
import { buildLines } from "../lines.ts";
import { type MouseEvent, onMouse } from "../mouse.ts";
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
import { saveLastSession } from "../state.ts";

type Mode = "chat" | "picker" | "new" | "fork" | "diff" | "model" | "panel" | "help";

export function App(
  { initialSessions, defaultWorkspace }: { initialSessions: Session[]; defaultWorkspace: string },
) {
  const { exit } = useApp();
  const { stdout } = useStdout();
  const store = useStore(initialSessions);
  const [mode, setMode] = useState<Mode>("chat");
  const [input, setInput] = useState("");
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
  // composer history (sent messages, oldest first); null idx = editing the draft
  const history = useRef<string[]>([]);
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
  const lines = useMemo(
    () => buildLines(store.thread, store.streaming, isExpanded, width),
    [store.thread, store.streaming, isExpanded, width],
  );
  const toggleGroup = useCallback((key: string) => {
    setToggled((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }, []);

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

  // Everything a mouse event needs from the current layout, without re-subscribing.
  const layout = useRef({ mode, start, padTop, maxOff });
  layout.current = { mode, start, padTop, maxOff };
  const linesRef = useRef(lines);
  linesRef.current = lines;
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
      const line = linesRef.current[idx];
      if (line?.click) toggleGroup(line.click);
    });
    return () => onMouse(null);
  }, [toggleGroup]);

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
      return;
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
    if (key.return) {
      const text = input.trim();
      if (!text) return;
      setInput("");
      history.current.push(text);
      setHistIdx(null);
      draft.current = "";
      // alt+enter stages the message until the turn finishes; plain enter steers.
      submit(text, key.meta);
      return;
    }
    // ↑/↓ recall previously sent messages (the in-progress draft is stashed).
    if (key.upArrow) {
      const h = history.current;
      if (h.length === 0) return;
      if (histIdx === null) draft.current = input;
      const ni = histIdx === null ? h.length - 1 : Math.max(0, histIdx - 1);
      setHistIdx(ni);
      setInput(h[ni]);
      return;
    }
    if (key.downArrow) {
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
    if (key.ctrl && ch === "u") return setInput("");
    if (key.backspace || key.delete) return setInput((v) => v.slice(0, -1));
    if (ch && !key.ctrl && !key.meta) setInput((v) => v + ch);
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
                { length: Math.max(0, bodyH - 2) },
                (_, i) => <Text key={`pad-${i}`}>{" "}</Text>,
              )}
              <Text>
                {" "}
                <Text color="green" bold>new session</Text>
                <Text dimColor>{"  "}{shortWs}</Text>
              </Text>
              <Text dimColor>{" "}type a message to start · ^p resume a session · ? help</Text>
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
              {store.pending
                ? <NetApproval req={store.pending} count={store.pendingCount} />
                : <Composer input={input} queued={[]} busy={store.busy} />}
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
        />
      </Box>
    </Box>
  );
}
