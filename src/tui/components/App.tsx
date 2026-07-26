// Top-level TUI shell — a full-screen (alternate buffer) app. The conversation is
// a virtualized viewport over pre-wrapped lines (lines.ts): scrolling is an index
// offset, mouse clicks map row → line → expandable tool group. The bottom chrome
// (composer/approval card + status bar) is pinned to the terminal's last rows; its
// height is measured after render so the viewport always fits exactly.
import { applyTheme, palette, THEME_PRESETS } from "../theme.ts";

// BOUGH_TUI_DEBUG=1: append composer/popup traces to ~/.bough/tui-debug.log
// (inside the TUI's --allow-write list; /tmp is not). For diagnosing behavior
// that only reproduces in a real terminal, where a screenshot can't show state.
const dbg = Deno.env.get("BOUGH_TUI_DEBUG")
  ? (m: string) => {
    try {
      Deno.writeTextFileSync(
        `${Deno.env.get("HOME")}/.bough/tui-debug.log`,
        `${new Date().toISOString()} ${m}\n`,
        { append: true },
      );
    } catch {
      // diagnostics must never break the TUI
    }
  }
  : null;
/** Slash-popup description: skill frontmatter arrives raw, so strip wrapping
 * quotes and cap on a word boundary with … (never cut mid-word). */
function popupDetail(desc: string, max = 72): string {
  let d = desc.trim();
  const q = d[0];
  if ((q === '"' || q === "'") && d.length > 1 && d.at(-1) === q) d = d.slice(1, -1).trim();
  if (d.length <= max) return d;
  const cut = d.lastIndexOf(" ", max);
  return d.slice(0, cut > 0 ? cut : max).replace(/[\s.,;:·—-]+$/, "") + "…";
}

/** Popup row label: fuzzy-matched chars in accent+bold (so results don't look
 * arbitrary), and for @ rows the directory prefix dimmed (`dimTo`) so basenames
 * stand out. Runs of same-styled chars render as one Text chunk. */
function PopupLabel({ label, hl = [], dimTo = 0 }: {
  label: string;
  hl?: number[];
  dimTo?: number;
}) {
  if (hl.length === 0 && dimTo === 0) return <>{label}</>;
  const set = new Set(hl);
  const segs: { text: string; style: "match" | "dim" | "plain" }[] = [];
  for (let i = 0; i < label.length; i++) {
    const style = set.has(i) ? "match" : i < dimTo ? "dim" : "plain";
    const last = segs.at(-1);
    if (last?.style === style) last.text += label[i];
    else segs.push({ text: label[i], style });
  }
  return (
    <>
      {segs.map((s, i) =>
        s.style === "match"
          ? <Text key={i} bold color={palette.accent}>{s.text}</Text>
          : s.style === "dim"
          ? <Text key={i} dimColor>{s.text}</Text>
          : <Text key={i}>{s.text}</Text>
      )}
    </>
  );
}

/** "due" / "in 5m" / "in 3h" / "in 2d" — the schedule list's next-run column. */
function nextIn(ts: number): string {
  const d = ts - Date.now();
  if (d <= 0) return "due";
  const m = Math.round(d / 60_000);
  if (m < 60) return `in ${Math.max(1, m)}m`;
  const h = Math.round(m / 60);
  return h < 24 ? `in ${h}h` : `in ${Math.round(h / 24)}d`;
}
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Box, type DOMElement, measureElement, useApp, useInput, useStdout } from "ink";
import { Text } from "./Text.tsx";
import type { Message, Session } from "../../schema/parts.ts";
import {
  api,
  type BoughConfig,
  type DirHit,
  type KeyProvider,
  type McpStatus,
  type SkillInfo,
  type ThemeState,
  type WfAgentView,
  type WireSchedule,
  type WireSection,
  type WireWorkflowRun,
} from "../api.ts";
import { useStore } from "../store.ts";
import { type Branch, buildLines, parseSubagentNote, type SubagentNote } from "../lines.ts";
import {
  ctxPctLeft,
  fmtTokens,
  fmtUsd,
  fuzzyPositions,
  fuzzyScore,
  linkAt,
  segmentParts,
  wordLeft,
  wordRight,
} from "../format.ts";
import { type MouseEvent, onMouse, onNavKey, onPaste } from "../mouse.ts";
import { extractSpan, highlightSpan, rowSpan, type Selection, selRows } from "../selection.ts";
import { findMatches, markLine } from "../search.ts";
import { Composer } from "./Composer.tsx";
import { ActivityLine, StatusBar } from "./StatusBar.tsx";
import { SubagentRail } from "./SubagentRail.tsx";
import { flattenTree, SessionPicker, type TreeRow } from "./SessionPicker.tsx";
import { AskCard } from "./AskCard.tsx";
import { NewSession } from "./NewSession.tsx";
import {
  buildTree,
  ConversationTree,
  sectionSpan,
  treeItems,
  type TreeNode,
} from "./ConversationTree.tsx";
import { DiffView, flattenDiffs } from "./DiffView.tsx";
import { modelEntries, ModelPicker } from "./ModelPicker.tsx";
import { Panel, PANEL_TABS, type PanelTab, PanelTabs } from "./Panel.tsx";
import { Jobs } from "./Jobs.tsx";
import {
  agentDetailLines,
  phaseGroups,
  visibleAgents,
  WF_FILTERS,
  type WfFilter,
  type WfLevel,
  WorkflowChip,
  Workflows,
} from "./Workflows.tsx";
import { Help, helpMaxScroll } from "./Help.tsx";
import { appendHistory, appendShellHistory, loadHistory } from "../state.ts";
import { shellHistoryCorpus } from "../shell_history.ts";
import { copyToClipboard } from "../clipboard.ts";
import { progressEnd, progressStart, setTitle, tabColor, termBackground } from "../term.ts";

// picker + conversation + mcp/skills are all tabs of the one "panel" view.
type Mode = "chat" | "new" | "panel" | "help";

export function App(
  { initialSessions, defaultWorkspace }: { initialSessions: Session[]; defaultWorkspace: string },
) {
  const { exit } = useApp();
  const { stdout } = useStdout();
  const store = useStore(initialSessions);
  const [mode, setMode] = useState<Mode>("chat");
  // Cursor into the subagent rail under the status bar; null = the composer has
  // focus. ↓ on an empty composer enters the rail, enter opens the branch.
  const [railSel, setRailSel] = useState<number | null>(null);
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
  // ^x "stop everything here" arms on the first press and fires on the second
  // (same double-tap as quit and archive — one keypress discarding a subtree of
  // running work is exactly the reflex those guards exist for).
  const lastCtrlX = useRef(0);
  const [err, setErr] = useState<string | null>(null);
  // /conversation info card above the composer; esc or the next send dismisses it.
  const [showInfo, setShowInfo] = useState(false);
  // viewport state
  const [scrollOff, setScrollOff] = useState(0); // lines up from the bottom; 0 = follow
  const [expandAll, setExpandAll] = useState(false);
  const [toggled, setToggled] = useState<Set<string>>(new Set()); // per-group overrides
  // Transcript search (^s): null = closed; while open the keyboard types the
  // query and enter/↑/↓ walk matches (the viewport recenters on the current one).
  const [searchQ, setSearchQ] = useState<string | null>(null);
  const [searchIdx, setSearchIdx] = useState(0);
  // `!cmd` local shell passthrough: last run's card (never touches the thread).
  const [shellOut, setShellOut] = useState<
    { cmd: string; out: string; code: number | null } | null
  >(null);
  const shellSeq = useRef(0);
  // /schedule popup: recurring runs (list + toggle/delete + a small create form).
  // Owns the keyboard while open; the human mirror of the model's schedule.* fn.
  const SCHED_FIELDS = ["title", "prompt", "spec", "workspace"] as const;
  const [sched, setSched] = useState<
    {
      scheds: WireSchedule[];
      sel: number;
      // Create form: one string per SCHED_FIELDS entry; null = list view.
      form: { fields: string[]; focus: number } | null;
      msg: string | null;
    } | null
  >(null);
  // (Re)load the list, keeping the cursor near where it was after a mutation.
  const loadSchedules = useCallback(() => {
    api.listSchedules().then(
      (scheds) =>
        setSched((s) => ({
          scheds,
          sel: Math.min(s?.sel ?? 0, Math.max(0, scheds.length - 1)),
          form: null,
          msg: null,
        })),
      (e) => setErr(String(e)),
    );
  }, []);
  const [, setTick] = useState(0); // resize repaint
  // picker state. The cursor also lives in a ref: two keys parsed from one stdin
  // chunk (fast typing / key repeat) run in the same React batch, so an Enter right
  // behind a ↓ would act on the pre-movement selection if it read the state var.
  const [pickSel, setPickSel] = useState(0);
  const pickSelRef = useRef(0);
  // The cursor is anchored to a session IDENTITY, not just this index: forks and
  // subagents stream into the list live and reorder the rows, so a raw index
  // would slide onto whatever row shifted under it. movePickSel records the id it
  // lands on; a re-anchor effect (below, near treeRows) re-resolves that id to
  // its new index whenever the row set changes.
  const pickSelIdRef = useRef<string | null>(null);
  const treeRowsRef = useRef<TreeRow[]>([]);
  const movePickSel = useCallback((v: number | ((i: number) => number)) => {
    // Resolve against the ref NOW (not in the updater): state updaters run at
    // flush, so a same-chunk follow-up key would still read the stale position.
    pickSelRef.current = typeof v === "number" ? v : v(pickSelRef.current);
    setPickSel(pickSelRef.current);
    pickSelIdRef.current = treeRowsRef.current[pickSelRef.current]?.s.id ?? null;
  }, []);
  const [filter, setFilter] = useState("");
  const [filterActive, setFilterActive] = useState(false);
  // Deprecated branches (and, in the sessions tab, archived sessions) are hidden
  // by default in both trees; `h` reveals them.
  const [showDeprecated, setShowDeprecated] = useState(false);
  // ^x archive confirm (double-tap, like ctrl+c quit): first press arms this.
  const archiveArm = useRef({ id: "", at: 0 });
  // composer history (sent messages, oldest first, persisted across runs);
  // null idx = editing the draft
  const history = useRef<string[]>(loadHistory());
  const [histIdx, setHistIdx] = useState<number | null>(null);
  const draft = useRef("");
  // panel state
  const [panelTab, setPanelTab] = useState<PanelTab>("sessions");
  // Jobs tab: list cursor, the drilled-into job (null = list), its fetched
  // buffer, and the output scroll offset.
  const [jobSel, setJobSel] = useState(0);
  const [jobOpen, setJobOpen] = useState<string | null>(null);
  const [jobText, setJobText] = useState<string | null>(null);
  const [jobScroll, setJobScroll] = useState(0);
  const [mcpSel, setMcpSel] = useState(0);
  const [panelMsg, setPanelMsg] = useState<string | null>(null);
  // Workflows tab: run-list cursor, drill level (runs → phases → that phase's
  // agents → one agent), the opened run's fetched detail (journal rows), the two
  // pane cursors, the `f` status filter, and the detail's scroll/prompt fold.
  const [wfSel, setWfSel] = useState(0);
  const [wfLevel, setWfLevel] = useState<WfLevel>(0);
  const [wfOpenId, setWfOpenId] = useState<string | null>(null);
  const [wfDetail, setWfDetail] = useState<
    { run: WireWorkflowRun; agents: WfAgentView[] } | null
  >(null);
  const [wfPhaseSel, setWfPhaseSel] = useState(0);
  const [wfAgentSel, setWfAgentSel] = useState(0);
  const [wfScroll, setWfScroll] = useState(0);
  const [wfFilter, setWfFilter] = useState<WfFilter>(null);
  const [wfPromptOpen, setWfPromptOpen] = useState(false);
  // The selected phase's agents after `f` — BOTH panes and every key that acts on
  // "the selected agent" index this same list, so a filtered view can never act
  // on a row it isn't showing.
  const wfGroups = wfDetail ? phaseGroups(wfDetail.run, wfDetail.agents) : [];
  const wfGroup = wfGroups[Math.min(wfPhaseSel, Math.max(0, wfGroups.length - 1))];
  const wfAgents = visibleAgents(wfGroup?.agents ?? [], wfFilter);
  // Refetch the opened run's detail on every workflow.* event (store.wfSeq bumps).
  useEffect(() => {
    if (mode !== "panel" || panelTab !== "workflows" || !wfOpenId) return;
    let dead = false;
    api.getWorkflow(wfOpenId).then(
      (d) => {
        if (!dead) setWfDetail({ run: d.workflow, agents: d.agents });
      },
      () => {},
    );
    return () => {
      dead = true;
    };
  }, [mode, panelTab, wfOpenId, store.wfSeq]);
  // Refetch the opened job's buffer. store.jobs already re-polls every 2s while
  // anything runs, so keying off it makes the output view tail a live job for
  // free and settle the moment it exits.
  useEffect(() => {
    if (mode !== "panel" || panelTab !== "jobs" || !jobOpen || !store.currentId) return;
    let dead = false;
    api.jobOutput(store.currentId, jobOpen).then(
      (r) => {
        if (!dead) setJobText(r.output);
      },
      () => {
        if (!dead) setJobText("(output unavailable — the job aged out of the registry)");
      },
    );
    return () => {
      dead = true;
    };
  }, [mode, panelTab, jobOpen, store.currentId, store.jobs]);
  // ask() question-hold state: the free-text draft + whether the text line is
  // active. Reset per question; an option-less question starts in typing mode —
  // free text is its only input. An arm-delay (askSince) so an in-flight composer
  // keystroke can't answer unseen.
  const askId = store.ask?.id;
  const askHasOptions = (store.ask?.options?.length ?? 0) > 0;
  const [askText, setAskText] = useState("");
  const [askTyping, setAskTyping] = useState(false);
  const askSince = useRef(0);
  useEffect(() => {
    setAskText("");
    setAskTyping(!askHasOptions);
    if (askId) askSince.current = Date.now();
  }, [askId, askHasOptions]);
  // Double-esc detection (Claude Code parity): only an esc that did nothing on
  // its own arms the pair — an esc that interrupted or dismissed something
  // already spent itself.
  const lastEscAt = useRef(0);
  const [mcpStat, setMcpStat] = useState<McpStatus | null>(null);
  const [skillsList, setSkillsList] = useState<SkillInfo[] | null>(null);
  // new-session state. The workspace query is a cursor-ed line edit like the
  // composer (^u appending instead of clearing was a triple-repro'd user bug).
  const [newComp, setNewComp] = useState({ text: "", cursor: 0 });
  const newQuery = newComp.text;
  const [newSel, setNewSel] = useState(0);
  const [dirHits, setDirHits] = useState<DirHit[]>([]);
  // conversation-tree state: cursor + a range-selection anchor (v starts/ends it).
  // Same ref-mirror as pickSel (same one-batch stale-read hazard), plus a "user
  // took over" flag so the cursor follows late-loading rows only until then.
  const [forkSel, setForkSel] = useState(0);
  const forkSelRef = useRef(0);
  const moveForkSel = useCallback((v: number | ((i: number) => number)) => {
    // Same synchronous-ref discipline as movePickSel above.
    forkSelRef.current = typeof v === "number" ? v : v(forkSelRef.current);
    setForkSel(forkSelRef.current);
  }, []);
  const forkNavTouched = useRef(false);
  const [rangeAnchor, setRangeAnchor] = useState<number | null>(null);
  // LLM-labeled activity sections over the tree's turns (s toggles; null = off).
  const [sections, setSections] = useState<WireSection[] | null>(null);
  // Turn ids pending a move: set by `m`, consumed by picking a destination on the
  // sessions tab (Enter appends there instead of opening).
  const [movePicks, setMovePicks] = useState<string[] | null>(null);
  // diff state
  const [fileSel, setFileSel] = useState(0);
  const [diffScroll, setDiffScroll] = useState(0);
  // Focused review: hide the file list so the selected file's hunks get the full
  // panel height. Enter/→ enters, ←/Esc leaves (Esc via the panel-close handler).
  const [diffFocus, setDiffFocus] = useState(false);
  // model state
  const [cfg, setCfg] = useState<BoughConfig | null>(null);
  const [modelSel, setModelSel] = useState(0);
  const [keyInput, setKeyInput] = useState<string | null>(null); // masked API-key entry
  // theme tab: cursor + auto-apply. Moving the cursor applies the hovered preset
  // (debounced — rapid arrows shouldn't race PUT/GET chains out of order).
  const [themeState, setThemeState] = useState<ThemeState | null>(null);
  const [themeSel, setThemeSel] = useState(0);
  const themeSelRef = useRef(0);
  const themeApplyTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  // The theme held on tab entry (or last Enter-confirmed): browsing previews
  // live, but Escape puts this one back. null = default; undefined = unknown.
  const themeEntryRef = useRef<ThemeState["theme"] | undefined>(undefined);
  const applyPreset = useCallback((idx: number) => {
    const p = THEME_PRESETS[idx];
    if (!p) return;
    (p.name === "Default" ? api.resetTheme() : api.putTheme(p.name, p.colors))
      .then(() => api.getTheme())
      .then((next) => {
        applyTheme(next); // the running TUI recolors now
        setThemeState(next);
      })
      .catch((e) => setPanelMsg(String(e)));
  }, []);
  // Escape from the theme tab: re-apply the entry (or Enter-confirmed) theme so
  // browsing never commits. Unconditional re-PUT — a debounced preview may still
  // be in flight, so "current == entry" can't be trusted.
  const revertTheme = useCallback(() => {
    if (themeApplyTimer.current) clearTimeout(themeApplyTimer.current);
    const entry = themeEntryRef.current;
    if (entry === undefined) return;
    (entry === null ? api.resetTheme() : api.putTheme(entry.name, entry.colors))
      .then(() => api.getTheme())
      .then((next) => {
        applyTheme(next);
        setThemeState(next);
      })
      .catch((e) => setPanelMsg(String(e)));
  }, []);
  const moveThemeSel = useCallback((v: (i: number) => number) => {
    themeSelRef.current = Math.max(
      0,
      Math.min(THEME_PRESETS.length - 1, v(themeSelRef.current)),
    );
    setThemeSel(themeSelRef.current);
    if (themeApplyTimer.current) clearTimeout(themeApplyTimer.current);
    themeApplyTimer.current = setTimeout(() => applyPreset(themeSelRef.current), 120);
  }, [applyPreset]);

  const { open } = store;
  // Per-session drafts: the composer buffer belongs to the conversation it was
  // typed in. On the way out of a session, stash its text server-side
  // (session.draft — the same column handoff prefills from) and clear the
  // buffer; the incoming session's own draft prefills via the effect below.
  const compRef = useRef(comp);
  compRef.current = comp;
  const currentIdRef = useRef<string | null>(null);
  currentIdRef.current = store.currentId;
  const stashDraft = useCallback(() => {
    const from = currentIdRef.current;
    const text = compRef.current.text;
    if (from) api.putDraft(from, text.trim() ? text : null).catch(() => {});
    else if (text.trim()) {
      // The launch draft has no session row to carry it — keep it recallable (↑).
      history.current.push(text);
      appendHistory(text);
    }
    setComp({ text: "", cursor: 0 });
    setHistIdx(null);
    draft.current = "";
  }, []);
  const openSession = useCallback((s: Session) => {
    setErr(null);
    setShowInfo(false);
    setMode("chat");
    setFilter("");
    setFilterActive(false);
    setScrollOff(0);
    setToggled(new Set());
    setExpandAll(false);
    setSearchQ(null);
    setShellOut(null);
    setSections(null); // labels describe the session they were computed for
    setRailSel(null); // the rail lists the session we're leaving
    if (currentIdRef.current !== s.id) stashDraft();
    open(s.id).catch((e) => setErr(String(e)));
  }, [open, stashDraft]);

  // The spawner of the open session, when it's a subagent branch (else null).
  // Peeking into a running subagent must be reversible in one key, and — the trap
  // this closes — without interrupting the subagent's turn (bare esc used to).
  const spawnerSession = useCallback((): Session | null => {
    const cur = store.session;
    if (cur?.kind !== "subagent" || !cur.originId) return null;
    return store.sessions.find((s) => s.id === cur.originId) ?? null;
  }, [store.session, store.sessions]);

  // A handoff draft prefills an EMPTY composer when its session opens (review,
  // edit, send); the server clears it on the first post. Never clobbers typed text.
  const sessionDraft = store.session?.draft ?? null;
  const sessionIdForDraft = store.session?.id;
  useEffect(() => {
    if (!sessionDraft) return;
    setComp((c) => (c.text.trim() ? c : { text: sessionDraft, cursor: sessionDraft.length }));
  }, [sessionIdForDraft, sessionDraft]);

  // Launch = a fresh draft targeting the caller's cwd; the session is created on
  // the first send. ^p resumes existing sessions.

  // Config once at startup so the status bar can name the active model (^o keeps
  // it fresh after switches; there's no config event to subscribe to).
  useEffect(() => {
    api.getConfig().then(setCfg, () => {});
  }, []);

  // Terminal chrome: the tab is named after the open conversation, tinted amber
  // (iTerm2) while an approval waits, and shows taskbar/tab progress while the
  // turn runs — all no-ops on terminals without the respective sequence.
  const sessionTitle = store.session?.title;
  useEffect(() => {
    setTitle(sessionTitle ? `bough — ${sessionTitle}` : "bough");
  }, [sessionTitle]);
  const hasPending = !!store.ask;
  useEffect(() => {
    tabColor(hasPending ? palette.warn : null);
  }, [hasPending]);
  // turnErrored rides in a ref: the effect must fire on busy EDGES only (an
  // error status landing later must not re-clear an already-cleared progress).
  const turnErroredRef = useRef(false);
  turnErroredRef.current = store.session?.lastTurnStatus === "error";
  useEffect(() => {
    if (store.busy) progressStart();
    else progressEnd(turnErroredRef.current);
  }, [store.busy]);

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
    if (tab === "mcp") {
      api.mcpStatus(currentId).then(setMcpStat, (e) => {
        setMcpStat(null);
        setPanelMsg(String(e));
      });
    } else if (tab === "skills") {
      api.skills().then(setSkillsList, () => setSkillsList([]));
    } else if (tab === "model") {
      // Refresh without nulling first — cfg also feeds the status-bar model chip.
      api.getConfig().then(setCfg, (e) => setErr(String(e)));
    } else if (tab === "theme") {
      // Fresh state + cursor onto the current theme, so the first arrow move
      // steps (and auto-applies) from where the user actually is.
      api.getTheme().then((t) => {
        setThemeState(t);
        themeEntryRef.current = t.theme; // what Escape restores
        const cur = t.theme?.name ?? "Default";
        const idx = Math.max(0, THEME_PRESETS.findIndex((p) => p.name === cur));
        themeSelRef.current = idx;
        setThemeSel(idx);
      }, () => {
        setThemeState(null);
        themeEntryRef.current = undefined;
      });
    }
    // sessions / conversation read from the store — nothing to fetch.
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
  // Help-overlay scroll offset (short terminals) — the max lives in a ref so
  // the stable mouse subscription can clamp wheel scrolling without resubscribing.
  const [helpScroll, setHelpScroll] = useState(0);
  const helpMaxScrollRef = useRef(0);
  helpMaxScrollRef.current = helpMaxScroll(rows, width);

  // ---- the conversation viewport ------------------------------------------
  // Tool groups stay COLLAPSED by default — even the running turn's trailing
  // group. Expansion is explicit (click a header, click the activity line, or
  // ^e expand-all). The collapsed header keeps the gist + ⚙ running tag, and
  // the activity line streams a live running summary, so live output is legible
  // without dumping every card open.
  const expandAllRef = useRef(expandAll);
  expandAllRef.current = expandAll;
  const isExpanded = useCallback(
    (key: string) => expandAll !== toggled.has(key),
    [expandAll, toggled],
  );
  // "Show all N lines" state for truncated blocks ("<groupKey>!full" entries in
  // the same toggled set). Plain membership, no expandAll XOR — ^e expand-all
  // must not dump every long output into the viewport.
  const isFull = useCallback(
    (key: string) => toggled.has(`${key}!full`),
    [toggled],
  );
  // Subagents spawned by the open session, each anchored to its spawning turn.
  // Its completion note (a system message in the thread) supplies the report; the
  // branch card is drawn under originMessageId and the raw note is suppressed.
  const noteById = useMemo(() => {
    const m = new Map<string, SubagentNote>();
    for (const msg of store.thread) {
      if (msg.role !== "system") continue;
      const t = msg.parts.filter((p) => p.type === "text").map((p) => (p as { text: string }).text)
        .join("\n");
      const note = parseSubagentNote(t);
      if (note) m.set(note.sessionId, note);
    }
    return m;
  }, [store.thread]);
  const branches = useMemo<Branch[]>(
    () =>
      store.sessions
        .filter((s) => s.kind === "subagent" && s.originId === currentId)
        .map((s) => ({
          id: s.id,
          title: s.title || "(untitled)",
          busy: !!s.busy,
          status: s.lastTurnStatus,
          ok: s.outcomeOk,
          checkPassed: s.outcomeCheckPassed,
          originMessageId: s.originMessageId,
          note: noteById.get(s.id) ?? null,
        })),
    [store.sessions, currentId, noteById],
  );
  // The rail's cursor must not dangle past a list that shrank (or emptied).
  useEffect(() => {
    setRailSel((s) =>
      s === null || branches.length === 0 ? null : Math.min(s, branches.length - 1)
    );
  }, [branches.length]);
  // Background shells: cards render only the open session's own jobs (a
  // subagent's jobs show inside its branch); the status-bar chip counts all.
  const ownJobs = useMemo(
    () => store.jobs.filter((j) => j.sessionId === currentId),
    [store.jobs, currentId],
  );
  const runningJobs = useMemo(
    () => store.jobs.filter((j) => j.status === "running").length,
    [store.jobs],
  );
  const lines = useMemo(
    () =>
      buildLines(
        store.thread,
        store.streaming,
        isExpanded,
        isFull,
        width,
        branches,
        store.toolLogs,
        ownJobs,
      ),
    // palette.epoch: an applied theme must recolor the pre-rendered SGR lines.
    [
      store.thread,
      store.streaming,
      store.toolLogs,
      isExpanded,
      isFull,
      width,
      branches,
      ownJobs,
      palette.epoch,
    ],
  );
  // Search matches over the current lines; the index clamps as lines rebuild
  // (streaming appends, folds toggling) so the counter never dangles.
  const matches = useMemo(
    () => (searchQ ? findMatches(lines, searchQ) : []),
    [lines, searchQ],
  );
  const curMatch = matches.length ? Math.min(searchIdx, matches.length - 1) : -1;
  const toggleGroup = useCallback((key: string) => {
    // A group's expanded state is the XOR of expand-all and its membership in
    // `toggled`, so flipping membership toggles the card either way.
    setToggled((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }, []);

  // Tool cards are CLOSED by default and stay closed while a turn runs. The
  // collapsed header keeps the gist + status tag, and anything the user must
  // act on (an artifact link, an answer) belongs in the reply prose, not
  // inside a fold.

  // Clicking the activity line ("⠴ running the test suite…") opens the fold it
  // describes: the running turn's trailing tool group. Ref'd so the mouse
  // subscription stays stable.
  const toggleRunningGroupRef = useRef<() => void>(() => {});
  toggleRunningGroupRef.current = () => {
    const pending = [...store.thread].reverse().find((m) => m.pending && m.role === "supervisor");
    if (!pending) return;
    const segs = segmentParts(pending.parts);
    for (let i = segs.length - 1; i >= 0; i--) {
      if (segs[i].kind === "tools") {
        toggleGroup(`${pending.id}:${i}`);
        return;
      }
    }
  };

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
  // While scrolled away from the bottom, anchor the viewport to CONTENT, not
  // the bottom: scrollOff counts lines up from the end, so transcript growth
  // (streaming) dragged the reading position down, and collapsing a fold
  // yanked the view away from its header. Absorb the length delta during
  // render (React's derived-state pattern) so a drifted frame is never
  // painted; at the bottom (scrollOff 0) the viewport keeps following output.
  const prevLineCount = useRef(lines.length);
  if (lines.length !== prevLineCount.current) {
    const delta = lines.length - prevLineCount.current;
    prevLineCount.current = lines.length;
    if (scrollOff > 0) {
      const next = Math.max(0, Math.min(scrollOff + delta, Math.max(0, lines.length - bodyH)));
      if (next !== scrollOff) setScrollOff(next);
    }
  }
  const maxOff = Math.max(0, lines.length - bodyH);
  const off = Math.min(scrollOff, maxOff);
  // Re-anchor the stored offset when the transcript shrinks (collapsing a long
  // fold): a stale scrollOff far above maxOff made PgDn presses no-ops until the
  // excess burned off — scrolling felt dead/non-monotonic (user-testing).
  useEffect(() => {
    setScrollOff((o) => Math.min(o, maxOff));
  }, [maxOff]);
  // Keep the current search match centered in the viewport — including as the
  // transcript grows under it (streaming) or the match set changes with the query.
  useEffect(() => {
    if (curMatch < 0) return;
    const line = matches[curMatch].line;
    setScrollOff(Math.max(0, Math.min(maxOff, lines.length - line - Math.ceil(bodyH / 2))));
  }, [curMatch, matches, lines.length, maxOff, bodyH]);
  const start = Math.max(0, lines.length - bodyH - off);
  const visible = lines.slice(start, start + bodyH);
  const padTop = bodyH - visible.length;

  // Everything a mouse event needs from the current layout. Synced in an effect
  // (post-commit) rather than during render, so a click always maps against the
  // frame actually ON SCREEN — chromeH is measured a frame late, so the padTop
  // used during an in-flight render can differ from what's painted by one row.
  const layout = useRef({ mode, start, padTop, maxOff, bodyH, lines });
  useEffect(() => {
    layout.current = { mode, start, padTop, maxOff, bodyH, lines };
  });
  // Drag selection over the viewport: mouse-down arms it, dragging highlights,
  // release copies the plain text. State drives the highlight render; the refs
  // keep the (stable) mouse subscription in sync.
  const [drag, setDrag] = useState<Selection | null>(null);
  const dragRef = useRef<Selection | null>(null);
  const pressRef = useRef<{ x: number; y: number } | null>(null);
  useEffect(() => {
    if (mode !== "chat") {
      setDrag(null);
      dragRef.current = null;
      pressRef.current = null;
    }
  }, [mode]);
  // Screen row (0-based) of the activity line, synced post-render below (its
  // position depends on chrome pieces declared later); null when hidden.
  const activityRowRef = useRef<number | null>(null);
  // The /conversation card's clickable region: screen row of its first data row
  // plus the rows themselves. Synced post-render alongside activityRowRef.
  const infoClickRef = useRef<{ firstRow: number; rows: [string, string, string][] } | null>(null);
  // Composer autocomplete: "/" at the start of a line completes skills, "@" completes
  // workspace files (in a draft, the prospective workspace's files by path),
  // "!" fuzzy-searches shell history backwards (ctrl-r muscle memory). The "/"
  // skill picker triggers at the start of any line (after the very first char or
  // following a newline), so it works mid-multiline-input — not only when "/"
  // is the absolute first character of the whole composer (the @ picker already
  // worked mid-line; the / picker was pinned to text startsWith, which made the
  // fzf vanish as soon as anything preceded it on the line).
  interface Popup {
    kind: "skill" | "file" | "shell";
    /** hl: label indices of the fuzzy-matched chars (accent+bold in the row). */
    items: { label: string; detail: string; insert: string; hl?: number[] }[];
    sel: number;
    tokenStart: number;
    tokenEnd: number;
  }
  const [popup, setPopup] = useState<Popup | null>(null);
  const skillsCache = useRef<SkillInfo[] | null>(null);
  // The `!` corpus (own runs + seeded shell files), read once per run; new runs
  // are appended in place so the popup stays fresh without re-reading files.
  const shellHist = useRef<string[] | null>(null);
  useEffect(() => {
    if (mode !== "chat") {
      setPopup(null);
      return;
    }
    const { text, cursor } = comp;
    dbg?.(`popup-effect text=${JSON.stringify(text)} cursor=${cursor} currentId=${currentId}`);
    const end = (() => {
      const ws = text.slice(cursor).search(/\s/);
      return ws < 0 ? text.length : cursor + ws;
    })();
    if (text.startsWith("!") && cursor >= 2) {
      // Backwards fzf over shell history: fuzzy filter, most recent first.
      // Requires at least one char after `!` — a bare `!` popping the whole
      // corpus buried the shell-mode label under an unbidden dropdown.
      // Completing replaces the whole input — a `!` line IS the command.
      const q = text.slice(1, cursor);
      const corpus = shellHist.current ??= shellHistoryCorpus();
      const items = corpus
        .map((cmd, i) => ({ cmd, i, score: fuzzyScore(cmd, q) }))
        .filter((x) => x.score > 0)
        .sort((a, b) => b.score - a.score || b.i - a.i)
        .slice(0, 6)
        .map(({ cmd }) => ({
          label: cmd,
          detail: "",
          insert: `!${cmd}`,
          hl: fuzzyPositions(cmd, q),
        }));
      // sel -1 = browsing, nothing picked: enter keeps running the TYPED line;
      // only an explicit ↑/↓ pick makes enter run a listed command instead.
      setPopup(
        items.length
          ? { kind: "shell", items, sel: -1, tokenStart: 0, tokenEnd: text.length }
          : null,
      );
      return;
    }
    // "/" at a word boundary (position 0 or after whitespace) arms the skill
    // picker — same rule the @ file picker uses, so "foo /commit" autocompletes
    // just like "foo @file" does. A "/" mid-word (e.g. a/path/b) is not a
    // command prefix and must not eat the token.
    const slashAt = text.lastIndexOf("/", cursor - 1);
    if (
      slashAt >= 0 && !/\s/.test(text.slice(slashAt + 1, cursor)) &&
      (slashAt === 0 || /\s/.test(text[slashAt - 1]))
    ) {
      const q = text.slice(slashAt + 1, cursor);
      const apply = (skills: SkillInfo[]) => {
        // Composer-local commands complete alongside server skills. The server's
        // `theme` skill is hidden here — the local /theme opens the panel's
        // theme tab instead.
        const local: SkillInfo[] = [
          { name: "handoff", description: "draft a fresh conversation focused on a goal" },
          { name: "conversation", description: "show this conversation's id and details" },
          { name: "schedule", description: "recurring agent runs — list, toggle, create" },
          { name: "workflows", description: "workflow runs — watch, stop, pause, rerun" },
          { name: "theme", description: "pick a color theme — opens the theme tab" },
          { name: "model", description: "switch the model — opens the model panel" },
          { name: "effort", description: "set thinking depth — opens the model panel" },
        ];
        const items = [...local, ...skills.filter((s) => s.name !== "theme")]
          .map((s, i) => ({ s, local: i < local.length, score: fuzzyScore(s.name, q) }))
          .filter((x) => x.score > 0)
          .sort((a, b) =>
            b.score - a.score || Number(b.local) - Number(a.local) ||
            a.s.name.length - b.s.name.length ||
            a.s.name.localeCompare(b.s.name)
          )
          .slice(0, 6)
          .map(({ s }) => ({
            label: `/${s.name}`,
            detail: popupDetail(s.description),
            insert: `/${s.name} `,
          }));
        dbg?.(`skill-popup q=${JSON.stringify(q)} items=${items.length}`);
        // A filter that matches nothing still shows the menu (as a "no matching
        // commands" row) — silently hiding it read as "/ is broken".
        setPopup(
          items.length || q
            ? { kind: "skill", items, sel: 0, tokenStart: slashAt, tokenEnd: end }
            : null,
        );
      };
      if (skillsCache.current) apply(skillsCache.current);
      else {
        api.skills().then(
          (s) => (skillsCache.current = s, apply(s)),
          (e) => dbg?.(`skills fetch FAILED: ${e}`),
        );
      }
      return;
    }
    const at = text.lastIndexOf("@", cursor - 1);
    if (
      at >= 0 && !/\s/.test(text.slice(at + 1, cursor)) &&
      (at === 0 || /\s/.test(text[at - 1]))
    ) {
      const q = text.slice(at + 1, cursor);
      const t = setTimeout(() => {
        const search = currentId
          ? api.searchFiles(currentId, q)
          : api.searchDraftFiles(defaultWorkspace, q);
        search.then((files) => {
          const items = files.slice(0, 6).map((f) => ({
            label: `@${f}`,
            detail: f.endsWith("/") ? "dir" : "",
            insert: `@${f} `,
            // Positions are vs the path; the label's leading "@" shifts them by 1.
            hl: fuzzyPositions(f, q).map((p) => p + 1),
          }));
          setPopup(
            items.length ? { kind: "file", items, sel: 0, tokenStart: at, tokenEnd: end } : null,
          );
        }, () => setPopup(null));
      }, 120);
      return () => clearTimeout(t);
    }
    setPopup(null);
  }, [comp, mode, currentId, defaultWorkspace]);

  // Ghost text: a dim inline preview after the cursor, accepted with tab. Two
  // sources — the open popup's selected item (previewing what tab would insert)
  // and, with no popup, a worker prediction of the user's NEXT message from the
  // conversation (fetched when a turn ends, shown on the idle composer). A ghost
  // only shows with the cursor at end-of-input; typing through it keeps the
  // remainder, diverging hides it.
  const [ghost, setGhost] = useState<string | null>(null);
  const suggestSeq = useRef(0);
  const ghostText = (() => {
    if (mode !== "chat" || comp.cursor !== comp.text.length) return null;
    if (popup) {
      const it = popup.items[popup.sel];
      if (!it) return null;
      const completed = comp.text.slice(0, popup.tokenStart) + it.insert;
      if (!completed.startsWith(comp.text)) return null;
      return completed.slice(comp.text.length).trimEnd() || null;
    }
    // Only surface the worker prediction once the user has started typing —
    // on an empty composer it clobbered the guiding placeholder with a whole
    // sentence. Typing a prefix of it still reveals the remainder.
    if (!ghost || comp.text === "" || store.busy || store.ask) return null;
    return ghost.startsWith(comp.text) && ghost.length > comp.text.length
      ? ghost.slice(comp.text.length)
      : null;
  })();
  // Fetch on the idle edge (turn end, session open). Typing must NOT re-run this
  // — comp is read through compRef (declared above) so a ghost survives being
  // typed through.
  useEffect(() => {
    ++suggestSeq.current; // context changed — invalidate any in-flight fetch
    setGhost(null); //     …and the prediction it would have refreshed
    if (mode !== "chat" || !currentId || store.busy || store.ask) return;
    const sid = currentId;
    const seq = suggestSeq.current;
    // A beat after idle so the turn's final message is committed server-side.
    const t = setTimeout(() => {
      if (compRef.current.text !== "") return; // mid-draft: not ours to finish
      api.suggest(sid).then(
        (s) => {
          if (s && suggestSeq.current === seq) setGhost(s);
        },
        () => {}, // no suggestion is fine; never surface an error for sugar
      );
    }, 400);
    return () => clearTimeout(t);
  }, [mode, currentId, store.busy, store.ask]);

  // Bracketed pastes land whole in the composer (chat mode only), newlines intact.
  const modeRef = useRef(mode);
  modeRef.current = mode;
  useEffect(() => {
    onPaste((text) => {
      if (modeRef.current === "chat") insertAtCursor(text);
    });
    return () => onPaste(null);
  }, [insertAtCursor]);

  // Physical Home/End and Cmd+←/→: ink drops Home/End sequences, and on
  // terminals without the kitty keyboard protocol it misparses Cmd+←/→ as
  // meta+arrow — so mouse.ts intercepts both and dispatches them here.
  // Ref-current like copyRef so the subscription stays stable.
  const navKeyRef = useRef<(k: "home" | "end" | "cmdHome" | "cmdEnd") => void>(() => {});
  navKeyRef.current = (k) => {
    if (mode === "new") {
      return setNewComp((c) => ({
        ...c,
        cursor: k === "home" || k === "cmdHome" ? 0 : c.text.length,
      }));
    }
    if (mode !== "chat" || store.ask || searchQ !== null || sched) return;
    if (k === "cmdHome") {
      // Cmd+← jumps to the start of the current line (multiline-aware).
      return moveCursor((c) => {
        const nl = c.text.lastIndexOf("\n", c.cursor - 1);
        return nl < 0 ? 0 : nl + 1;
      });
    }
    if (k === "cmdEnd") {
      // Cmd+→ jumps to the end of the current line (multiline-aware).
      return moveCursor((c) => {
        const nl = c.text.indexOf("\n", c.cursor);
        return nl < 0 ? c.text.length : nl;
      });
    }
    moveCursor((c) => (k === "home" ? 0 : c.text.length));
  };
  useEffect(() => {
    onNavKey((k) => navKeyRef.current(k));
    return () => onNavKey(null);
  }, []);

  // Copy feedback is a blinking overlay chip over the bottom viewport row — it
  // must not take a layout row of its own (a toast above the status bar changes
  // the measured chrome height and shoves the whole transcript up).
  const [flash, setFlash] = useState<{ msg: string; on: boolean } | null>(null);
  const flashTimers = useRef<ReturnType<typeof setTimeout>[]>([]);
  const flashMsg = useCallback((msg: string) => {
    flashTimers.current.forEach(clearTimeout);
    setFlash({ msg, on: true });
    // Three blinks (~1.5s), then gone.
    const seq: Array<[number, { msg: string; on: boolean } | null]> = [
      [350, { msg, on: false }],
      [500, { msg, on: true }],
      [850, { msg, on: false }],
      [1000, { msg, on: true }],
      [1500, null],
    ];
    flashTimers.current = seq.map(([ms, v]) => setTimeout(() => setFlash(v), ms));
  }, []);
  useEffect(() => () => flashTimers.current.forEach(clearTimeout), []);
  // A background session finishing while you're looking elsewhere is otherwise
  // invisible (the desktop banner self-gates on focus, the picker dot needs ^p).
  useEffect(() => {
    if (store.bgFinish) flashMsg(`✓ ${store.bgFinish.title} finished`);
  }, [store.bgFinish, flashMsg]);

  // Copy handler, kept in a ref so the mouse subscription stays stable.
  // Info-card rows copy on any click; everything else copies on right-click.
  const copyRef = useRef<(text: string, label: string) => void>(() => {});
  copyRef.current = (text, label) => {
    copyToClipboard(text).then(
      () => flashMsg(`✓ copied ${label}`),
      () => flashMsg("✗ copy failed (pbcopy)"),
    );
  };
  // The running turn's activity blurb, mirrored for the right-click handler.
  const activityTextRef = useRef<string | null>(null);
  activityTextRef.current = store.activity;

  // A click key is either a tool-group fold or "open:<sessionId>" (descend into a
  // subagent branch). Kept in a ref so the mouse subscription stays stable.
  const onClickRef = useRef<(key: string) => void>(() => {});
  onClickRef.current = (key) => {
    if (key.startsWith("open:")) {
      const s = store.sessions.find((x) => x.id === key.slice(5));
      if (s) openSession(s);
      return;
    }
    // Clicking a background-job card jumps to the jobs tab — the card is a
    // summary, the tab is where the whole output and the kill live.
    if (key === "jobs") return openTab("jobs");
    toggleGroup(key);
  };
  useEffect(() => {
    // Click/right-click dispatch at a screen cell (chrome rows first, then the
    // viewport line under it). Left clicks land on mouse-UP so they can be told
    // apart from a starting drag.
    const clickAt = (x: number, y: number, right: boolean) => {
      const l = layout.current;
      // Info-card rows live in the chrome: any click (left or right) copies the
      // row's raw value.
      const info = infoClickRef.current;
      if (info) {
        const rel = y - 1 - info.firstRow;
        if (rel >= 0 && rel < info.rows.length) {
          const [label, , value] = info.rows[rel];
          copyRef.current(value, label);
          return;
        }
      }
      // The activity line also lives in the chrome: left-click expands/collapses
      // the running tool group it summarizes, right-click copies the blurb.
      if (activityRowRef.current !== null && y - 1 === activityRowRef.current) {
        if (right) {
          if (activityTextRef.current) copyRef.current(activityTextRef.current, "activity");
        } else toggleRunningGroupRef.current();
        return;
      }
      // Only rows inside the painted viewport map to lines — a click on the
      // chrome (composer, cards, status bar) while scrolled up must not toggle
      // an off-screen fold.
      const rel = (y - 1) - l.padTop;
      if (rel < 0 || rel >= l.bodyH) return;
      const line = l.lines[l.start + rel];
      if (!line) return;
      // Right-click copies the raw text of the section the line belongs to;
      // left-click keeps its fold/open behavior.
      if (right) {
        if (line.copy) copyRef.current(line.copy, "section");
        return;
      }
      // A click landing on a hyperlink opens it — the TUI owns the mouse, so
      // the terminal's own cmd+click never fires without a shift bypass.
      const url = linkAt(line.text, x - 1);
      if (url && /^https?:\/\//.test(url)) {
        new Deno.Command("open", { args: [url], stdout: "null", stderr: "null" }).spawn();
        return;
      }
      if (line.click) onClickRef.current(line.click);
    };
    onMouse((ev: MouseEvent) => {
      const l = layout.current;
      if (ev.kind === "wheel-up") {
        if (l.mode === "help") {
          setHelpScroll((o) => Math.max(0, o - 3));
          return;
        }
        setScrollOff((o) => Math.min(l.maxOff, o + 3));
        return;
      }
      if (ev.kind === "wheel-down") {
        if (l.mode === "help") {
          setHelpScroll((o) => Math.min(helpMaxScrollRef.current, o + 3));
          return;
        }
        setScrollOff((o) => Math.max(0, o - 3));
        return;
      }
      if (l.mode !== "chat") return;
      if (ev.kind === "down") {
        pressRef.current = { x: ev.x, y: ev.y };
        return;
      }
      if (ev.kind === "drag") {
        const p = pressRef.current;
        if (!p) return;
        // Drags starting on the chrome (composer, status bar) aren't selections.
        const rel = p.y - 1 - l.padTop;
        if (rel < 0 || rel >= l.bodyH) return;
        const next = { anchor: p, focus: { x: ev.x, y: ev.y } };
        dragRef.current = next;
        setDrag(next);
        return;
      }
      if (ev.kind === "up") {
        const sel = dragRef.current;
        const press = pressRef.current;
        pressRef.current = null;
        if (sel) {
          dragRef.current = null;
          setDrag(null);
          // Rows top to bottom, each clipped to its selected span; skip rows
          // outside the painted viewport.
          const [y1, y2] = selRows(sel);
          const rows: string[] = [];
          for (let y = y1; y <= y2; y++) {
            const rel = y - 1 - l.padTop;
            if (rel < 0 || rel >= l.bodyH) continue;
            const line = l.lines[l.start + rel];
            if (!line) continue;
            const span = rowSpan(sel, y);
            if (span) rows.push(extractSpan(line.text, span.from, span.to));
          }
          const text = rows.join("\n").replace(/^\n+|\n+$/g, "");
          if (text) copyRef.current(text, "selection");
          return;
        }
        if (press) clickAt(press.x, press.y, false);
        return;
      }
      if (ev.kind === "right-click") clickAt(ev.x, ev.y, true);
    });
    return () => onMouse(null);
  }, []);

  // Deprecated branches (and archived sessions) are hidden from the picker
  // unless revealed with `h`.
  const pickerSessions = useMemo(
    () =>
      showDeprecated
        ? [
          ...store.sessions,
          ...store.archived.filter((a) => !store.sessions.some((s) => s.id === a.id)),
        ]
        : store.sessions.filter((s) => !s.deprecatedAt),
    [store.sessions, store.archived, showDeprecated],
  );
  // Memoized so its reference is stable across unrelated re-renders (the 1s age
  // clock below), which keeps the re-anchor effect from firing every render.
  const treeRows: TreeRow[] = useMemo(
    () =>
      filter
        ? flattenTree(pickerSessions)
          .filter(({ s }) => (s.title || "").toLowerCase().includes(filter.toLowerCase()))
          .map(({ s }) => ({ s, depth: 0, prefix: "" }))
        : flattenTree(pickerSessions),
    [pickerSessions, filter],
  );
  treeRowsRef.current = treeRows;
  // Keep the picker cursor on the SAME session when the row set changes: a fork
  // or subagent streaming in (store prepends) or a reload reordering rows must
  // not slide the cursor onto a different session. While filtering, the cursor
  // stays pinned to the top match instead (the keystroke handlers own that).
  useEffect(() => {
    if (filter) return;
    const wantId = pickSelIdRef.current;
    if (wantId === null) return;
    const idx = treeRows.findIndex((r) => r.s.id === wantId);
    if (idx >= 0) {
      if (idx !== pickSelRef.current) movePickSel(idx);
    } else {
      // The anchored session vanished (archived/deprecated) — clamp and re-seed.
      movePickSel((i) => Math.min(i, Math.max(0, treeRows.length - 1)));
    }
  }, [treeRows, filter, movePickSel]);

  // The sessions tab's per-row age column only moves when something re-renders,
  // so a working session read as stalled between events. While the panel is open
  // AND a visible session is working, tick once a second; otherwise no timer.
  const anyVisibleBusy = pickerSessions.some((s) => s.busy);
  const [, setPanelClock] = useState(0);
  useEffect(() => {
    if (mode !== "panel" || !anyVisibleBusy) return;
    const t = setInterval(() => setPanelClock((x) => x + 1), 1000);
    return () => clearInterval(t);
  }, [mode, anyVisibleBusy]);

  // The conversation tree: user turns as nodes with every child session (forks,
  // compactions, subagents) that split off during the turn attached to it.
  // Inside a subagent branch the tree re-roots at the SPAWNER — the same full
  // tree as from the parent — instead of a tree of the subagent alone, which
  // left the parent thread invisible/unreachable (UX audit). The parent thread
  // is fetched when the tab opens; message-level ops then act on the parent.
  const parentId = store.session?.kind === "subagent" ? store.session.originId ?? null : null;
  const treeRootId = parentId && store.sessions.some((s) => s.id === parentId)
    ? parentId
    : currentId;
  const [parentThread, setParentThread] = useState<{ id: string; thread: Message[] } | null>(null);
  useEffect(() => {
    if (mode !== "panel" || panelTab !== "conversation") return;
    if (!treeRootId || treeRootId === currentId) return;
    let stale = false;
    api.getSession(treeRootId)
      .then(({ thread }) => {
        if (!stale) setParentThread({ id: treeRootId, thread });
      })
      .catch(() => {});
    return () => {
      stale = true;
    };
  }, [mode, panelTab, treeRootId, currentId]);
  const treeThread = treeRootId === currentId
    ? store.thread
    : parentThread?.id === treeRootId
    ? parentThread.thread
    : [];
  const childSessions = useMemo(
    () =>
      store.sessions.filter((s) =>
        s.originId === treeRootId && (showDeprecated || !s.deprecatedAt)
      ),
    [store.sessions, treeRootId, showDeprecated],
  );
  const convItems = useMemo(() => treeItems(buildTree(treeThread, childSessions), sections), [
    treeThread,
    childSessions,
    sections,
  ]);
  const diffEntries = flattenDiffs(store.changes);
  const cfgEntries = cfg ? modelEntries(cfg) : [];
  // ^f opens on the live tip, but branch rows can stream in after the open and
  // strand the initial "end" index mid-list — keep following the tip until the
  // user takes the cursor over; after that only clamp to a shrinking list.
  // Re-rooted at the spawner (inside a subagent), the cursor starts on the
  // current subagent's own row instead — "you are here" in the parent's tree.
  useEffect(() => {
    if (mode !== "panel" || panelTab !== "conversation") return;
    if (forkNavTouched.current) {
      moveForkSel((i) => Math.min(i, Math.max(0, convItems.length - 1)));
    } else if (treeRootId !== currentId) {
      const own = convItems.findIndex((it) => it.type === "branch" && it.session.id === currentId);
      moveForkSel(own >= 0 ? own : Math.max(0, convItems.length - 1));
    } else {
      moveForkSel(Math.max(0, convItems.length - 1));
    }
  }, [mode, panelTab, convItems, treeRootId, currentId, moveForkSel]);

  // `!cmd` runs locally in the session's workspace — a quick look (git status,
  // ls) shouldn't cost an agent turn. Output lands in a card above the composer;
  // the conversation never sees it. 30s cap so a hung command can't wedge the card.
  const shellWorkspace = store.session?.workspace ?? defaultWorkspace;
  const runShell = useCallback((cmd: string) => {
    const seq = ++shellSeq.current;
    // Into the backwards-fzf corpus (persisted + the in-memory copy, deduped
    // to the tip like fzf) before it even finishes — reruns are the point.
    appendShellHistory(cmd);
    if (shellHist.current) {
      shellHist.current = [...shellHist.current.filter((c) => c !== cmd), cmd];
    }
    setShellOut({ cmd, out: "", code: null });
    (async () => {
      try {
        const child = new Deno.Command("/bin/sh", {
          args: ["-c", cmd],
          cwd: shellWorkspace,
          stdin: "null",
          stdout: "piped",
          stderr: "piped",
        }).spawn();
        const timer = setTimeout(() => {
          try {
            child.kill("SIGKILL");
          } catch {
            // already exited
          }
        }, 30_000);
        const { code, stdout, stderr } = await child.output();
        clearTimeout(timer);
        const dec = new TextDecoder();
        const out = (dec.decode(stdout) + dec.decode(stderr)).replace(/\s+$/, "");
        if (shellSeq.current === seq) setShellOut({ cmd, out, code });
      } catch (e) {
        if (shellSeq.current === seq) setShellOut({ cmd, out: String(e), code: -1 });
      }
    })();
  }, [shellWorkspace]);

  // Send, creating the draft's session on first use. `/handoff <goal>` is a
  // composer command, not a message: the server drafts a self-contained opening
  // prompt from this thread and we open the fresh conversation with the composer
  // prefilled from it (the draft-prefill effect above the composer state).
  const submit = useCallback((text: string, queue: boolean) => {
    setScrollOff(0);
    setShowInfo(false);
    if (text.startsWith("!")) {
      const cmd = text.slice(1).trim();
      if (!cmd) return setErr("usage: !<command> — runs locally in the workspace");
      runShell(cmd);
      return;
    }
    setShellOut(null); // a real send supersedes the local-shell card
    if (/^\/conversation\s*$/.test(text)) {
      if (!store.currentId) return setErr("no open conversation — send a message first");
      setShowInfo(true);
      return;
    }
    if (/^\/schedules?\s*$/.test(text)) {
      loadSchedules();
      return;
    }
    if (/^\/workflows?\s*$/.test(text)) {
      // The workflows browser is the panel's workflows tab (openTab is defined
      // below, so inline its drill-state reset here, like /model does).
      setPanelMsg(null);
      setWfSel(0);
      setWfLevel(0);
      setWfOpenId(null);
      setWfDetail(null);
      setWfPhaseSel(0);
      setWfAgentSel(0);
      setWfScroll(0);
      setWfFilter(null);
      setWfPromptOpen(false);
      setPanelTab("workflows");
      setMode("panel");
      return;
    }
    if (/^\/theme\s*$/.test(text)) {
      // The theme picker is the panel's theme tab; the refreshPanel effect
      // fetches state + snaps the cursor onto the current theme on entry.
      setPanelMsg(null);
      setPanelTab("theme");
      setMode("panel");
      return;
    }
    if (/^\/model\s*$/.test(text)) {
      // Model switcher lives in the panel's model tab; land on the first model
      // row (openTab is defined below, so inline its model-entry reset here).
      setPanelMsg(null);
      setModelSel(0);
      setKeyInput(null);
      setPanelTab("model");
      setMode("panel");
      return;
    }
    if (/^\/effort\s*$/.test(text)) {
      // Same panel, cursor on the first thinking-depth row — the efforts
      // section follows the models in the flat list.
      setPanelMsg(null);
      setModelSel(cfg ? cfg.models.length : 0);
      setKeyInput(null);
      setPanelTab("model");
      setMode("panel");
      return;
    }
    if (/^\/handoff\b/.test(text)) {
      const goal = text.slice("/handoff".length).trim();
      if (!goal) return setErr("usage: /handoff <goal for the new conversation>");
      if (!store.currentId) return setErr("no open conversation to hand off from");
      store.notify("⤳ drafting handoff…");
      store.handoff(goal).then((s) => {
        if (!s) return; // the store surfaced the error as a notice
        openSession(s);
        store.notify("⤳ handoff drafted — review the prompt, edit, send");
      });
      return;
    }
    // A command-shaped "/word" that matches no command or skill must not leak
    // to the LLM as chat — say so instead. /theme is deliberately hidden from
    // the menu (theming lives in the panel), so point there.
    const slash = /^\/([A-Za-z][\w-]*)(\s|$)/.exec(text);
    if (slash) {
      const name = slash[1];
      if (name === "theme") {
        return setErr("theming lives in the panel — press ^t, then the theme tab");
      }
      const known = [
        "handoff",
        "conversation",
        "schedule",
        "schedules",
        "model",
        "effort",
        "workflow",
        "workflows",
      ].includes(name) ||
        skillsCache.current?.some((s) => s.name === name);
      if (skillsCache.current && !known) {
        return setErr(`unknown command: /${name} — tab completes from the / menu`);
      }
    }
    if (store.currentId) {
      store.send(text, queue).catch((e) => setErr(String(e)));
      return;
    }
    store.newSession(defaultWorkspace).then(
      (s) => {
        return store.send(text, false, s.id);
      },
      (e) => setErr(String(e)),
    );
  }, [
    store.currentId,
    store.send,
    store.newSession,
    store.handoff,
    store.notify,
    openSession,
    defaultWorkspace,
    runShell,
    loadSchedules,
    cfg,
  ]);

  // Open the panel on a tab, resetting that tab's transient state. One entry
  // point for the chat-mode jump chords and the same chords inside the panel.
  const openTab = (t: PanelTab) => {
    setPanelMsg(null);
    if (t === "sessions") {
      movePickSel(0);
      setFilter("");
      setMovePicks(null);
    } else if (t === "conversation") {
      // Open even on an empty thread (the tab says "no turns yet") — bailing here
      // made ^f a silent no-op in a fresh session while ^p/^d/^o all worked.
      // Start on the live tip (last selectable row); ↑ walks back through history.
      forkNavTouched.current = false;
      moveForkSel(Math.max(0, convItems.length - 1));
      setRangeAnchor(null);
    } else if (t === "changes") {
      setFileSel(0);
      setDiffScroll(0);
    } else if (t === "model") {
      setModelSel(0);
      setKeyInput(null);
    } else if (t === "mcp") {
      setMcpSel(0);
    } else if (t === "jobs") {
      setJobSel(0);
      setJobOpen(null);
      setJobText(null);
      setJobScroll(0);
    } else if (t === "workflows") {
      setWfSel(0);
      setWfLevel(0);
      setWfOpenId(null);
      setWfDetail(null);
      setWfAgentSel(0);
      setWfScroll(0);
    }
    setPanelTab(t);
    setMode("panel");
  };

  useInput((ch, key) => {
    // Quit: double ctrl+c — but during a turn the first ctrl+c interrupts
    // (Claude-Code parity); it only arms quit once idle.
    if (key.ctrl && ch === "c") {
      if (store.busy) return store.interrupt();
      const now = Date.now();
      if (now - lastCtrlC.current < 2000) exit();
      lastCtrlC.current = now;
      setQuitHint(true);
      setTimeout(() => setQuitHint(false), 2000);
      return;
    }

    if (mode === "panel") {
      // Masked API-key entry (model tab) owns the keyboard while open.
      if (panelTab === "model" && keyInput !== null) {
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
        if (key.super && (key.backspace || key.delete)) return setKeyInput(() => "");
        if (key.backspace || key.delete) return setKeyInput((v) => (v ?? "").slice(0, -1));
        if (ch && !key.ctrl && !key.meta) setKeyInput((v) => (v ?? "") + ch);
        return;
      }
      // Escape closes the panel — unless it's dismissing the sessions filter or a
      // pending conversation range selection.
      if (key.escape) {
        if (panelTab === "sessions" && filterActive) {
          setFilterActive(false);
          setFilter("");
        } else if (panelTab === "sessions" && movePicks) {
          setMovePicks(null); // cancel a pending move
        } else if (panelTab === "conversation" && rangeAnchor !== null) {
          setRangeAnchor(null);
        } else if (panelTab === "jobs" && jobOpen) {
          // Back to the job list; only esc at the list level leaves the panel.
          setJobOpen(null);
          setJobText(null);
          setJobScroll(0);
        } else if (panelTab === "workflows" && wfLevel > 0) {
          // Back out one drill level; only esc at the top level leaves the panel.
          if (wfLevel === 3) {
            setWfLevel(2);
            setWfScroll(0);
          } else if (wfLevel === 2) {
            setWfLevel(1);
          } else {
            setWfLevel(0);
            setWfOpenId(null);
            setWfDetail(null);
          }
        } else {
          // Theme browsing must not commit: put back the tab-entry theme
          // (Enter while in the tab moved that baseline to the confirmed one).
          if (panelTab === "theme") revertTheme();
          setMode("chat");
        }
        return;
      }
      // Tab cycles tabs (but while typing a session filter, let it type).
      if (key.tab && !(panelTab === "sessions" && filterActive)) {
        setPanelMsg(null);
        setMcpSel(0);
        setPanelTab((t) => PANEL_TABS[(PANEL_TABS.indexOf(t) + 1) % PANEL_TABS.length]);
        return;
      }
      // The chat-mode chords keep working here: ^p/^f/^d/^o jump tabs, ^t closes.
      if (key.ctrl && ch === "t") return setMode("chat");
      if (key.ctrl && (ch === "p" || ch === "f" || ch === "d" || ch === "o" || ch === "b")) {
        openTab(
          ch === "p"
            ? "sessions"
            : ch === "f"
            ? "conversation"
            : ch === "d"
            ? "changes"
            : ch === "b"
            ? "jobs"
            : "model",
        );
        return;
      }
      // ? opens the help overlay here too (the status bar promises it) — except
      // while the sessions filter is capturing printable keys.
      if (ch === "?" && !key.ctrl && !key.meta && !(panelTab === "sessions" && filterActive)) {
        setMode("help");
        return;
      }

      // ---- sessions: the lineage tree (was ^p) ----
      if (panelTab === "sessions") {
        if (filterActive) {
          // Enter opens the top match (fzf convention): exit filter mode and fall
          // through to the open handler below, cursor still pinned to row 0.
          if (key.return) setFilterActive(false);
          else if (key.backspace || key.delete) {
            movePickSel(0);
            return setFilter((f) => f.slice(0, -1));
          } else if (ch && !key.ctrl && !key.meta && !key.upArrow && !key.downArrow) {
            movePickSel(0);
            return setFilter((f) => f + ch);
          }
        }
        if (key.return) {
          const sel = treeRows[pickSelRef.current];
          if (!sel) return;
          // A pending move lands the turns on the chosen target, then opens it.
          if (movePicks) {
            const picks = movePicks;
            const target = sel.s;
            setMovePicks(null);
            setMode("chat");
            store.moveRange(target.id, picks).then((s) => s && openSession(s));
            return;
          }
          openSession(sel.s);
          return;
        }
        if (key.upArrow || (!filterActive && ch === "k")) {
          return movePickSel((i) => Math.max(0, i - 1));
        }
        if (key.downArrow || (!filterActive && ch === "j")) {
          return movePickSel((i) => Math.min(treeRows.length - 1, i + 1));
        }
        if (!filterActive && ch === "g") return movePickSel(0);
        if (!filterActive && ch === "G") return movePickSel(Math.max(0, treeRows.length - 1));
        if (!filterActive && ch === "/") return setFilterActive(true);
        // n: new session via the workspace-autocomplete dialog (esc returns here).
        if (!filterActive && ch === "n") {
          setNewComp({ text: "", cursor: 0 });
          setNewSel(0);
          setDirHits([]);
          setMode("new");
          return;
        }
        // x is the destructive key in every tab; ^x stays as an alias here for
        // the muscle memory it used to be the only binding for.
        if ((!filterActive && ch === "x") || (key.ctrl && ch === "x")) {
          const sel = treeRows[pickSelRef.current];
          if (!sel) return;
          // Always a double-tap (same as ctrl+c quit) — one keypress silently
          // losing work was a persona-testing finding. The empty-session
          // exemption that used to live here was safe while archive was ^x, but
          // plain x used to DEPRECATE: an old reflex now reaches the more
          // destructive action, so it must not fire on the first press.
          const armed = archiveArm.current.id === sel.s.id &&
            Date.now() - archiveArm.current.at < 3000;
          if (!armed) {
            archiveArm.current = { id: sel.s.id, at: Date.now() };
            setPanelMsg(`x again to archive "${sel.s.title || "(untitled)"}"`);
            return;
          }
          archiveArm.current = { id: "", at: 0 };
          setPanelMsg("archived — h reveals archived sessions, u restores one");
          store.archive(sel.s.id);
          return;
        }
        if (!filterActive && ch === "D") {
          const sel = treeRows[pickSelRef.current];
          if (!sel) return;
          if (sel.s.kind !== "root") {
            setPanelMsg(null);
            store.deprecate(sel.s.id, !sel.s.deprecatedAt);
          } else setPanelMsg("roots can't be deprecated — x archives"); // was a silent no-op
          return;
        }
        if (!filterActive && ch === "h") {
          // Revealing also lists archived sessions — fetch them on the way in.
          if (!showDeprecated) store.loadArchived().catch(() => {});
          setPanelMsg(null); // let the "(showing hidden…)" header show
          return setShowDeprecated((v) => !v);
        }
        if (!filterActive && ch === "u") {
          const sel = treeRows[pickSelRef.current];
          if (sel?.s.archivedAt) {
            setPanelMsg(null);
            store.unarchive(sel.s.id);
          } else setPanelMsg("u restores an archived session — h reveals them");
          return;
        }
        return;
      }

      // ---- conversation: the branch tree (was ^f) ----
      if (panelTab === "conversation") {
        // Section labeling (s): an LLM groups the turns into colored activity
        // sections (debug/implement/explore/…); pressing s again hides them.
        if (ch === "s") {
          if (sections) {
            setSections(null);
            return;
          }
          const id = treeRootId;
          const nodes = convItems.flatMap((it) => (it.type === "node" ? [it.node] : []));
          if (!id || nodes.length === 0) return;
          const byId = new Map(treeThread.map((m) => [m.id, m]));
          const firstLine = (m: Message | undefined): string => {
            const t = m?.parts.find((p) => p.type === "text");
            return t && "text" in t ? (t.text.split("\n").find((l) => l.trim()) ?? "") : "";
          };
          const gists = nodes.map((n: TreeNode) => {
            const user = firstLine(n.msg).slice(0, 140);
            // The reply's final text (the outcome) beats its first (the preamble).
            const replyMsg = [...n.msgIds.slice(1)].reverse()
              .map((mid) => byId.get(mid))
              .find((m) => m?.parts.some((p) => p.type === "text"));
            const reply = firstLine(replyMsg).slice(0, 140);
            const tools = n.steps.length;
            return {
              gist: `${user}${reply ? ` → ${reply}` : ""}${tools ? ` [${tools} tool runs]` : ""}`,
            };
          });
          setPanelMsg("✳ labeling sections…");
          api.getSections(id, gists).then((s) => {
            setSections(s);
            setPanelMsg(null);
          }, (e) => setPanelMsg(String(e)));
          return;
        }
        // Range selection (v): highlight turns, then compact/extract them. On a
        // section header, v (like enter) selects the whole section. The range
        // ops edit a session's OWN turns, so they need the parent open — not
        // the re-rooted view from inside one of its subagents.
        if (ch === "v" && treeRootId !== currentId) {
          setPanelMsg("this is the parent's tree — open the parent to edit its turns");
          return;
        }
        if (ch === "v") {
          forkNavTouched.current = true;
          const it = convItems[forkSelRef.current];
          if (it?.type === "section") {
            const span = sectionSpan(convItems, forkSelRef.current);
            if (span) {
              setRangeAnchor(span[0]);
              moveForkSel(span[1]);
            }
            return;
          }
          setRangeAnchor((a) => (a === null ? forkSelRef.current : null));
          return;
        }
        // x deletes the range — the destructive key is x in every tab.
        if (rangeAnchor !== null && (ch === "c" || ch === "e" || ch === "x" || ch === "m")) {
          const lo = Math.min(rangeAnchor, forkSelRef.current);
          const hi = Math.max(rangeAnchor, forkSelRef.current);
          const ids = convItems.slice(lo, hi + 1)
            .flatMap((it) => (it.type === "node" ? it.node.msgIds : []));
          if (ch === "m") {
            // Copy-to: stash the picks and switch to the sessions tab to pick a target.
            setRangeAnchor(null);
            setMovePicks(ids);
            movePickSel(0);
            setPanelTab("sessions");
            return;
          }
          setRangeAnchor(null);
          setMode("chat");
          const op = ch === "c"
            ? store.compactPicks
            : ch === "e"
            ? store.extractPicks
            : store.deleteRange;
          op(ids).then((s) => s && openSession(s));
          return;
        }
        if (key.return) {
          const it = convItems[forkSelRef.current];
          if (!it) return;
          // Enter on a section header arms the range over the whole section —
          // the c/e/d/m ops then apply to it like a hand-picked selection.
          if (it.type === "section") {
            const span = sectionSpan(convItems, forkSelRef.current);
            if (span) {
              forkNavTouched.current = true;
              setRangeAnchor(span[0]);
              moveForkSel(span[1]);
            }
            return;
          }
          setRangeAnchor(null);
          setMode("chat");
          // Forking is instant and silent — confirm what happened and the way back.
          const forked = (s: Session | null) => {
            if (!s) return;
            openSession(s);
            store.notify("⑂ forked — new branch opened (^p switches back)");
          };
          if (it.type === "branch") openSession(it.session);
          else if (it.type === "step") {
            store.fork(
              it.step.point.msgId,
              it.step.point.atPart,
              undefined,
              treeRootId ?? undefined,
            )
              .then(forked);
          } else {
            // Rewind-to-edit: the branch cuts BEFORE this turn and its user message
            // lands back in the composer, ready to edit & resend (Claude Code's
            // rewind), instead of sitting in history with the cursor after it.
            const text = it.node.msg.parts
              .filter((p) => p.type === "text")
              .map((p) => p.text)
              .join("\n");
            store.fork(it.node.point.msgId, undefined, true, treeRootId ?? undefined).then((s) => {
              if (!s) return;
              // Open first — openSession stashes/clears the composer for the
              // per-session draft; the rewound text must land after that.
              forked(s);
              if (text) {
                setComp({ text, cursor: text.length });
                setHistIdx(null);
                draft.current = "";
              }
            });
          }
          return;
        }
        if (key.upArrow || ch === "k") {
          forkNavTouched.current = true;
          return moveForkSel((i) => Math.max(0, i - 1));
        }
        if (key.downArrow || ch === "j") {
          forkNavTouched.current = true;
          return moveForkSel((i) => Math.min(convItems.length - 1, i + 1));
        }
        if (ch === "x") {
          const it = convItems[forkSelRef.current];
          if (it?.type === "branch") store.deprecate(it.session.id, !it.session.deprecatedAt);
          return;
        }
        if (ch === "h") return setShowDeprecated((v) => !v);
        // C: compact the WHOLE session onto a summary branch (a v-range + c
        // compacts just the selected span). Same own-turns rule as v above.
        if (ch === "C") {
          if (treeRootId !== currentId) {
            setPanelMsg("this is the parent's tree — open the parent to compact it");
            return;
          }
          store.compact().then((s) => s && openSession(s));
          return;
        }
        return;
      }

      if (panelTab === "theme") {
        // Moving the cursor previews live (the hovered preset lands, debounced,
        // on the server and the TUI recolors); Enter keeps it, Escape reverts.
        if (key.upArrow || ch === "k") return moveThemeSel((i) => i - 1);
        if (key.downArrow || ch === "j") return moveThemeSel((i) => i + 1);
        if (key.return) {
          const p = THEME_PRESETS[themeSelRef.current];
          if (p) {
            themeEntryRef.current = p.name === "Default"
              ? null
              : { name: p.name, colors: p.colors };
          }
          return applyPreset(themeSelRef.current);
        }
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
      // ---- changes: the run's uncommitted diffs (was the ^d modal) ----
      if (panelTab === "changes") {
        // enter acts — here that means apply the selected file, like every other
        // tab's enter. →/← still focus the hunks pane (full-height) and back out.
        if (key.return) {
          const e = diffEntries[fileSel];
          if (!e) return;
          store.applyChanges(e.source, [e.file.path]);
          return;
        }
        if (key.rightArrow) {
          if (diffEntries[fileSel]) setDiffFocus(true);
          return;
        }
        if (key.leftArrow) return setDiffFocus(false);
        if (key.upArrow || (!diffFocus && ch === "k")) {
          setDiffScroll(0);
          return setFileSel((i) => Math.max(0, i - 1));
        }
        if (key.downArrow || (!diffFocus && ch === "j")) {
          setDiffScroll(0);
          return setFileSel((i) => Math.min(diffEntries.length - 1, i + 1));
        }
        // Inside the focused hunks pane j/k scroll it — the same "move" key, one
        // level down.
        if (ch === "j") return setDiffScroll((s) => s + 3);
        if (ch === "k") return setDiffScroll((s) => Math.max(0, s - 3));
        if (ch === "A") {
          // Apply everything, one call per source (a session can have both the
          // repo source and clonefile).
          const own = diffEntries;
          for (const source of new Set(own.map((e) => e.source))) {
            store.applyChanges(
              source,
              own.filter((e) => e.source === source).map((e) => e.file.path),
            );
          }
          return;
        }
        if (ch === "x") {
          // Nothing listed → nothing to revert; the raw call 400s ("no
          // workspace") into a confusing error line.
          if (diffEntries.length > 0) store.revertChanges();
          return;
        }
        return;
      }
      // ---- model: model/effort/worker switch + API keys (was the ^o modal) ----
      if (panelTab === "model") {
        if (key.upArrow || ch === "k") return setModelSel((i) => Math.max(0, i - 1));
        if (key.downArrow || ch === "j") {
          return setModelSel((i) => Math.min(cfgEntries.length - 1, i + 1));
        }
        // x on a set key row removes that provider's key (the row hint says so).
        if (ch === "x" && !key.ctrl && !key.meta) {
          const e = cfgEntries[modelSel];
          if (e?.kind === "key" && cfg?.keys?.[e.id as KeyProvider]) {
            api.deleteKey(e.id as KeyProvider)
              .then(() => api.getConfig().then(setCfg))
              .catch((err) => setErr(String(err)));
          }
          return;
        }
        if (key.return) {
          const e = cfgEntries[modelSel];
          if (!e) return;
          if (e.kind === "key") {
            setKeyInput("");
            return;
          }
          // Model/effort switches pin the OPEN session and move the default for
          // new sessions (other sessions keep theirs); worker stays process-global.
          (e.kind === "model"
            ? api.setModel(e.id, currentId ?? undefined)
            : e.kind === "effort"
            ? api.setEffort(e.id, currentId ?? undefined)
            : api.setWorker(e.id))
            .then(() => api.getConfig().then(setCfg))
            .catch((err) => setErr(String(err)));
          return;
        }
        return;
      }
      // ---- jobs: background shells → one shell's full output ----
      if (panelTab === "jobs") {
        const list = store.jobs;
        if (jobOpen) {
          if (key.upArrow || ch === "k") return setJobScroll((n) => Math.max(0, n - 1));
          if (key.downArrow || ch === "j") return setJobScroll((n) => n + 1);
          if (key.pageUp) return setJobScroll((n) => Math.max(0, n - (rows - 10)));
          if (key.pageDown) return setJobScroll((n) => n + (rows - 10));
          if (ch === "x") {
            return void api.killJob(currentId!, jobOpen).then(
              (r) => setPanelMsg(r.message),
              (e) => setPanelMsg(String(e)),
            );
          }
          return;
        }
        if (key.upArrow || ch === "k") return setJobSel((i) => Math.max(0, i - 1));
        if (key.downArrow || ch === "j") {
          return setJobSel((i) => Math.min(Math.max(0, list.length - 1), i + 1));
        }
        const job = list[jobSel];
        if (!job) return;
        if (key.return || key.rightArrow) {
          setJobOpen(job.id);
          setJobText(null);
          setJobScroll(0);
          return;
        }
        if (ch === "x") {
          if (job.status !== "running") return setPanelMsg(`${job.id} already finished`);
          return void api.killJob(currentId!, job.id).then(
            (r) => setPanelMsg(r.message),
            (e) => setPanelMsg(String(e)),
          );
        }
        return;
      }
      // ---- workflows: runs → one run's agents → one agent's detail ----
      if (panelTab === "workflows") {
        const runs = store.workflows;
        // Shared run actions; feedback lands in the panel message line. The
        // list/detail refresh rides the workflow.* events, not these responses.
        const action = (id: string, act: "stop" | "pause" | "resume") =>
          api.workflowAction(id, act).then(
            (r) => setPanelMsg(`${act}: run is now ${r.status}`),
            (e) => setPanelMsg(String(e)),
          );
        const agentAction = (id: string, agentId: string, act: "stop" | "restart") =>
          api.workflowAgentAction(id, agentId, act).then(
            (a) => setPanelMsg(`${act}: ${a.label}`),
            (e) => setPanelMsg(String(e)),
          );
        const rerun = (id: string) =>
          api.rerunWorkflow(id).then(
            (r) => setPanelMsg(`rerun started: ${r.name} (${r.id.slice(0, 8)})`),
            (e) => setPanelMsg(String(e)),
          );
        const openAgentSession = (sid: string | null) => {
          const s = store.sessions.find((x) => x.id === sid);
          if (s) openSession(s);
          else setPanelMsg("that agent has no session yet");
        };
        const openRun = (id: string) => {
          setWfOpenId(id);
          setWfDetail(null);
          setWfPhaseSel(0);
          setWfAgentSel(0);
          setWfFilter(null);
          setWfLevel(1);
        };
        if (wfLevel === 0) {
          if (key.upArrow || ch === "k") return setWfSel((i) => Math.max(0, i - 1));
          if (key.downArrow || ch === "j") {
            return setWfSel((i) => Math.min(Math.max(0, runs.length - 1), i + 1));
          }
          const r = runs[wfSel];
          if (!r) return;
          if (key.return || key.rightArrow) return openRun(r.id);
          if (ch === "x") return void action(r.id, "stop");
          if (ch === "p") return void action(r.id, r.status === "paused" ? "resume" : "pause");
          if (ch === "r") return void rerun(r.id);
          return;
        }
        const run = wfDetail?.run;
        const agent = wfAgents[wfAgentSel];
        if (wfLevel === 1) {
          if (key.upArrow || ch === "k") {
            setWfAgentSel(0);
            return setWfPhaseSel((i) => Math.max(0, i - 1));
          }
          if (key.downArrow || ch === "j") {
            setWfAgentSel(0);
            return setWfPhaseSel((i) => Math.min(Math.max(0, wfGroups.length - 1), i + 1));
          }
          if (key.leftArrow) {
            setWfLevel(0);
            setWfOpenId(null);
            setWfDetail(null);
            return;
          }
          if (key.return || key.rightArrow) {
            setWfAgentSel(0);
            setWfLevel(2);
            return;
          }
          if (run) {
            if (ch === "x") return void action(run.id, "stop");
            if (ch === "p") {
              return void action(run.id, run.status === "paused" ? "resume" : "pause");
            }
            if (ch === "r") return void rerun(run.id);
          }
          return;
        }
        if (wfLevel === 2) {
          if (key.upArrow || ch === "k") return setWfAgentSel((i) => Math.max(0, i - 1));
          if (key.downArrow || ch === "j") {
            return setWfAgentSel((i) => Math.min(Math.max(0, wfAgents.length - 1), i + 1));
          }
          if (key.leftArrow) return setWfLevel(1);
          if ((key.return || key.rightArrow) && agent) {
            setWfScroll(0);
            setWfPromptOpen(false);
            setWfLevel(3);
            return;
          }
          // / cycles the status filter (the filter key everywhere); the cursor
          // goes home so it can never point past the end of the filtered list.
          if (ch === "/") {
            setWfAgentSel(0);
            return setWfFilter((cur) =>
              WF_FILTERS[(WF_FILTERS.indexOf(cur) + 1) % WF_FILTERS.length]
            );
          }
          if (ch === "o") return openAgentSession(agent?.sessionId ?? null);
          // x/r are SCOPED to the selected agent here — the run keeps going.
          if ((ch === "x" || ch === "r") && run && agent) {
            return void agentAction(run.id, agent.id, ch === "x" ? "stop" : "restart");
          }
          if (ch === "p" && run) {
            return void action(run.id, run.status === "paused" ? "resume" : "pause");
          }
          return;
        }
        // Level 3: one agent's detail — the list cursor still moves so you can
        // read down a phase without backing out.
        if (key.upArrow) {
          setWfScroll(0);
          return setWfAgentSel((i) => Math.max(0, i - 1));
        }
        if (key.downArrow) {
          setWfScroll(0);
          return setWfAgentSel((i) => Math.min(Math.max(0, wfAgents.length - 1), i + 1));
        }
        if (key.return) return setWfPromptOpen((o) => !o);
        if (ch === "j") {
          // Clamp at the last screenful — unclamped j walked off the end into a
          // blank pane reading "45/44".
          const total = agent ? agentDetailLines(agent, wfPromptOpen) : 0;
          const max = Math.max(0, total - Math.max(4, rows - 15));
          return setWfScroll((s) => Math.min(max, s + 3));
        }
        if (ch === "k") return setWfScroll((s) => Math.max(0, s - 3));
        if (key.leftArrow) {
          setWfLevel(2);
          setWfScroll(0);
          return;
        }
        if (ch === "o") return openAgentSession(wfAgents[wfAgentSel]?.sessionId ?? null);
        return;
      }
      // Unbound printable key while a panel tab has focus: say where typing goes
      // instead of a silent no-op (user-testing: input "vanished" until esc).
      if (ch && !key.ctrl && !key.meta) {
        setPanelMsg("this panel has focus — esc returns to the chat composer");
      }
      return;
    }

    if (mode === "help") {
      // A list taller than the terminal scrolls (j/k, arrows, page keys — the
      // overlay used to cut off with "enlarge the window"); anything else closes.
      const maxS = helpMaxScrollRef.current;
      if (maxS > 0) {
        if (key.downArrow || ch === "j") return setHelpScroll((o) => Math.min(maxS, o + 1));
        if (key.upArrow || ch === "k") return setHelpScroll((o) => Math.max(0, o - 1));
        if (key.pageDown) return setHelpScroll((o) => Math.min(maxS, o + 10));
        if (key.pageUp) return setHelpScroll((o) => Math.max(0, o - 10));
      }
      setHelpScroll(0);
      setMode("chat"); // any other key closes
      // …but a ctrl-chord typed right behind the closing key shouldn't be
      // swallowed — fall through to the chat handlers below (Esc+^p chording).
      if (!(key.ctrl && ch)) return;
    }

    if (mode === "new") {
      // Back to where it was launched from: the panel's sessions tab.
      if (key.escape) return setMode("panel");
      if (key.return) {
        const hit = dirHits[newSel];
        // A typed query with no hit does nothing — a silent workspace-less session
        // would run turns in the server's cwd (the live repo). Clear the query to
        // create one deliberately.
        if (!hit && newQuery.trim() !== "") return;
        // Stash now: newSession switches currentId before openSession runs, so
        // the openSession stash would miss and the new composer inherit old text.
        stashDraft();
        store.newSession(hit?.path).then((s) => openSession(s), (e) => setErr(String(e)));
        return;
      }
      if (key.upArrow) return setNewSel((i) => Math.max(0, i - 1));
      if (key.downArrow) return setNewSel((i) => Math.min(dirHits.length - 1, i + 1));
      // The query is a line edit with the composer's keys (Home/End arrive via
      // onNavKey below); edits reset the pick to the top hit.
      // Cmd (super) + ←/→ jumps to start/end of the query line (macOS habit).
      if (key.super && key.leftArrow) return setNewComp((c) => ({ ...c, cursor: 0 }));
      if (key.super && key.rightArrow) return setNewComp((c) => ({ ...c, cursor: c.text.length }));
      if (key.leftArrow) return setNewComp((c) => ({ ...c, cursor: Math.max(0, c.cursor - 1) }));
      if (key.rightArrow) {
        return setNewComp((c) => ({ ...c, cursor: Math.min(c.text.length, c.cursor + 1) }));
      }
      if (key.ctrl && ch === "a") return setNewComp((c) => ({ ...c, cursor: 0 }));
      if (key.ctrl && ch === "e") return setNewComp((c) => ({ ...c, cursor: c.text.length }));
      if (key.ctrl && ch === "u") {
        setNewSel(0);
        return setNewComp({ text: "", cursor: 0 });
      }
      if (key.ctrl && ch === "w") {
        setNewSel(0);
        return setNewComp((c) => {
          const from = wordLeft(c.text, c.cursor);
          return { text: c.text.slice(0, from) + c.text.slice(c.cursor), cursor: from };
        });
      }
      if (key.ctrl && ch === "k") {
        setNewSel(0);
        return setNewComp((c) => ({ text: c.text.slice(0, c.cursor), cursor: c.cursor }));
      }
      // ⌘⌫ deletes to line start — multiline-aware, matching the main composer.
      if (key.super && (key.backspace || key.delete)) {
        setNewSel(0);
        return setNewComp((c) => {
          const from = c.text.lastIndexOf("\n", c.cursor - 1);
          const start = from < 0 ? 0 : from + 1;
          return start === c.cursor
            ? c
            : { text: c.text.slice(0, start) + c.text.slice(c.cursor), cursor: start };
        });
      }
      if (key.backspace || key.delete) {
        setNewSel(0);
        return setNewComp((c) =>
          c.cursor === 0 ? c : {
            text: c.text.slice(0, c.cursor - 1) + c.text.slice(c.cursor),
            cursor: c.cursor - 1,
          }
        );
      }
      if (ch && !key.ctrl && !key.meta) {
        setNewSel(0);
        setNewComp((c) => ({
          text: c.text.slice(0, c.cursor) + ch + c.text.slice(c.cursor),
          cursor: c.cursor + ch.length,
        }));
      }
      return;
    }

    // Transcript search owns the keyboard while open: type to refine, enter/↓
    // next, ↑ previous, ^s advances too, esc closes. Page keys still scroll;
    // any other chord closes the bar and falls through to its normal action.
    if (searchQ !== null) {
      if (key.escape) return setSearchQ(null);
      if (key.return || key.downArrow || (key.ctrl && ch === "s")) {
        if (matches.length) {
          setSearchIdx((i) => (Math.min(i, matches.length - 1) + 1) % matches.length);
        }
        return;
      }
      if (key.upArrow) {
        if (matches.length) {
          setSearchIdx((i) =>
            (Math.min(i, matches.length - 1) - 1 + matches.length) % matches.length
          );
        }
        return;
      }
      if (key.backspace || key.delete) {
        setSearchIdx(0);
        return setSearchQ((q) => (q ?? "").slice(0, -1));
      }
      if (ch && !key.ctrl && !key.meta) {
        // Coalesced input can smuggle returns in as DATA ("\r\r" from rapid
        // enters) — a raw CR in the query corrupts the bar render. Strip them;
        // a pure-returns chunk means "advance" like the enter branch above.
        const norm = ch.replace(/[\r\n]/g, "");
        if (norm) {
          setSearchIdx(0);
          setSearchQ((q) => (q ?? "") + norm);
        } else if (matches.length) {
          setSearchIdx((i) => (Math.min(i, matches.length - 1) + 1) % matches.length);
        }
        return;
      }
      if (!key.pageUp && !key.pageDown) setSearchQ(null); // other chords exit search…
      // …and fall through to their usual meaning.
    }
    if (key.ctrl && ch === "s") {
      setSearchIdx(0);
      setSearchQ("");
      return;
    }
    // The /schedule popup owns the keyboard while open (list: pick/toggle/delete/
    // new; form: type into the focused field). Esc unwinds form → list → closed.
    if (sched) {
      const f = sched.form;
      if (f) {
        if (key.escape) return setSched((s) => s && { ...s, form: null, msg: null });
        // Tab (or enter on any field but the last) moves focus; enter on the
        // last field submits through the same validated route as REST/host fn.
        if (key.tab || (key.return && f.focus < SCHED_FIELDS.length - 1)) {
          return setSched((s) =>
            s?.form
              ? { ...s, form: { ...s.form, focus: (s.form.focus + 1) % SCHED_FIELDS.length } }
              : s
          );
        }
        if (key.return) {
          const [title, prompt, spec, workspace] = f.fields.map((v) => v.trim());
          if (!title || !prompt || !spec) {
            return setSched((s) => s && { ...s, msg: "title, prompt, and spec are required" });
          }
          api.createSchedule({ title, prompt, spec, ...(workspace ? { workspace } : {}) }).then(
            () => loadSchedules(),
            (e) => setSched((s) => s && { ...s, msg: String((e as Error).message ?? e) }),
          );
          return;
        }
        if (key.super && (key.backspace || key.delete)) {
          return setSched((s) =>
            s?.form
              ? { ...s, form: { ...s.form, fields: s.form.fields.map((v, i) => i === s.form!.focus ? "" : v) } }
              : s
          );
        }
        if (key.backspace || key.delete) {
          return setSched((s) =>
            s?.form
              ? {
                ...s,
                form: {
                  ...s.form,
                  fields: s.form.fields.map((v, i) => i === s.form!.focus ? v.slice(0, -1) : v),
                },
              }
              : s
          );
        }
        if (ch && !key.ctrl && !key.meta) {
          const norm = ch.replace(/[\r\n]/g, "");
          if (!norm) return;
          return setSched((s) =>
            s?.form
              ? {
                ...s,
                form: {
                  ...s.form,
                  fields: s.form.fields.map((v, i) => i === s.form!.focus ? v + norm : v),
                },
              }
              : s
          );
        }
        return;
      }
      if (key.escape) return setSched(null);
      if (key.upArrow) return setSched((s) => s && { ...s, sel: Math.max(0, s.sel - 1) });
      if (key.downArrow) {
        return setSched((s) =>
          s && { ...s, sel: Math.min(Math.max(0, s.scheds.length - 1), s.sel + 1) }
        );
      }
      if (ch === "n" || ch === "a") {
        // Workspace defaults to the open session's (else the launch default) —
        // mirroring schedule.add()'s default for the model.
        const ws = store.session?.workspace ?? defaultWorkspace ?? "";
        return setSched((s) =>
          s && { ...s, form: { fields: ["", "", "", ws], focus: 0 }, msg: null }
        );
      }
      const cur = sched.scheds[sched.sel];
      if (cur && (ch === " " || key.return || ch === "e")) {
        api.patchSchedule(cur.id, { enabled: !cur.enabled }).then(
          () => loadSchedules(),
          (e) => setSched((s) => s && { ...s, msg: String((e as Error).message ?? e) }),
        );
        return;
      }
      if (cur && (ch === "d" || ch === "x")) {
        api.deleteSchedule(cur.id).then(
          () => loadSchedules(),
          (e) => setSched((s) => s && { ...s, msg: String((e as Error).message ?? e) }),
        );
        return;
      }
      return;
    }
    // chat mode. Five jump chords open the one panel view on a tab; ^t toggles
    // the panel on whatever tab it last showed.
    if (key.ctrl && ch === "p") return openTab("sessions");
    if (key.ctrl && ch === "f") return openTab("conversation");
    if (key.ctrl && ch === "d") return openTab("changes");
    if (key.ctrl && ch === "o") return openTab("model");
    if (key.ctrl && ch === "b") return openTab("jobs");
    if (key.ctrl && ch === "t") return openTab(panelTab);
    // ^x: stop this conversation's turn, every subagent under it, and their
    // background shells. esc only stops a RUNNING turn, so a detached subagent
    // that outlived its parent's turn had no stop path in the TUI at all —
    // quitting the TUI doesn't stop it either, since the server outlives it.
    if (key.ctrl && ch === "x") {
      const live = branches.filter((b) => b.busy).length;
      if (!store.currentId) return;
      if (!store.busy && live === 0 && runningJobs === 0) {
        return store.notify("nothing running in this conversation");
      }
      const now = Date.now();
      if (now - lastCtrlX.current >= 3000) {
        lastCtrlX.current = now;
        const bits = [
          live > 0 ? `${live} subagent${live > 1 ? "s" : ""}` : null,
          runningJobs > 0 ? `${runningJobs} job${runningJobs > 1 ? "s" : ""}` : null,
        ].filter(Boolean).join(" · ");
        return store.notify(`^x again to stop ${bits || "the running turn"}`);
      }
      lastCtrlX.current = 0;
      store.stopAll();
      return;
    }
    // ^e doubles as readline end-of-line while composing (matching the help's
    // line-editing table) — expand-all only fires on an empty input; toggling
    // folds mid-thought on a text-editing chord was a user-testing bug.
    if (key.ctrl && ch === "e" && input === "") {
      setToggled(new Set());
      setExpandAll((v) => !v);
      return;
    }
    if (ch === "?" && !key.ctrl && !key.meta && input === "" && !store.ask) {
      setMode("help");
      return;
    }
    // Autocomplete popup owns navigation + enter while open. Claude Code muscle
    // memory: tab completes into the composer, enter RUNS a slash command (an
    // enter that merely re-inserted the text you already typed read as "/ is
    // broken" — nothing visibly happened). @-file selections always just insert:
    // they're part of a message being composed, not a command.
    if (popup && popup.items.length === 0) {
      // The "no matching commands" row: nothing to navigate or complete. Esc
      // dismisses; enter falls through so submit() answers with the unknown-
      // command hint; other keys keep typing/filtering.
      if (key.escape) return setPopup(null);
      if (key.tab || key.upArrow || key.downArrow) return;
    } else if (popup) {
      const completed = (c: { text: string; cursor: number }) => {
        // Tab on an unselected (sel -1) shell list completes the top match.
        const it = popup.items[Math.max(0, popup.sel)];
        return {
          text: c.text.slice(0, popup.tokenStart) + it.insert + c.text.slice(popup.tokenEnd),
          cursor: popup.tokenStart + it.insert.length,
        };
      };
      if (key.escape) return setPopup(null);
      if (key.upArrow) {
        return setPopup((p) =>
          p &&
          {
            ...p,
            sel: p.sel < 0 ? p.items.length - 1 : (p.sel - 1 + p.items.length) % p.items.length,
          }
        );
      }
      if (key.downArrow) {
        return setPopup((p) => p && { ...p, sel: p.sel < 0 ? 0 : (p.sel + 1) % p.items.length });
      }
      if (key.tab) {
        setComp(completed);
        setPopup(null);
        return;
      }
      if (key.return && popup.kind === "shell" && popup.sel < 0) {
        // Browsing, nothing picked: close the list and fall through — enter
        // sends the TYPED line (sendNow below), never a command you didn't pick.
        setPopup(null);
      } else if (key.return) {
        if (popup.kind !== "file") {
          // Skills and shell picks RUN on enter (fzf/ctrl-r muscle memory).
          // Resolve inside the updater (same stale-closure guard as sendNow).
          setComp((c) => {
            const text = completed(c).text.trim();
            queueMicrotask(() => {
              history.current.push(text);
              appendHistory(text);
              setHistIdx(null);
              draft.current = "";
              submit(text, key.meta);
            });
            return { text: "", cursor: 0 };
          });
        } else {
          setComp(completed);
        }
        setPopup(null);
        return;
      }
    }
    // Tab accepts the worker ghost (the popup's tab wins above when one is open).
    if (key.tab && !popup && ghostText && !store.ask) {
      const add = ghostText;
      setGhost(null);
      return setComp((c) => ({ text: c.text + add, cursor: c.text.length + add.length }));
    }
    if (key.pageUp) return setScrollOff((o) => Math.min(maxOff, o + Math.max(1, viewH - 2)));
    if (key.pageDown) return setScrollOff((o) => Math.max(0, o - Math.max(1, viewH - 2)));
    if (store.ask) {
      // The question card replaces the composer. Deciding keys arm after a short
      // beat so an in-flight composer keystroke can't answer unseen; esc DECLINES
      // the question — the program catches a "user declined" error and continues,
      // so the turn keeps running (interrupt still reachable once the card clears).
      const armed = Date.now() - askSince.current > 250;
      if (askTyping) {
        if (key.return) {
          if (armed && askText.trim()) store.answerAsk(askText.trim());
          return;
        }
        if (key.escape) {
          // With options, esc backs out of the text line first (panel
          // convention); option-less questions have nothing to back into.
          if (askHasOptions) {
            setAskTyping(false);
            setAskText("");
          } else if (armed) store.declineAsk();
          return;
        }
        if (key.super && (key.backspace || key.delete)) return setAskText(() => "");
        if (key.backspace || key.delete) return setAskText((t) => t.slice(0, -1));
        if (ch && !key.ctrl && !key.meta) return setAskText((t) => t + ch);
        return;
      }
      if (/^[1-9]$/.test(ch) && armed) {
        const opt = store.ask.options?.[Number(ch) - 1];
        if (opt) store.answerAsk(opt);
        return;
      }
      if (ch === "t") return setAskTyping(true);
      if (key.escape && armed) return store.declineAsk();
      return;
    }
    // The subagent rail owns the arrows while it holds the cursor: ↑/↓ walk the
    // rows (↑ past the top hands focus back to the composer), enter opens the
    // branch, esc leaves. Anything else drops focus and types as usual.
    if (railSel !== null && branches.length > 0) {
      if (key.downArrow) return setRailSel(Math.min(branches.length - 1, railSel + 1));
      if (key.upArrow) return setRailSel(railSel === 0 ? null : railSel - 1);
      if (key.return) {
        const b = branches[Math.min(railSel, branches.length - 1)];
        const s = store.sessions.find((x) => x.id === b.id);
        setRailSel(null);
        if (s) openSession(s);
        return;
      }
      if (key.escape) return setRailSel(null);
      setRailSel(null);
    }
    if (key.escape) {
      // Inside a subagent branch, esc means "get me back up" — return to the
      // spawner without touching the subagent's turn. This closes the trap where
      // peeking into a running subagent and hitting esc silently killed its work
      // (esc = interrupt below). Explicitly interrupting a subagent is niche and
      // risky — the parent may be awaiting it — so it's not on the bare reflex.
      const spawner = spawnerSession();
      if (spawner) return openSession(spawner);
      // Esc is the agent's stop button; when idle it clears a lingering notice
      // (error notices used to sit above the composer with no way to dismiss).
      if (store.busy) store.interrupt();
      else if (shellOut) setShellOut(null);
      else if (showInfo) setShowInfo(false);
      else if (store.notice) store.dismissNotice();
      else if (key.meta || Date.now() - lastEscAt.current < 600) {
        // key.meta: two ESC bytes in one stdin chunk (fast double-tap) parse as
        // a single meta+escape keypress rather than two events.
        // Double-esc (Claude Code parity): clear the draft (↑ recalls it), or
        // with an empty composer open the rewind view — the conversation tree,
        // where enter branches at an earlier turn.
        lastEscAt.current = 0;
        if (input.trim() !== "") {
          history.current.push(input);
          appendHistory(input);
          setHistIdx(null);
          draft.current = "";
          setComp({ text: "", cursor: 0 });
          // Silent clearing read as data loss (and help sells ↑ as history-only).
          flashMsg("draft cleared · ↑ restores");
        } else if (store.thread.length > 0) {
          forkNavTouched.current = false;
          moveForkSel(Math.max(0, convItems.length - 1));
          setRangeAnchor(null);
          setPanelTab("conversation");
          setMode("panel");
        }
        return;
      } else {
        // This esc did nothing on its own — arm the pair.
        lastEscAt.current = Date.now();
      }
      return;
    }
    // Send resolves the text inside the updater: an Enter that lands in the same
    // React batch as just-typed/pasted text would otherwise read a stale (empty)
    // closure and silently drop the send.
    const sendNow = (queue: boolean) => {
      setComp((c) => {
        const text = c.text.trim();
        if (!text) return c;
        if (text === "?") {
          // A bare "?" sent as a message is a help request, not a prompt — the
          // status bar advertises "? help" without the empty-composer caveat.
          queueMicrotask(() => setMode("help"));
          return { text: "", cursor: 0 };
        }
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
    // Shift+enter = newline (kitty-protocol terminals report the modifier;
    // elsewhere it arrives as a plain return and ctrl+j stays the chord).
    if (key.return && key.shift) {
      return insertAtCursor("\n");
    }
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
      // Past the end of history (or with nothing typed), ↓ drops into the
      // subagent rail under the status bar rather than doing nothing.
      if (histIdx === null) {
        if (input === "" && branches.length > 0) setRailSel(0);
        return;
      }
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
    // Cmd (super) + ←/→ jumps to start/end of the current line — macOS muscle
    // memory. Kitty protocol delivers the modifier as key.super; on terminals
    // without it, mouse.ts intercepts the legacy CSI 1;9 C/D sequences and
    // dispatches them as cmdHome/cmdEnd nav keys (handled in navKeyRef above).
    if (key.super && key.leftArrow) {
      return moveCursor((c) => {
        const nl = c.text.lastIndexOf("\n", c.cursor - 1);
        return nl < 0 ? 0 : nl + 1;
      });
    }
    if (key.super && key.rightArrow) {
      return moveCursor((c) => {
        const nl = c.text.indexOf("\n", c.cursor);
        return nl < 0 ? c.text.length : nl;
      });
    }
    if (key.leftArrow) {
      // Empty composer inside a subagent branch: ← pops back to the spawner
      // (same exit as esc). With text it stays plain cursor movement.
      if (input === "") {
        const spawner = spawnerSession();
        if (spawner) return openSession(spawner);
      }
      return moveCursor((c) => c.cursor - 1);
    }
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
    // Ctrl+J is a raw "\n": ink names it 'enter' with no ctrl flag (kitty-protocol
    // terminals do report ctrl+"j"), so match both — it's the newline chord, and
    // without this it fell into the coalesced-text branch below whose trailing-\n
    // rule means "send" (user-testing bug: ^j submitted half a message).
    if ((key.ctrl && ch === "j") || (ch === "\n" && !key.return)) {
      return insertAtCursor("\n");
    }
    // ⌘⌫ (Cmd+Backspace) deletes to the start of the current line — the macOS
    // counterpart of the ⌘← jump above. Multiline-aware: the line is bounded by
    // the preceding newline (or the text start), matching the ⌘← behavior.
    if (key.super && (key.backspace || key.delete)) {
      return setComp((c) => {
        const from = c.text.lastIndexOf("\n", c.cursor - 1);
        const start = from < 0 ? 0 : from + 1;
        return start === c.cursor
          ? c
          : { text: c.text.slice(0, start) + c.text.slice(c.cursor), cursor: start };
      });
    }
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
  // The open session's project dir, ~-shortened for the status bar. Prefer
  // originDir, which mirrors the workspace and is never rewritten; null for
  // pre-originDir sessions (a legacy internal worktree path would only mislead).
  const sessionDir = store.session?.originDir
    ? store.session.originDir.replace(new RegExp(`^${Deno.env.get("HOME")}`), "~")
    : null;

  // /conversation card rows — derived at render so they track live session state.
  // [label, display, copy] — copy is the raw value a click puts on the clipboard
  // (full workspace path, bare model id, origin session id — not the pretty text).
  const infoRows: [string, string, string][] = (() => {
    const s = store.session;
    if (!showInfo || !s) return [];
    const rows: [string, string, string][] = [
      ["id", s.id, s.id],
      ["title", s.title || "(untitled)", s.title],
      ["kind", s.kind, s.kind],
      ["created", new Date(s.createdAt).toLocaleString(), new Date(s.createdAt).toISOString()],
    ];
    if (s.workspace) {
      rows.push([
        "workspace",
        s.workspace.replace(new RegExp(`^${Deno.env.get("HOME")}`), "~"),
        s.workspace,
      ]);
    }
    const model = s.model ?? cfg?.model;
    rows.push([
      "model",
      s.model ? `${s.model} (pinned)` : model ? `${model} (default)` : "—",
      model ?? "",
    ]);
    const effort = s.effort ?? cfg?.effort;
    if (effort) {
      rows.push([
        "thinking",
        s.effort ? `${s.effort} (pinned)` : `${effort} (default)`,
        effort,
      ]);
    }
    if (s.originId) {
      const origin = store.sessions.find((x) => x.id === s.originId);
      rows.push([
        "origin",
        origin ? `${origin.title || origin.id} · ${s.originId}` : s.originId,
        s.originId,
      ]);
    }
    rows.push(["messages", String(store.thread.length), String(store.thread.length)]);
    const bg = termBackground();
    if (bg) rows.push(["terminal", `${bg.scheme} (${bg.hex})`, bg.hex]);
    if (s.contextTokens) {
      const cached = s.cachedTokens
        ? ` · ${Math.round((s.cachedTokens / s.contextTokens) * 100)}% cached`
        : "";
      const pct = ctxPctLeft({
        contextTokens: s.contextTokens,
        contextLimit: store.usage.contextLimit,
      });
      const left = pct !== null ? ` · ${pct}% left` : "";
      const display = `${fmtTokens(s.contextTokens)} tokens${cached}${left}`;
      rows.push(["context", display, display]);
    }
    const spend = store.usage.tree?.costUsd ?? store.usage.costUsd ?? 0;
    if (spend > 0) {
      const subs = store.usage.tree?.sessions ?? 0;
      const display = fmtUsd(spend) +
        (subs > 0 ? ` incl. ${subs} subagent${subs > 1 ? "s" : ""}` : "");
      rows.push(["cost", display, display]);
    }
    return rows;
  })();

  // Where the activity line lands on screen this frame: chrome starts at viewH,
  // then queued rows, the error line, and the info card (bordered: rows + 4)
  // precede it. Synced post-commit like `layout`, so clicks map to the painted frame.
  const activityRow = mode === "chat" && store.busy && store.activity
    ? viewH + store.queued.length + (err ? 1 : 0) +
      (infoRows.length > 0 ? infoRows.length + 4 : 0)
    : null;
  // The info card's data rows are clickable (click copies the value): first data
  // row sits below the card's top border + title. Same post-commit sync as above.
  const infoClick = mode === "chat" && infoRows.length > 0
    ? { firstRow: viewH + store.queued.length + (err ? 1 : 0) + 2, rows: infoRows }
    : null;
  useEffect(() => {
    activityRowRef.current = activityRow;
    infoClickRef.current = infoClick;
  });

  // The unified management view: one bordered container, a tab bar, and the active
  // tab's content (sessions/conversation trees, or mcp/skills).
  const panel = mode === "panel"
    ? (
      <Box
        flexDirection="column"
        borderStyle="round"
        backgroundColor={palette.panel}
        borderColor={palette.border}
        paddingX={1}
      >
        <PanelTabs tab={panelTab} />
        {panelTab === "sessions"
          ? (
            <SessionPicker
              rowsList={treeRows}
              selected={pickSel}
              filter={filter}
              filterActive={filterActive}
              rows={rows}
              currentId={currentId}
              showDeprecated={showDeprecated}
              moveHint={movePicks !== null}
              msg={panelMsg}
            />
          )
          : panelTab === "conversation"
          ? (
            <ConversationTree
              items={convItems}
              selected={forkSel}
              rows={rows}
              showDeprecated={showDeprecated}
              range={rangeAnchor === null
                ? null
                : [Math.min(rangeAnchor, forkSel), Math.max(rangeAnchor, forkSel)]}
            />
          )
          : panelTab === "changes"
          ? (
            <DiffView
              entries={diffEntries}
              fileSel={fileSel}
              scroll={diffScroll}
              rows={rows}
              focused={diffFocus}
            />
          )
          : panelTab === "workflows"
          ? (
            <Workflows
              runs={store.workflows}
              sel={wfSel}
              level={wfLevel}
              run={wfDetail?.run ?? null}
              agents={wfDetail?.agents ?? []}
              phaseSel={wfPhaseSel}
              agentSel={wfAgentSel}
              scroll={wfScroll}
              filter={wfFilter}
              promptOpen={wfPromptOpen}
              rows={rows}
              cols={width}
              lastLog={wfOpenId ? store.wfLogs[wfOpenId] : undefined}
            />
          )
          : panelTab === "jobs"
          ? (
            <Jobs
              jobs={store.jobs}
              sel={jobSel}
              open={jobOpen}
              output={jobText}
              scroll={jobScroll}
              rows={rows}
            />
          )
          : panelTab === "model"
          ? (cfg
            ? (
              <ModelPicker
                cfg={cfg}
                entries={cfgEntries}
                selected={modelSel}
                keyInput={keyInput}
                sessionModel={store.session?.model}
                sessionEffort={store.session?.effort}
                rows={rows}
              />
            )
            : <Text dimColor>loading config…</Text>)
          : (
            <Panel
              tab={panelTab}
              mcp={mcpStat}
              mcpSel={mcpSel}
              mcpMsg={panelMsg}
              skills={skillsList}
              rows={rows}
              theme={themeState}
              themeSel={themeSel}
            />
          )}
        {/* sessions + mcp render panelMsg themselves; the rest show it here. */}
        {panelMsg && panelTab !== "sessions" && panelTab !== "mcp"
          ? <Text color={palette.warn} wrap="truncate">{panelMsg}</Text>
          : null}
      </Box>
    )
    : null;

  const modal = panel ??
    (mode === "help"
      ? <Help rows={rows} width={width} scroll={helpScroll} />
      : mode === "new"
      ? <NewSession query={newQuery} cursor={newComp.cursor} hits={dirHits} selected={newSel} />
      : null);

  return (
    <Box flexDirection="column" height={rows} width={width} backgroundColor={palette.bg}>
      <Box flexDirection="column" flexGrow={1} overflow="hidden">
        {modal ?? (lines.length === 0
          ? (
            // Any empty conversation gets the hint — a freshly created session
            // used to show a blank void while only the pre-creation draft screen
            // said "type to start" (user-testing).
            <>
              {Array.from(
                { length: Math.max(0, Math.floor((bodyH - 5) / 2)) },
                (_, i) => <Text key={`pad-${i}`}>{" "}</Text>,
              )}
              <Box flexDirection="column" alignItems="center">
                <Text>
                  <Text color={palette.accent}>●</Text> <Text bold>bough</Text>
                </Text>
                <Text dimColor>
                  {isDraft
                    ? `new session in ${shortWs}`
                    : sessionDir
                    ? `new conversation in ${sessionDir}`
                    : "new conversation"}
                </Text>
                <Text>{" "}</Text>
                {
                  /* The hint clears once a draft exists — leaving it up read as
                   "my typing didn't register" (visual audit). Blank keeps the
                   block's height so nothing shifts on the first keystroke. */
                }
                {input
                  ? <Text>{" "}</Text>
                  : <Text dimColor>type to start · ^p sessions & new project · ? help</Text>}
              </Box>
              {
                /* The welcome screen has no transcript row for the copy-flash
                 chip to blink over — give it one (draft-cleared feedback). */
              }
              {flash?.on
                ? (
                  <Box justifyContent="flex-end">
                    <Text bold color={palette.accent} backgroundColor={palette.panel}>
                      {" "}
                      {flash.msg}
                      {" "}
                    </Text>
                  </Box>
                )
                : null}
            </>
          )
          : (
            <>
              {Array.from({ length: padTop }, (_, i) => <Text key={`pad-${i}`}>{" "}</Text>)}
              {visible.map((l, i) => {
                // The copy flash blinks over the bottom viewport row — same row,
                // no extra height, so nothing shifts. Off-phases show the line.
                if (flash?.on && i === visible.length - 1) {
                  return (
                    <Box key={`l-${start + i}`} justifyContent="flex-end">
                      <Text bold color={palette.accent} backgroundColor={palette.panel}>
                        {" "}
                        {flash.msg}
                        {" "}
                      </Text>
                    </Box>
                  );
                }
                // Screen row padTop+i+1 (1-based); a live drag paints its span
                // in inverse video; search matches mark theirs (current inverse,
                // the rest underlined).
                const span = drag ? rowSpan(drag, padTop + i + 1) : null;
                const text = span
                  ? highlightSpan(l.text || " ", span.from, span.to)
                  : searchQ && matches.length
                  ? markLine(l.text || " ", matches, start + i, curMatch)
                  : l.text || " ";
                return <Text key={`l-${start + i}`} wrap="truncate">{text}</Text>;
              })}
              {off > 0
                ? (
                  // Chrome, not content: dim base with the arrow+count emphasized.
                  // The % is the viewport TOP's position in the thread (top = 0%),
                  // not the bottom's — the old form never read near 0 when fully
                  // scrolled up (visual audit).
                  <Text>
                    <Text color={palette.info}>↓ {off}</Text>
                    <Text dimColor>
                      {" "}more line{off === 1 ? "" : "s"} below ·{" "}
                      {Math.round((start / Math.max(1, maxOff)) * 100)}%
                    </Text>
                  </Text>
                )
                : null}
            </>
          ))}
      </Box>
      <Box ref={chromeRef} flexDirection="column">
        {mode === "chat"
          ? (
            <>
              {store.queued.map((q, i) => <Text key={i} dimColor>⧖ queued: {q}</Text>)}
              {err ? <Text color={palette.error}>{err}</Text> : null}
              {infoRows.length > 0
                ? (
                  <Box
                    flexDirection="column"
                    borderStyle="round"
                    backgroundColor={palette.panel}
                    borderColor={palette.border}
                    paddingX={1}
                  >
                    <Text bold>conversation</Text>
                    {infoRows.map(([k, v]) => (
                      <Text key={k} wrap="truncate">
                        <Text color={palette.accent}>{k.padEnd(11)}</Text>
                        {v}
                      </Text>
                    ))}
                    <Text dimColor>click a row to copy · esc dismisses</Text>
                  </Box>
                )
                : null}
              {sched
                ? (
                  <Box
                    flexDirection="column"
                    borderStyle="round"
                    backgroundColor={palette.panel}
                    borderColor={palette.border}
                    paddingX={1}
                  >
                    <Text bold>schedules</Text>
                    {sched.form
                      ? (
                        <>
                          {SCHED_FIELDS.map((name, i) => (
                            <Text key={name} wrap="truncate">
                              <Text color={palette.accent}>{name.padEnd(11)}</Text>
                              {sched.form!.fields[i]}
                              {i === sched.form!.focus ? <Text inverse>{" "}</Text> : null}
                            </Text>
                          ))}
                          <Text dimColor>
                            spec: every:{"<N><m|h|d>"}{" "}
                            or daily@HH:MM · enter next / create · esc back
                          </Text>
                        </>
                      )
                      : (
                        <>
                          {sched.scheds.map((s, i) => (
                            <Text key={s.id} inverse={i === sched.sel} wrap="truncate">
                              {s.enabled
                                ? <Text color={palette.accent}>{"● "}</Text>
                                : <Text dimColor>{"○ "}</Text>}
                              {s.title}
                              <Text dimColor>
                                {"  "}
                                {s.spec}{"  "}{s.enabled ? nextIn(s.nextRunAt) : "off"}
                              </Text>
                            </Text>
                          ))}
                          {sched.scheds.length === 0
                            ? <Text dimColor>no schedules — n creates one</Text>
                            : null}
                          <Text dimColor>
                            ↑↓ pick · space toggle · x delete · n new · esc close
                          </Text>
                        </>
                      )}
                    {sched.msg ? <Text color={palette.error}>{sched.msg}</Text> : null}
                  </Box>
                )
                : null}
              {store.busy && store.activity ? <ActivityLine text={store.activity} /> : null}
              {shellOut
                ? (
                  <Box
                    flexDirection="column"
                    borderStyle="round"
                    backgroundColor={palette.panel}
                    borderColor={palette.border}
                    paddingX={1}
                  >
                    <Text wrap="truncate">
                      <Text color={palette.accent}>${" "}</Text>
                      <Text bold>{shellOut.cmd}</Text>
                      {shellOut.code === null
                        ? <Text color={palette.warn}>{"  "}⚙ running…</Text>
                        : shellOut.code === 0
                        ? <Text color={palette.accent}>{"  "}✓</Text>
                        : <Text color={palette.error}>{"  "}✗ exit {shellOut.code}</Text>}
                    </Text>
                    {(() => {
                      // Tail of the output (errors live at the end), capped so
                      // the card can't swallow the viewport.
                      const all = shellOut.out ? shellOut.out.split("\n") : [];
                      // Shrinks with the terminal: a card taller than the
                      // chrome's room makes Ink paint rows over each other
                      // (dropped lines, the label colliding with the echo).
                      const CAP = Math.max(3, Math.min(12, rows - 15));
                      const shown = all.slice(-CAP);
                      const skipped = all.length - shown.length;
                      return (
                        <>
                          {skipped > 0
                            ? (
                              <Text dimColor>
                                … {skipped} earlier line{skipped === 1 ? "" : "s"}
                              </Text>
                            )
                            : null}
                          {shown.map((l, i) => (
                            <Text key={`sh-${i}`} wrap="truncate">{l || " "}</Text>
                          ))}
                        </>
                      );
                    })()}
                    <Text dimColor>local shell · not part of the conversation · esc dismisses</Text>
                  </Box>
                )
                : null}
              {searchQ !== null
                ? (
                  <Text wrap="truncate">
                    <Text color={palette.accent}>⌕{" "}</Text>
                    {searchQ}
                    <Text color={palette.accent}>▌</Text>{"  "}{matches.length
                      ? <Text dimColor>{curMatch + 1}/{matches.length}</Text>
                      : searchQ
                      ? <Text color={palette.warn}>no matches</Text>
                      : <Text dimColor>type to search</Text>}
                    <Text dimColor>{" "}· enter/↓ next · ↑ prev · esc close</Text>
                  </Text>
                )
                : null}
              {popup && !store.ask
                ? (
                  <Box
                    flexDirection="column"
                    borderStyle="round"
                    backgroundColor={palette.panel}
                    borderColor={palette.border}
                    paddingX={1}
                  >
                    {popup.items.length === 0
                      ? <Text dimColor>no matching commands</Text>
                      : popup.items.map((it, i) => {
                        const sel = i === popup.sel;
                        // @ rows: dim the directory prefix so basenames stand
                        // out (skipped on the selected row — dim under the
                        // inverse bar goes illegible).
                        const dimTo = popup.kind === "file" && !sel
                          ? it.label.lastIndexOf("/") + 1
                          : 0;
                        return (
                          <Text key={it.label} inverse={sel} wrap="truncate">
                            <PopupLabel label={it.label} hl={it.hl} dimTo={dimTo} />
                            {it.detail
                              ? (
                                // Selected row: mid-tone, not dim — dim+inverse
                                // collapsed name and description into one
                                // near-black on the bar (visual audit).
                                <Text dimColor={!sel} color={sel ? "#8a919c" : undefined}>
                                  {"  "}
                                  {it.detail}
                                </Text>
                              )
                              : null}
                          </Text>
                        );
                      })}
                    <Text dimColor>
                      {popup.kind === "file"
                        ? "files & dirs — ↑↓ select · tab insert · esc close"
                        : popup.kind === "shell"
                        ? "local shell — ↑↓ pick · enter runs · esc close"
                        : "commands — ↑↓ select · enter runs · esc close"}
                    </Text>
                  </Box>
                )
                : null}
              {store.ask
                ? (
                  <AskCard
                    q={store.ask}
                    count={store.askCount}
                    input={askText}
                    typing={askTyping}
                  />
                )
                : (
                  <Composer
                    input={input}
                    cursor={comp.cursor}
                    busy={store.busy}
                    ghost={ghostText ?? ""}
                    width={width}
                    // A paste must not grow the box past the viewport: cap the
                    // rendered rows at a third of the terminal.
                    maxRows={Math.max(4, Math.floor(rows / 3))}
                  />
                )}
            </>
          )
          : null}
        {
          /* Action feedback (apply results, errors) must show in every mode — an
            apply from the diff panel that reports nowhere reads as a silent no-op. */
        }
        {store.notice ? <Text color={palette.warn} wrap="truncate">{store.notice}</Text> : null}
        {
          /* Live workflow chip(s): one row per running/paused run of this
            conversation, always visible in chat mode — /workflows drills in. */
        }
        {mode === "chat"
          ? store.workflows.filter((w) => w.status === "running" || w.status === "paused")
            .slice(0, 2)
            .map((w) => <WorkflowChip key={w.id} run={w} log={store.wfLogs[w.id]} />)
          : null}
        <StatusBar
          connected={store.connected}
          busy={store.busy}
          session={store.session}
          pendingCount={store.askCount}
          quitHint={quitHint}
          composerEmpty={input === ""}
          mode={mode === "chat" && store.ask ? "approval" : mode}
          usage={store.usage}
          bgJobs={runningJobs}
          draftLabel={isDraft ? `new · ${shortWs}` : null}
          dir={sessionDir}
          model={cfg
            ? (() => {
              // The session's pinned model (and depth) win over the globals.
              const id = store.session?.model ?? cfg.model;
              const label = cfg.models.find((m) => m.id === id)?.label ?? id;
              const effort = store.session?.effort ?? cfg.effort;
              return effort ? `${label} · ${effort}` : label;
            })()
            : null}
          parentTitle={store.session?.kind === "subagent" && store.session.originId
            ? (store.sessions.find((s) => s.id === store.session!.originId)?.title ?? "parent")
              .replace(/^subagent · /, "")
            : null}
        />
        {
          /* Subagents live under the status bar (Claude Code parity): a pinned
            rail, not a transcript card that scrolls away while it works. */
        }
        {mode === "chat" ? <SubagentRail branches={branches} sel={railSel} /> : null}
      </Box>
    </Box>
  );
}
