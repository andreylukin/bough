// Top-level TUI shell. One global useInput dispatcher keyed by mode, and the
// append-only Static log: finished messages print once into native terminal
// scrollback; only the dynamic tail (pending messages, composer, status bar)
// re-renders. Opening a session appends a divider and reprints its thread.
import { useCallback, useEffect, useRef, useState } from "react";
import { Box, Static, Text, useApp, useInput, useStdout } from "ink";
import type { Message, Session } from "../../schema/parts.ts";
import {
  api,
  type BoughConfig,
  type DirHit,
  type McpStatus,
  type NetConfig,
  type NetStatus,
  type SkillInfo,
} from "../api.ts";
import { type TuiSession, useStore } from "../store.ts";
import { Divider, MessageView } from "./Conversation.tsx";
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
import { loadLastSession, saveLastSession } from "../state.ts";

// How much history reprints into scrollback when opening a session.
const HISTORY_CAP = 50;

type LogItem =
  | { key: string; kind: "divider"; label: string }
  | { key: string; kind: "message"; msg: Message };

type Mode = "chat" | "picker" | "new" | "fork" | "diff" | "model" | "panel" | "help";

export function App({ initialSessions }: { initialSessions: Session[] }) {
  const { exit } = useApp();
  const { stdout } = useStdout();
  const store = useStore(initialSessions);
  const [mode, setMode] = useState<Mode>("chat");
  const [input, setInput] = useState("");
  const [expandTools, setExpandTools] = useState(false);
  const [log, setLog] = useState<LogItem[]>([]);
  // Which message ids have been printed into the Static log. Cleared when a session
  // opens so switching back reprints its thread under a fresh divider.
  const logged = useRef(new Set<string>());
  const divSeq = useRef(0);
  const [quitHint, setQuitHint] = useState(false);
  const lastCtrlC = useRef(0);
  const [err, setErr] = useState<string | null>(null);
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

  const { open } = store;
  const openSession = useCallback((s: Session) => {
    logged.current.clear();
    divSeq.current += 1;
    setLog((l) => [
      ...l,
      { key: `div-${divSeq.current}`, kind: "divider", label: s.title || "(untitled)" },
    ]);
    setErr(null);
    setMode("chat");
    setFilter("");
    setFilterActive(false);
    saveLastSession(s.id);
    open(s.id).catch((e) => setErr(String(e)));
  }, [open]);

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

  // On launch: reopen the session from last time, else the most recently active
  // one (lastLlmAt beats createdAt so an old-but-live session wins), else the picker.
  useEffect(() => {
    const candidates = flattenTree(initialSessions as TuiSession[]).map((r) => r.s);
    const lastId = loadLastSession();
    const target = candidates.find((s) => s.id === lastId) ??
      [...candidates].sort((a, b) =>
        (b.lastLlmAt ?? b.createdAt) - (a.lastLlmAt ?? a.createdAt)
      )[0];
    if (target) openSession(target);
    else setMode("picker");
    // eslint-disable-next-line react-hooks/exhaustive-deps -- mount only
  }, []);

  // Seal the thread's finished prefix into the Static log. A message is sealed once
  // it and everything before it is non-pending (Static is append-only, so order and
  // immutability both matter).
  useEffect(() => {
    const idx = store.thread.findIndex((m) => m.pending);
    const sealed = idx < 0 ? store.thread : store.thread.slice(0, idx);
    const fresh = sealed.filter((m) => !logged.current.has(m.id));
    if (fresh.length === 0) return;
    const toLog = fresh.slice(-HISTORY_CAP);
    for (const m of fresh) logged.current.add(m.id);
    setLog((l) => [
      ...l,
      ...(fresh.length > toLog.length
        ? [{
          key: `skip-${++divSeq.current}`,
          kind: "divider" as const,
          label: `… ${fresh.length - toLog.length} earlier messages`,
        }]
        : []),
      ...toLog.map((m) => ({ key: m.id, kind: "message" as const, msg: m })),
    ]);
  }, [store.thread]);

  // Debounced workspace autocomplete while the new-session dialog is open.
  useEffect(() => {
    if (mode !== "new") return;
    const t = setTimeout(() => {
      api.searchDirs(newQuery).then(setDirHits, () => setDirHits([]));
    }, 120);
    return () => clearTimeout(t);
  }, [mode, newQuery]);

  const treeRows: TreeRow[] = filter
    ? flattenTree(store.sessions)
      .filter(({ s }) => (s.title || "").toLowerCase().includes(filter.toLowerCase()))
      .map(({ s }) => ({ s, depth: 0 }))
    : flattenTree(store.sessions);

  const forkMsgs = [...store.thread].reverse();
  const diffEntries = flattenDiffs(store.changes);
  const cfgEntries = cfg ? modelEntries(cfg) : [];

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
        if (store.currentId) setMode("chat");
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
      if (key.escape) return setMode("chat");
      if (key.upArrow) return setModelSel((i) => Math.max(0, i - 1));
      if (key.downArrow) return setModelSel((i) => Math.min(cfgEntries.length - 1, i + 1));
      if (key.return) {
        const e = cfgEntries[modelSel];
        if (!e) return;
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
    if (key.ctrl && ch === "e") return setExpandTools((v) => !v);
    if (ch === "?" && !key.ctrl && !key.meta && input === "" && !store.pending) {
      setMode("help");
      return;
    }
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
      store.send(text, key.meta).catch((e) => setErr(String(e)));
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

  // Messages not yet sealed into the Static log — the live tail that re-renders.
  const dynamic = store.thread.filter((m) => !logged.current.has(m.id));
  const rows = stdout?.rows || 24; // `||`: a 0-row pty must not collapse the modals

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
      ? <ModelPicker cfg={cfg} entries={cfgEntries} selected={modelSel} />
      : <Text dimColor>loading config…</Text>)
    : null;

  return (
    <>
      <Static items={log}>
        {(item) =>
          item.kind === "divider"
            ? <Divider key={item.key} label={item.label} />
            : <MessageView key={item.key} msg={item.msg} />}
      </Static>
      <Box flexDirection="column">
        {modal ?? (
          <>
            {dynamic.map((m) => (
              <MessageView
                key={m.id}
                msg={m}
                streaming={store.streaming[m.id]}
                expandTools={expandTools}
              />
            ))}
            {err ? <Text color="red">{err}</Text> : null}
            {store.notice ? <Text color="yellow">{store.notice}</Text> : null}
            {store.pending
              ? <NetApproval req={store.pending} count={store.pendingCount} />
              : <Composer input={input} queued={store.queued} busy={store.busy} />}
          </>
        )}
        <StatusBar
          connected={store.connected}
          busy={store.busy}
          session={store.session}
          pendingCount={store.pendingCount}
          quitHint={quitHint}
          mode={mode === "chat" && store.pending ? "approval" : mode}
          usage={store.usage}
        />
      </Box>
    </>
  );
}
