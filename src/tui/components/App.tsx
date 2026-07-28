/**
 * The composition root: the store on one side, the components on the other, and a
 * keymap in between.
 *
 * THE INVARIANT THIS HOLDS: **this file contains no logic worth testing.** Every
 * decision it appears to make is made somewhere else and imported — what a key
 * means (`keys.ts`), what the transcript looks like (`lines.ts`), what the state
 * is (`store.ts`), what each surface renders (`Chat`, `Composer`, `Tree`,
 * `SubagentRail`, `Workflows`). What is left is `useState` for where the cursor
 * is, a `switch` that turns a `Command` into a store call, and JSX. If this file
 * starts growing, something has been put in the wrong place: the old tree's
 * `App.tsx` was 3,618 lines, and that is the whole reason this milestone was
 * split the way it was.
 *
 * SECOND INVARIANT — **no I/O of its own.** There is no `fetch` here, no
 * `EventSource`, no SSE subscription: the store owns the socket and this component
 * reads it through `useSyncExternalStore`. That is what makes every surface below
 * renderable from a fixture, and it is the property the task's AC names. The one
 * concession is `AppControls` — a handful of injected thunks for the operations
 * the store does not yet expose (workflow steering, delegated drill-in). They are
 * PARAMETERS, supplied by `main.tsx`; this component neither builds a client nor
 * knows a URL. See the note on each field.
 *
 * THIRD — **the mode is derived, not stacked.** `keys.ts` resolves a keypress
 * against exactly one binding set, so there is no chain of handlers a chord falls
 * through and no "did something above me already consume this" question. A live
 * `ask()` hold takes the keyboard away from the composer for as long as it is
 * held, because the card replaces the composer (spec §6) — that is one line here
 * rather than a flag threaded through six components. There is exactly ONE non-chat
 * surface, the panel (spec §15): sessions, tree, changes, workflows, model, MCP,
 * skills and theme are tabs of it, and this file mounts it in one place with one
 * `panel.handle(command)` line. It has no idea which tab is showing.
 *
 * FOURTH — **the transcript is built once.** `buildLines` produces the `VLine[]`
 * that `Chat` paints, and it is memoized here rather than inside `Chat`, because
 * the same array is what a click, a search mark or a drag selection would be
 * addressed against. Two derivations would be two answers to "which row is that".
 */
import { type KeyEvent, TextAttributes } from "@opentui/core";
import { useKeyboard, useRenderer, useTerminalDimensions } from "@opentui/react";
import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import type { ModelRow } from "../../llm/client.ts";
import { api, type SessionRow } from "../api.ts";
import type { Message } from "../../schema/parts.ts";
import type { MouseEvent, NavKey } from "../mouse.ts";
import { buildLines, chatBodyHeight, lineAtSlot, type VLine } from "../lines.ts";
import sliceAnsi from "slice-ansi";
import stripAnsi from "strip-ansi";
import {
  isEmptySelection,
  rowContent,
  rowSpan,
  selRows,
  type Selection,
  selectedCopy,
} from "../selection.ts";
import {
  activeTrigger,
  applyCompletion,
  linkAt,
  meterLine,
  urlAcross,
  rankCompletions,
  sessionLabel,
  shortenPath,
  SPINNER_MS,
  UI,
} from "../format.ts";
import {
  chordOf,
  type Command,
  editLine,
  EMPTY_LINE,
  helpLines,
  insertText,
  isTextInput,
  type KeyContext,
  type KeyFlags,
  type LineState,
  lookup,
  stripCtl,
  type UiMode,
} from "../keys.ts";
import {
  currentAsk,
  isBusy,
  liveUnits,
  marksFor,
  type Store,
  type TuiState,
} from "../store.ts";
import { Chat, type ChatMeter } from "./Chat.tsx";
import { JobOutput, jobBodyRows } from "./JobOutput.tsx";
import { Composer, completionPopupHeight, composerHeight } from "./Composer.tsx";
import { type PanelControls, type PanelHostDeps, usePanelHost } from "./PanelHost.tsx";
import { liveSubagents, SubagentRail } from "./SubagentRail.tsx";
import { forestRows, rewindIndex } from "../forest.ts";

import { palette } from "../theme.ts";
import { tabAtColumn } from "./Panel.tsx";

/** How long a second Escape still counts as a double-tap. */
const DOUBLE_ESC_MS = 600;

/**
 * The operations the store does not expose yet.
 *
 * Every one of these is a REST call the TUI's client already has a method for
 * (`api.ts`) and the store has no action for — workflow steering (spec §8), MCP
 * grants, and the `?originId=` drill-in the tree and the subagent rail are built on.
 * They are injected rather than imported so this component keeps its "no I/O"
 * property and so a test drives them with three lines of fakes. They belong in
 * `store.ts` eventually; that file is not this task's to change. The shape is
 * declared in `PanelHost.tsx`, which is what consumes most of it.
 */
export type AppControls = PanelControls;

/**
 * What the stdin filter (`mouse.ts`) took out of the stream before the renderer
 * saw it.
 *
 * Registration rather than props, because these are *streams* of events and not
 * state: each `on*` takes a handler and returns an unsubscribe. `main.tsx` owns
 * the filter; this component only says what a wheel tick or a paste means.
 */
export interface InputHooks {
  onPaste?: (handler: (text: string) => void) => () => void;
  onMouse?: (handler: (event: MouseEvent) => void) => () => void;
  onNavKey?: (handler: (key: NavKey) => void) => () => void;
}

export interface AppProps {
  store: Store;
  /** Where a new conversation starts — where `bough` was launched. */
  defaultWorkspace?: string;
  /** `$HOME`, so the header can print `~/repos/x` instead of eating the line. */
  home?: string;
  controls?: AppControls;
  input?: InputHooks;
  /**
   * The model picker's catalog. A prop because `llm/client.ts` pulls the provider
   * SDK: `tui/main.tsx` imports it, the component tree does not.
   */
  models?: readonly ModelRow[];
  /**
   * The theme the server served at boot, and the writer that persists a kept one
   * (spec §16). A prop for the same reason `models` is: this is transport, and the
   * composition root owns transport. Absent = the choice lasts for this process.
   */
  theme?: PanelHostDeps["theme"];
  /**
   * How a desktop notification is delivered. `term.ts` detects per-terminal
   * whether that is OSC 9 or a bell and suppresses it while the window is
   * focused; this component only decides WHEN there is something worth saying.
   * A prop for the same reason `theme` is — the composition root owns transport.
   */
  notifyDesktop?: (body: string) => void;
  /**
   * Put text on the system clipboard (OSC 52 — `term.ts`).
   *
   * A prop for the same reason `notifyDesktop` is: this file owns no transport. It
   * decides only WHAT is worth copying; whether that reaches the clipboard over an
   * escape sequence, and whether this terminal even honours it, is `term.ts`'s.
   */
  copyText?: (text: string) => void;
  /**
   * Hand a URL to the OS.
   *
   * bough turns mouse reporting ON (`mouse.ts`), which takes the terminal's own
   * hyperlink hit-testing out of the loop — the click arrives here instead of
   * opening anything. `linkAt` has existed to answer "what is under this column"
   * since OSC 8 links were added; this is the half that acts on the answer.
   */
  openUrl?: (url: string) => void;
  /** Injected so a render is reproducible and the esc double-tap is testable. */
  now?: () => number;
}

/** Rows a wheel tick moves. Three, like every other pager. */
const WHEEL_ROWS = 3;

/**
 * The 1-based screen row the transcript starts on. The header owns row 1 and is a
 * single `<text>`; everything that maps a mouse report to a transcript line goes
 * through this rather than re-deriving it.
 */
const CHAT_TOP = 2;

/**
 * The 1-based screen row the panel's tab titles are painted on: the header owns
 * row 1 and `Panel`'s top border owns row 2.
 */
const PANEL_TABS_ROW = 3;

/** How often the rail re-reads the session's children while a turn is running. */
const RAIL_POLL_MS = 1500;

/**
 * How often the rail's elapsed times advance when no turn is running.
 *
 * A background shell outlives the turn that started it, so the rail needs a clock of
 * its own — but it is showing seconds, not a spinner, and a spinner's 120ms would be
 * eight wakeups a second to move a number once. One clock, two rates.
 */
const RAIL_TICK_MS = 1000;

/** Rows the `?` overlay moves per ↑/↓. A keymap is scanned, not paged. */
const HELP_STEP = 3;

/** Rows the open job's output moves per ↑/↓. Build output is read, not scanned. */
const JOB_STEP = 3;

/**
 * How often the open job's buffer is re-read while it runs.
 *
 * The same second the rail's clock ticks on: this is a `tail -f` and a slower one
 * reads as a frozen screen. It stops the moment the job exits — the fetch that
 * returns an exited row is the last one, so a finished job costs nothing to leave
 * open.
 */
const JOB_POLL_MS = 1000;

/** The named keys `keys.ts` knows by flag rather than by byte. */
const NAMED_KEYS: Record<string, keyof KeyFlags> = {
  up: "upArrow",
  down: "downArrow",
  left: "leftArrow",
  right: "rightArrow",
  pageup: "pageUp",
  pagedown: "pageDown",
  return: "return",
  enter: "return",
  escape: "escape",
  tab: "tab",
  backspace: "backspace",
  delete: "delete",
};

/**
 * One OpenTUI `KeyEvent` in the `(input, key)` shape `keys.ts` reads.
 *
 * `keys.ts` is the keymap and it is heavily tested against ink's `Key` shape, so
 * the terminal's parser is adapted to IT rather than the other way round —
 * `chordOf`/`isTextInput`/`resolve` and their thirty cases are unchanged.
 *
 * Two things are not a rename. `input` is ink's first argument, which for a chord
 * is the BASE character (`^c` → `"c"`), and OpenTUI's `sequence` is the raw byte
 * (`"\x03"`); so a modified key takes its `name` and only unmodified printable
 * text takes its `sequence` — which is what keeps a capital `A` a capital `A`.
 * And macOS reports Option as `option`, which is ink's `meta`.
 *
 * `home`/`end`/`CSI Z`/forward-delete are deliberately absent: the stdin filter
 * (`mouse.ts`) consumes them and dispatches them through `hooks.onNavKey`, so
 * they never reach this handler at all.
 */
function inkKey(event: KeyEvent): { input: string; key: KeyFlags } {
  const key: KeyFlags = {
    ctrl: event.ctrl,
    shift: event.shift,
    meta: event.meta || event.option,
    super: event.super ?? false,
  };
  const named = NAMED_KEYS[event.name];
  if (named) key[named] = true;
  // ESC ESC arrives as ONE event flagged meta (the same pathology ink had). It is
  // an escape, not ⌥esc: the double-tap is recognised below, off the sequence.
  if (event.name === "escape") key.meta = false;

  const printable = event.sequence.length === 1 && event.sequence >= " " &&
    event.sequence !== "\x7f";
  const input = printable && !event.ctrl
    ? event.sequence
    : event.name.length === 1
    ? event.name
    : event.sequence;
  return { input, key };
}

/** ESC ESC in a single read — OpenTUI coalesces it into one event. */
const DOUBLE_ESC_SEQ = "\x1b\x1b";

export function App(
  {
    store,
    defaultWorkspace,
    home,
    controls = {},
    input: hooks = {},
    models,
    theme,
    notifyDesktop,
    copyText,
    openUrl,
    now = Date.now,
  }: AppProps,
) {
  const renderer = useRenderer();
  const state = useSyncExternalStore<TuiState>(store.subscribe, store.getState, store.getState);

  const [mode, setMode] = useState<UiMode>("chat");
  // The draft is kept in a REF as well as in state, and every write goes through
  // `setLine` so the two cannot drift.
  //
  // WHY: a keypress is guarded against the draft (`emptyDraft` decides whether `?`
  // opens the overlay and whether `^d` jumps to the changes tab), and React state
  // does not update until the next render. A terminal delivers a burst — a fast
  // typist, key repeat, a laggy ssh session — as several events processed back to
  // back BEFORE any render, so every one of them read the draft as it was before
  // the burst started. Writing `abc?def` in one chunk therefore opened the help
  // overlay on the fourth character and silently ate the `?`: the composer lost a
  // character the user typed, which is the one thing it may never do. The `?` was
  // just the visible case; `quitArmed` below had the same defect (`^c^c` in one
  // read did not quit), and any future draft-derived guard would inherit it.
  const [line, setLineState] = useState<LineState>(EMPTY_LINE);
  const lineRef = useRef<LineState>(EMPTY_LINE);
  const setLine = useCallback((next: LineState | ((s: LineState) => LineState)) => {
    const value = typeof next === "function" ? next(lineRef.current) : next;
    lineRef.current = value;
    setLineState(value);
  }, []);
  const [scrollOff, setScrollOff] = useState(0);
  const [foldAll, setFoldAll] = useState(false);
  /**
   * Groups the reader opened one at a time, and blocks whose line cap they lifted.
   *
   * `buildLines` has always taken `isExpanded(key)`/`isFull(key)` per group, and
   * both were passed `() => foldAll` — so the only fold control in the product was
   * all-or-nothing and every `click` target `lines.ts` emitted resolved to a state
   * nothing could set. These are that state. `^e` still flips everything at once;
   * it now also clears these, so the global toggle stays the thing that wins.
   */
  /**
   * The drag in progress, or the one that finished and is still highlighted.
   *
   * `selection.ts` has held the whole arithmetic for this — ordering, per-row
   * spans, inverse-video highlighting, clipboard extraction — since it was written,
   * and its only importer was its own test. Turning mouse reporting on is what made
   * this necessary: the terminal's native drag-select never sees the drag, so a
   * transcript you could not select was the price of a transcript you could scroll.
   */
  const [sel, setSel] = useState<Selection | null>(null);
  /** The same value, readable synchronously mid-burst — see the mouse handler. */
  const selRef = useRef<Selection | null>(null);
  /** `run`, reachable from callbacks declared above it. Assigned on every render. */
  const runRef = useRef<((command: Command, input: string) => void) | null>(null);
  /** The screen as it looked when the drag began — see the `down` branch. */
  const paintedRef = useRef<string[]>([]);
  const [openKeys, setOpenKeys] = useState<ReadonlySet<string>>(() => new Set<string>());
  const [fullKeys, setFullKeys] = useState<ReadonlySet<string>>(() => new Set<string>());
  const [railSel, setRailSel] = useState(0);
  /** The rail unit a first `x` armed. Id, not the unit: the row is re-derived. */
  const [armedStop, setArmedStop] = useState<string | null>(null);
  // The open job's scroll, counted up from the tail like the transcript's. Its own
  // state and not `scrollOff`: sharing one offset with the transcript is what makes
  // help open in its middle (see `help.open`), and a job is opened and left far more
  // often than the overlay is.
  const [jobScroll, setJobScroll] = useState(0);
  // Delegated fan-outs drilled into (spec §4's collapse), and conversations whose
  // TURNS are shown. Two sets, because they are two different disclosures on the same
  // row: `⋯ 12 delegated` and "show me this conversation's history".
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set<string>());
  const [openTurns, setOpenTurns] = useState<ReadonlySet<string>>(() => new Set<string>());
  const [children, setChildren] = useState<Record<string, SessionRow[]>>({});
  /**
   * Threads by session id, for conversations OTHER than the open one.
   *
   * The open conversation's thread is `state.thread` and is live; any other has to
   * be fetched, and it is fetched once, when it is first expanded. A tree that
   * pre-fetched every conversation's history would be one `GET` per row on arrival.
   */
  const [threads, setThreads] = useState<Record<string, Message[]>>({});
  const [sent, setSent] = useState<string[]>([]);
  const [histAt, setHistAt] = useState<number | null>(null);
  const [askText, setAskText] = useState("");
  // Same reason as the draft ref above: `^c^c` arriving in ONE read left the second
  // event reading `quitArmed` as false, so the gesture armed the hint twice and
  // never quit. The ref is what the handler reads; the state is what renders.
  // A ref and not state: nothing RENDERS from it — the hint is a store notice — so
  // state bought only the staleness.
  const quitArmedRef = useRef(false);
  const setQuitArmed = useCallback((armed: boolean) => {
    quitArmedRef.current = armed;
  }, []);
  const lastEsc = useRef(0);
  /** A first Escape held while we wait to see whether a second one follows. */
  const escHold = useRef<ReturnType<typeof setTimeout> | null>(null);

  // The size is TRACKED, not sampled once. ink re-rendered the tree for the first
  // resize and then never again, so a terminal that grew left the bottom of the
  // screen unused until restart, and this app had to own the SIGWINCH itself
  // (`term.ts`'s `onResize`). OpenTUI's renderer re-renders on every resize and
  // `useTerminalDimensions` is that signal, so there is exactly ONE subscription
  // here rather than two competing ones.
  // `|| 80`, not `?? 80`: a renderer with no tty reports 0, and a zero-width
  // viewport wraps the transcript one character per line rather than falling back.
  const term = useTerminalDimensions();
  const cols = Math.max(20, term.width || 80);
  const rows = Math.max(8, term.height || 24);

  const ask = currentAsk(state);
  const busy = isBusy(state);

  // A spinner needs a clock, and a clock that runs when nothing is happening is a
  // wakeup per frame forever. This one exists only while something does: a turn
  // (spinner rate) or a background unit still on the rail (one second, which is the
  // resolution of the number it moves).
  const [tick, setTick] = useState(0);
  // Whether anything is running is answered from the RAW sources, never from `units`
  // below — `units` is derived from this clock, and gating the clock on it would be a
  // loop through a memo.
  const railBranches = useMemo(
    () => liveSubagents(children[state.currentId ?? ""] ?? []),
    [children, state.currentId],
  );
  const anyLive = busy || railBranches.length > 0 ||
    state.jobs.some((j) => j.status === "running") ||
    state.workflows.some((w) => w.status === "running" || w.status === "paused");
  useEffect(() => {
    if (!anyLive) return;
    const id = setInterval(() => setTick((t) => t + 1), busy ? SPINNER_MS : RAIL_TICK_MS);
    return () => clearInterval(id);
  }, [anyLive, busy]);
  // THE STORE'S METER IS THE ONLY TURN CLOCK. This used to be a local `busySince` ref
  // stamped when `busy` went true, which meant two answers to "how long has this been
  // running" — and the ref's one was the one that could not also say what the turn had
  // cost, because it knew nothing but a timestamp. `state.turn` carries both.
  const turn = state.turn && state.turn.endedAt === null ? state.turn : null;
  const elapsedMs = turn ? Math.max(0, now() - turn.startedAt) : 0;

  // ---- live delegated work -------------------------------------------------
  // The rail reads `children[currentId]`, and the ONLY thing that ever filled
  // `children` was the tree tab's drill-in. So in chat it was permanently empty:
  // no rail ever rendered, `railLive` was always false, and ↓ — documented as
  // "into the live subagent rail" — did nothing while subagents were running.
  // Delegation is a primary capability (spec §2, §6) and it was invisible.
  //
  // Polled rather than evented because a subagent is a SESSION and its lifecycle
  // arrives as `session.created` on a stream the store does not index by origin.
  //
  // Polled while the TURN runs OR while a child is still on the rail — not while the
  // turn runs alone. A subagent outlives the turn that spawned it, which is the entire
  // reason the rail is pinned (spec §5), so "one pull when the turn stops" caught the
  // agent still busy and then never looked again: the rail read `◆ sleep-1 · running`
  // for as long as the process lived while the server had it `busy:false, interrupted`.
  // Nothing runs invisibly is half the rule; the other half is that nothing on the rail
  // is a ghost. This converges rather than loops — the poll writes `children`, and when
  // the last child settles `railBranches` empties and the interval stops.
  useEffect(() => {
    const id = state.currentId;
    const list = controls.listChildren;
    if (!id || !list) return;
    let alive = true;
    const pull = () =>
      void list(id)
        .then((rows) => alive && setChildren((c) => ({ ...c, [id]: rows })))
        .catch(() => {}); // a failed poll is a stale rail, never an error card
    pull();
    if (!busy && railBranches.length === 0) return () => void (alive = false);
    const timer = setInterval(pull, RAIL_POLL_MS);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, [state.currentId, busy, railBranches.length, controls.listChildren]);

  // ---- the @// completion -------------------------------------------------
  // Every pure piece of this already existed and was tested — `activeTrigger`,
  // `rankCompletions`, `applyCompletion`, `CompletionPopup` — and nothing had ever
  // connected them, so typing `@src/` in bough did nothing at all while every
  // comparable harness completes the path. What was missing is exactly this: a
  // candidate list, a cursor, and four key handlers.
  const [files, setFiles] = useState<string[]>([]);
  // Skills are an install-wide fact, so once per process rather than per session.
  const [skills, setSkills] = useState<{ name: string; description?: string }[]>([]);
  useEffect(() => {
    void api.listSkills()
      .then((r) => setSkills(r.skills))
      .catch(() => {}); // no skills, no `/` rows — never a modal
  }, []);
  const [completionSel, setCompletionSel] = useState(0);
  const [dismissed, setDismissed] = useState(false);
  // Once per session, not per keystroke: the list is thousands of paths and the
  // ranking is local. A session switch invalidates it.
  useEffect(() => {
    setFiles([]);
    let live = true;
    // A conversation that has not run a turn has no session id — and that is the
    // screen where someone first types `@`. Fall back to the workspace it WOULD
    // start in, or the popup opens with zero rows and reads as broken.
    const pull = state.currentId
      ? api.listFiles(state.currentId)
      : defaultWorkspace
      ? api.listFilesIn(defaultWorkspace)
      : null;
    if (!pull) return;
    void pull
      .then((r) => live && setFiles(r.files))
      .catch(() => {}); // no repo, no candidates — the popup simply stays closed
    return () => {
      live = false;
    };
  }, [state.currentId, defaultWorkspace]);

  const trigger = useMemo(
    () => (dismissed ? null : activeTrigger(line.text, line.cursor)),
    [line.text, line.cursor, dismissed],
  );
  const completion = useMemo(() => {
    if (!trigger) return { items: [], total: 0 };
    const candidates = trigger.kind === "file"
      ? files.map((name) => ({ name }))
      : skills.map((sk) => ({ name: sk.name, detail: sk.description ?? "" }));
    return rankCompletions(candidates, trigger);
  }, [trigger, files, skills]);
  const completing = completion.items.length > 0;
  // The cursor must never point past a list that just got shorter as you typed.
  const selAt = completing ? Math.min(completionSel, completion.items.length - 1) : 0;

  // A conversation you are NOT looking at finished its turn. The store has computed
  // this since the rewrite and nothing read it, so the answer to "is the thing I
  // started in the other session done" was to go and look. `seq` rather than the id
  // is the dependency: the same session finishing twice is two pieces of news.
  const backgroundSeq = state.background?.seq ?? 0;
  useEffect(() => {
    const done = state.background;
    if (!done || backgroundSeq === 0) return;
    const line = `✓ ${done.title} finished — ^s to open it`;
    store.notify(line);
    notifyDesktop?.(`${done.title} finished`);
    // `store` and the title are read through the event that changed `seq`; adding
    // them here would re-announce on any unrelated store identity change.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [backgroundSeq]);
  // THE LIVE-WORK RAIL (spec §5: nothing runs invisibly). Shells, delegated agents and
  // workflow runs, each as one row with its own elapsed and its own tokens. `liveUnits`
  // is pure and ordered so a row never moves under the cursor; `tick` is in the deps
  // because elapsed is measured against the clock, not against the data.
  const units = useMemo(
    () =>
      liveUnits({
        jobs: state.jobs,
        subagents: railBranches,
        workflows: state.workflows,
        now: now(),
      }),
    [state.jobs, railBranches, state.workflows, tick],
  );
  // A RUNNING shell is on the rail, so its card would be the same fact twice — and the
  // copy at the tail of the transcript is the one you can only see while scrolled to
  // the bottom. Exited jobs stay: an outcome belongs in the transcript.
  const exitedJobs = useMemo(
    () => state.jobs.filter((j) => j.status !== "running"),
    [state.jobs],
  );
  // THE RAIL IS NOT A MODE YOU CAN BE STRANDED IN. Work finishes on its own — and now
  // it can also be killed with `x` from inside the rail — so the row under the cursor
  // can be the last one there is. Without this the rail renders nothing, `mode` is
  // still "rail", and every subsequent keystroke is swallowed by a surface that is not
  // on screen: the composer looks focused and eats what you type.
  useEffect(() => {
    if (units.length === 0) {
      setMode((m) => (m === "rail" ? "chat" : m));
      setRailSel(0);
      return;
    }
    setRailSel((i) => Math.min(i, units.length - 1));
  }, [units.length]);

  // THE OPEN JOB, FOLLOWED. A background shell publishes no output events — the
  // buffer lives in the registry and is read over HTTP (`server/jobs.ts`) — so the
  // only way for an open job view to be live is to re-read it. Running only: the
  // fetch that first sees an exit is the last one, and a finished job then sits
  // there costing nothing.
  const jobRunning = state.jobView?.job?.status === "running";
  // The job view is not a mode you can be stranded in either — the rail's rule. A
  // session switch clears `jobView` from under it (`store.ts`), and a mode with no
  // surface on screen swallows every keypress into a composer that only looks focused.
  useEffect(() => {
    if (mode === "job" && !state.jobView) setMode("chat");
  }, [mode, state.jobView]);
  useEffect(() => {
    if (mode !== "job" || !jobRunning) return;
    const timer = setInterval(() => void store.refreshJob(), JOB_POLL_MS);
    (timer as { unref?: () => void }).unref?.();
    return () => clearInterval(timer);
  }, [mode, jobRunning, store]);

  // Memoized on the LEDGER, not on `state`: `marksFor` filters, so a fresh array every
  // render would rebuild the whole transcript on every keystroke.
  const marks = useMemo(() => marksFor(state, state.currentId), [state.marks, state.currentId]);
  const lines = useMemo(
    () =>
      buildLines(
        state.thread,
        (key) => foldAll || openKeys.has(key),
        (key) => foldAll || fullKeys.has(key),
        cols,
        {
          streaming: state.streaming,
          toolLogs: state.toolLogs,
          jobs: exitedJobs,
          // Every run of this session, not just the live ones: the card's whole
          // purpose is that a finished run still reads its outcome in place.
          runs: state.workflows,
          marks,
          now: now(),
        },
      ),
    [
      state.thread,
      state.streaming,
      state.toolLogs,
      exitedJobs,
      state.workflows,
      marks,
      cols,
      foldAll,
      openKeys,
      fullKeys,
    ],
  );
  // The open conversation's thread comes from the store — it is live, and a copy in
  // `threads` would go stale the moment a turn appended.
  const allThreads = useMemo(
    () => (state.currentId ? { ...threads, [state.currentId]: state.thread } : threads),
    [threads, state.currentId, state.thread],
  );

  /**
   * Show a conversation's turns, fetching its thread the first time.
   *
   * The open conversation needs no fetch (`allThreads` above), and a second expand
   * of the same session needs none either — a thread is append-only, and the rows
   * this feeds are history.
   */
  const expand = useCallback((sessionId: string) => {
    setOpenTurns((set) => new Set([...set, sessionId]));
    if (sessionId === state.currentId || threads[sessionId]) return;
    void api.getSession(sessionId)
      .then((snap) => setThreads((t) => ({ ...t, [sessionId]: snap.thread })))
      .catch(() => store.notify("could not read that conversation's history"));
  }, [threads, state.currentId, store]);

  const collapseTurns = useCallback((sessionId: string) => {
    setOpenTurns((set) => new Set([...set].filter((x) => x !== sessionId)));
  }, []);

  const drillIn = useCallback((originId: string) => {
    setExpanded((set) => new Set([...set, originId]));
    if (children[originId] || !controls.listChildren) return;
    void controls.listChildren(originId)
      .then((rows) => setChildren((c) => ({ ...c, [originId]: rows })))
      .catch(() => store.notify("could not load delegated work for that conversation"));
  }, [children, controls, store]);

  const collapse = useCallback((originId: string) => {
    setExpanded((set) => new Set([...set].filter((x) => x !== originId)));
  }, []);

  // The one non-chat surface (spec §15). Eight tabs, one hook, no logic here.
  /**
   * pi's `/tree` selection, executed. Branch at the message, open the branch, and
   * — for a user turn — put its text back in the composer so the re-send IS the
   * new branch. `api` rather than a `controls` thunk because this needs `setLine`,
   * which only this component has.
   */
  const forkAt = useCallback(
    async (
      sessionId: string,
      body: { atMessageId: string; exclusive?: boolean; summarizeAbandoned?: boolean },
      editorText?: string,
    ) => {
      try {
        const res = await api.fork(sessionId, body);
        if (editorText) setLine({ text: editorText, cursor: editorText.length });
        await store.open(res.session.id);
      } catch (error) {
        store.notify(error instanceof Error ? error.message : String(error));
      }
    },
    [store],
  );

  // ---- the frame -----------------------------------------------------------
  // Three fixed regions and one growing one: header, then the growing region
  // (transcript or panel), then the rail, the composer and the status row pinned
  // to the bottom. Every fixed region reports its OWN height — `composerHeight`
  // and `completionPopupHeight` mirror `Composer`'s render, the rail is its rows
  // plus its hint — so the growing one is exactly what is left. The old math
  // reserved a quarter of the screen for the composer and then subtracted a
  // further constant 4, which left six unpainted rows under the status line at
  // 34 rows: the input bar was not pinned to anything.
  const composerRows = Math.min(8, Math.max(3, Math.floor(rows / 4)));
  const railH = units.length === 0 ? 0 : units.length + (mode === "rail" ? 0 : 1);
  const boxH = composerHeight({ input: line.text, busy, width: cols, maxRows: composerRows });
  const popupH = trigger
    ? completionPopupHeight(
      completion.items.length,
      Math.max(0, completion.total - completion.items.length),
    )
    : 0;
  // An `ask()` takes the composer's place: prompt + options + the typed row + the
  // legend, inside a border.
  const inputH = ask ? 4 + (ask.options?.length ?? 0) : boxH + popupH;
  // Hoisted out of the JSX because the click hit-test needs the same number the
  // renderer lays out with; two copies would put a click one row off its row.
  const chatH = Math.max(1, rows - 1 /* header */ - railH - inputH - 1 /* status */);

  // The panel keeps the pinned rows below it (rule 4: state lives in one row), so
  // it gets the screen minus them. `Panel` subtracts its own chrome twice on the
  // way down (`PanelHost` takes 4, `Panel` takes 2 more) and draws `rows - 2`, so
  // it is handed two more than the region it must fit inside.
  const panelBodyH = Math.max(
    1,
    rows - 1 /* header */ - railH - boxH - 1 /* status */ - (state.replay ? 1 : 0) -
      (state.notice ? 1 : 0),
  );
  const panelRows = panelBodyH + 2;

  const panel = usePanelHost({
    store,
    state,
    rows: panelRows,
    cols,
    now: now(),
    controls: { ...controls, forkAt },
    models,
    ...(theme ? { theme } : {}),
    // The raw material for the ONE tree. `PanelHost` folds it into rows because it
    // owns the `/` filter that narrows them (`forest.ts`).
    forest: {
      sessions: state.sessions,
      childrenByOrigin: children,
      threads: allThreads,
      expanded: openTurns,
      drilled: expanded,
    },
    expand,
    collapseTurns,
    drillIn,
    collapse,
  });

  // A held question owns the keyboard: the card replaces the composer (spec §6).
  // The panel outranks both — while it is open it IS the surface with the keyboard.
  const uiMode: UiMode = panel.open ? "panel" : mode === "chat" && ask ? "ask" : mode;

  /**
   * Dispatch a click on a screen row.
   *
   * `lines.ts` has emitted a `click` target on every foldable group, every capped
   * block and every branch card since the transcript was written, and its own
   * comments record that nothing ever read them — a line that said "click to open"
   * was "an instruction to do something impossible". This is the reader.
   *
   * The screen is three stacked regions and the arithmetic is theirs, not this
   * function's: the header owns row 1, `Chat` owns the next `chatH`, the rail owns
   * `units.length` after that. `chatBodyHeight`/`lineAtSlot` are shared with the
   * renderer, so a click cannot land one row off the row that was drawn.
   */
  /**
   * The styled text painted on 1-based screen row `y`, or null where nothing is.
   *
   * The one place that answers "what is on that row", shared by the link hit-test,
   * the highlight and the clipboard extraction — three readers that must agree, or
   * you copy one row and see another highlighted.
   */
  const rowAt = useCallback((y: number): VLine | null => {
    const slot = y - CHAT_TOP;
    if (panel.open || slot < 0 || slot >= chatH) return null;
    const body = chatBodyHeight(chatH, state.queued.length, Boolean(state.notice));
    return lineAtSlot(lines, body, scrollOff, slot);
  }, [panel.open, chatH, state.queued.length, state.notice, lines, scrollOff]);

  /**
   * Every row currently PAINTED, read back off the renderer as plain text.
   *
   * `rowAt` above answers from the transcript, which is the only surface whose
   * lines this file holds — so a drag over the panel, the rail or the composer
   * copied nothing at all. This answers from the screen instead, so a selection
   * works on every surface without each one having to hand its rows up.
   *
   * Read ONCE per gesture rather than per row: it decodes the whole grid, and
   * `selectedText` asks per row.
   */
  const screenRows = useCallback((): string[] => {
    const buffer = (renderer as unknown as {
      currentRenderBuffer?: { getRealCharBytes?: (trim: boolean) => Uint8Array };
    })?.currentRenderBuffer;
    const bytes = buffer?.getRealCharBytes?.(true);
    return bytes ? new TextDecoder().decode(bytes).split("\n") : [];
  }, [renderer]);

  /**
   * The selection, painted.
   *
   * `Chat` addresses a row by its index in `lines`, and a selection is addressed by
   * SCREEN row — so the index is converted back rather than the selection being
   * stored in transcript coordinates. That is deliberate: a drag is a gesture on
   * the screen, and storing it against the transcript would make it slide when new
   * output arrives underneath it, highlighting text nobody selected.
   */
  const highlight = useMemo(() => {
    if (!sel || isEmptySelection(sel)) return null;
    const painted = paintedRef.current;
    const [top, bottom] = selRows(sel);
    const rows: { y: number; from: number; text: string }[] = [];
    for (let y = top; y <= bottom; y++) {
      const span = rowSpan(sel, y);
      const line = painted[y - 1];
      if (!span || !line) continue;
      const text = span.to === Infinity
        ? sliceAnsi(line, span.from)
        : sliceAnsi(line, span.from, span.to);
      // A drag past end-of-line selects nothing on that row; an empty inverse run
      // renders as a blip in some terminals.
      if (text.trim()) rows.push({ y, from: span.from, text });
    }
    return rows;
  }, [sel]);

  /**
   * The selection, drawn as one absolutely-positioned layer over whatever is
   * underneath.
   *
   * ONE mechanism for every surface. The first cut highlighted through `Chat`'s
   * `decorate` hook, which only the transcript has — so the panel, the rail and the
   * composer could be dragged over and copied but never showed what was selected.
   * Threading a decorator into all nine tab bodies would have been nine chances to
   * get it inconsistent; an overlay is addressed in screen coordinates, which is
   * what a drag already is.
   */
  const SelectionLayer = () =>
    highlight === null ? null : (
      <>
        {highlight.map((r) => (
          <box
            key={`sel-${r.y}`}
            style={{ position: "absolute", left: r.from, top: r.y - 1, zIndex: 100 }}
          >
            {/*
              EXPLICIT COLOURS, not `INVERSE`. Inverting is what a terminal's own
              selection does, but it needs something to invert: this overlay draws
              onto a fresh renderable with no colours of its own, and OpenTUI
              resolved both sides to white — so every selected cell came out
              #ffffff on #ffffff and the text you were dragging over vanished. The
              cells reported `inverse: true` the whole time, which is why counting
              the attribute was not enough to call this verified.
            */}
            <text fg={palette.bg} bg={palette.accent} wrapMode="none">
              {stripAnsi(r.text)}
            </text>
          </box>
        ))}
      </>
    );

  const clickAt = useCallback((y: number, x = 1) => {
    if (panel.open) {
      // The strip is the one thing under the panel that a click can reach. Its row
      // is fixed: the header owns row 1, `Panel`'s border owns row 2, the titles
      // are row 3, and the border also costs one column on the left.
      if (y === PANEL_TABS_ROW) {
        const hit = tabAtColumn(panel.tab, x - 1 - 1);
        // Through a ref: `run` is declared below this, and a click is dispatched
        // long after both exist. Same arrangement as `lineRef` and `selRef`.
        if (hit) runRef.current?.(`tab.${hit}` as Command, "");
      }
      return;
    }
    if (y >= CHAT_TOP && y < CHAT_TOP + chatH) {
      const body = chatBodyHeight(chatH, state.queued.length, Boolean(state.notice));
      const target = lineAtSlot(lines, body, scrollOff, y - CHAT_TOP)?.click;
      if (!target) return;
      // A branch card descends; it does not fold. Same route the rail's ⏎ takes.
      if (target.startsWith("open:")) {
        const id = target.slice("open:".length);
        void store.open(id).catch(() => store.notify("could not open that branch"));
        return;
      }
      // A job card opens that job's output — the same surface ⏎ reaches from the
      // rail, and the only route to a job that has already exited off it.
      if (target.startsWith("job:")) {
        const [, sessionId, jobId] = target.split(":");
        if (!sessionId || !jobId) return;
        setJobScroll(0);
        setMode("job");
        void store.openJob(jobId, sessionId);
        return;
      }
      // A workflow card opens that run's view. The run is detached and off the live
      // rail the moment it ends, so the card is the only door left to its phases,
      // its per-agent cost and its replay accounting.
      if (target.startsWith("workflow:")) {
        panel.openRun(target.slice("workflow:".length));
        return;
      }
      // "+N more lines" lifts the cap and stays lifted — re-capping it is `^e`.
      if (target.endsWith("!full")) {
        const base = target.slice(0, -"!full".length);
        return setFullKeys((s) => new Set([...s, base]));
      }
      return setOpenKeys((s) => {
        const next = new Set(s);
        if (!next.delete(target)) next.add(target);
        return next;
      });
    }
    // Live work: select the unit you clicked and give it the keyboard, so the row's
    // own legend (`⏎ open · x stop`) is true the moment it lights up.
    const railTop = CHAT_TOP + chatH;
    if (y >= railTop && y < railTop + units.length) {
      setRailSel(y - railTop);
      setMode("rail");
    }
  }, [panel.open, panel.tab, chatH, state.queued.length, state.notice, lines, scrollOff, units.length, store]);

  // The history cursor is a POSITION IN `sent`, and it only means anything while the
  // draft is holding a prompt taken from there. Every route back to an empty composer
  // — ^u, ^w down to nothing, backspace, esc esc, send — ends the browse, so the next
  // ↑ is "my last prompt" again rather than wherever browsing happened to stop. Only
  // `draft.clear` and `submit` reset it before, so the most common gesture of all
  // (clear the line, ↑ to re-run) came back a prompt or two too far.
  //
  // An effect on the draft rather than a case per command: line editing is one
  // functional `setLine` in `run`'s default arm, and reading `line` there would
  // rebuild that callback — and re-subscribe the paste/mouse hooks — every keystroke.
  useEffect(() => {
    if (line.text === "") setHistAt(null);
  }, [line.text]);

  const submit = useCallback((queue: boolean) => {
    const text = line.text.trim();
    if (text === "") return;
    setLine(EMPTY_LINE);
    setHistAt(null);
    setSent((h) => [...h, text]);
    setScrollOff(0);
    if (state.currentId) void store.send(text, { queue });
    else void store.createSession(defaultWorkspace).then((s) => s && store.send(text));
  }, [line.text, state.currentId, defaultWorkspace, store]);

  /** One command → one effect. Nothing here decides anything; it only dispatches. */
  const run = useCallback((command: Command, input: string) => {
    // The panel answers first and says so: every `panel.*`, `tab.*`, list-navigation
    // and workflow-steering command is its own, and none of them appears below. The
    // RAW keypress rides along because `panel.pick` needs the digit — a `1` and a `7`
    // resolve to the same command, exactly as they do for `ask.pick`.
    if (panel.handle(command, input)) return;
    const page = Math.max(1, rows - 8);
    const last = (n: number) => Math.max(0, n - 1);
    switch (command) {
      case "quit.arm":
        setQuitArmed(true);
        return store.notify("^c again to quit — subagents and workflows keep running");
      case "quit":
        // Deferred: `destroy()` synchronously tears down renderables the React
        // reconciler is still holding. `main.tsx` awaits the renderer's
        // `onDestroy` in place of ink's `waitUntilExit`, so this IS the exit.
        return queueMicrotask(() => renderer.destroy());
      // The overlay and the transcript share `scrollOff`, so entering or leaving
      // help must rewind it — otherwise a scrolled-back transcript opens the
      // overlay somewhere in its middle, and vice versa.
      case "help.open":
        setScrollOff(0);
        return setMode("help");
      case "help.close":
        setScrollOff(0);
        return setMode("chat");

      case "send":
        return submit(false);
      case "send.queue":
        return submit(true);
      case "draft.clear":
        setHistAt(null);
        return setLine(EMPTY_LINE);
      case "cancel":
        setScrollOff(0);
        return store.dismissNotice();
      // Spec §5. The keymap only routes this while a turn is running (`keys.ts`), so
      // there is no busy check here — the guard is the binding.
      case "turn.interrupt":
        return void store.interrupt();
      case "history.prev": {
        if (sent.length === 0) return;
        const at = histAt === null ? last(sent.length) : Math.max(0, histAt - 1);
        setHistAt(at);
        return setLine({ text: sent[at], cursor: sent[at].length });
      }
      case "history.next": {
        if (histAt === null) return;
        const at = histAt + 1;
        if (at >= sent.length) {
          setHistAt(null);
          return setLine(EMPTY_LINE);
        }
        setHistAt(at);
        return setLine({ text: sent[at], cursor: sent[at].length });
      }

      case "complete.accept": {
        const item = completion.items[selAt];
        if (!trigger || !item) return;
        const next = applyCompletion(line.text, trigger, item);
        setCompletionSel(0);
        return setLine(next);
      }
      case "complete.prev":
        return setCompletionSel((i) => Math.max(0, i - 1));
      case "complete.next":
        return setCompletionSel((i) => Math.min(completion.items.length - 1, i + 1));
      case "complete.dismiss":
        // Stays dismissed until the trigger token changes, so esc means esc.
        return setDismissed(true);

      case "fold.all":
        // The global toggle wins: flipping it drops the per-group state, so `^e`
        // twice is a reset rather than a return to whatever was open before.
        setOpenKeys(new Set());
        setFullKeys(new Set());
        return setFoldAll((v) => !v);

      case "session.out": {
        const origin = state.session?.originId;
        if (!origin) return;
        void store.open(origin).catch(() => store.notify("could not reopen the spawning session"));
        return;
      }
      // Two surfaces, opposite senses. The transcript's offset counts BACKWARDS
      // from the bottom, so scrolling up raises it; the overlay is a document read
      // top-down, so scrolling up lowers it. Same command, because it is the same
      // key doing the same thing to whatever is on screen.
      case "scroll.up":
        if (uiMode === "job") return setJobScroll((o) => o + JOB_STEP);
        if (uiMode === "help") return setScrollOff((o) => Math.max(0, o - HELP_STEP));
        return setScrollOff((o) => Math.min(Math.max(0, lines.length - 1), o + page));
      case "scroll.down":
        // No upper clamp here and none needed: `JobOutput` clamps to its own buffer,
        // which is the only thing that knows how many lines there are.
        if (uiMode === "job") return setJobScroll((o) => Math.max(0, o - JOB_STEP));
        if (uiMode === "help") {
          const max = Math.max(0, helpLines().length - Math.max(1, rows - 2));
          return setScrollOff((o) => Math.min(max, o + HELP_STEP));
        }
        return setScrollOff((o) => Math.max(0, o - page));
      // A SCREEN rather than a step. ↑↓ scan the overlay three rows at a time, which
      // put its last section — `won't do`, where the no-sandbox posture is stated —
      // forty keypresses from the top, on the one surface that advertises pgup/pgdn.
      case "scroll.pageUp":
        if (uiMode === "job") return setJobScroll((o) => o + jobBodyRows(chatH));
        if (uiMode === "help") return setScrollOff((o) => Math.max(0, o - page));
        return setScrollOff((o) => Math.min(Math.max(0, lines.length - 1), o + page));
      case "scroll.pageDown":
        if (uiMode === "job") {
          return setJobScroll((o) => Math.max(0, o - jobBodyRows(chatH)));
        }
        if (uiMode === "help") {
          const max = Math.max(0, helpLines().length - Math.max(1, rows - 2));
          return setScrollOff((o) => Math.min(max, o + page));
        }
        return setScrollOff((o) => Math.max(0, o - page));

      // The rail's cursor moving is also the arming being dropped: a confirmation
      // that outlives the row it was read on is not a confirmation.
      case "rail.enter":
        setRailSel(0);
        setArmedStop(null);
        return setMode("rail");
      case "rail.up":
        setArmedStop(null);
        return railSel === 0 ? setMode("chat") : setRailSel((i) => i - 1);
      case "rail.down":
        setArmedStop(null);
        return setRailSel((i) => Math.min(last(units.length), i + 1));
      case "rail.open": {
        const target = units[railSel];
        setArmedStop(null);
        if (!target) return;
        // A SHELL opens its output, which is what the row is about and what the
        // legend has always promised ("open this agent / shell output"). It used to
        // open the owning session instead — for the session's own shells that is the
        // screen you are already on, so ⏎ on a background job did nothing at all,
        // and the only route to the buffer was asking the model to read it back.
        if (target.kind === "shell") {
          setJobScroll(0);
          setMode("job");
          return void store.openJob(target.id, target.sessionId);
        }
        setMode("chat");
        return void store.open(target.sessionId);
      }
      // Back to the rail, not to chat: you opened this to glance at it.
      case "job.close":
        setArmedStop(null);
        store.closeJob();
        return setMode(units.length > 0 ? "rail" : "chat");
      // The rail's two-step, on the job you are watching — same letter, same arm,
      // same record (spec §7). A job that has already exited has nothing to kill.
      case "job.stop": {
        const view = state.jobView;
        if (!view || view.job?.status !== "running") return;
        if (armedStop !== view.id) {
          setArmedStop(view.id);
          return store.notify(`x again to kill ${view.job.name || view.id}`);
        }
        setArmedStop(null);
        return void store.stopUnit({
          kind: "shell",
          id: view.id,
          sessionId: view.sessionId,
          title: view.job.name || view.id,
          elapsedMs: 0,
          tokens: null,
          costUsd: null,
          progress: null,
          detail: view.job.command,
        });
      }
      case "rail.exit":
        setArmedStop(null);
        return setMode("chat");
      // Spec §7: consent is never inferred. The first press NAMES what dies, the
      // second one kills it, and `stopUnit` records the kill where a toast cannot
      // expire it.
      case "rail.stop": {
        const u = units[railSel];
        if (!u) return;
        if (armedStop !== u.id) {
          setArmedStop(u.id);
          return store.notify(
            u.kind === "shell"
              ? `x again to kill ${u.title} — ${u.detail ?? "background shell"}`
              : `x again to stop ${u.title} — work in flight is lost`,
          );
        }
        setArmedStop(null);
        return void store.stopUnit(u);
      }

      case "ask.pick": {
        const choice = ask?.options?.[Number(input) - 1];
        if (!choice) return;
        setAskText("");
        return void store.answerAsk(choice);
      }
      case "ask.send": {
        if (askText.trim() === "") return;
        const answer = askText;
        setAskText("");
        return void store.answerAsk(answer);
      }
      case "ask.decline":
        setAskText("");
        return void store.declineAsk();

      default:
        // Everything left is line editing, which is a pure function of the draft.
        return setLine((s) => editLine(s, command));
    }
  }, [
    armedStop,
    ask,
    askText,
    renderer,
    histAt,
    lines.length,
    uiMode,
    panel,
    units,
    railSel,
    rows,
    sent,
    store,
    submit,
  ]);

  // A paste, a wheel tick and the Home/End keys the filter takes off the stream
  // before the renderer's own parser can see them (`mouse.ts`). Registered
  // once; each handler is one line, because none of them is a decision.
  useEffect(() => {
    const off = [
      hooks.onPaste?.((text) => setLine((s) => insertText(s, stripCtl(text)))),
      hooks.onMouse?.((event) => {
        if (event.kind === "wheel-up") return setScrollOff((o) => o + WHEEL_ROWS);
        if (event.kind === "wheel-down") return setScrollOff((o) => Math.max(0, o - WHEEL_ROWS));
        const at = { x: event.x, y: event.y };
        // READ AND WRITTEN THROUGH A REF, not through the state alone. A drag is a
        // burst — down, drag, drag, …, up — and React batches the whole burst before
        // it renders, so an `up` reading `sel` from the closure would see the value
        // from before the press. Same reason `lineRef` and `quitArmedRef` exist.
        // The side effects below MUST NOT live in a `setSel` updater: an updater is
        // required to be pure, and React is free to call it twice or defer it.
        const write = (next: Selection | null) => {
          selRef.current = next;
          setSel(next);
        };
        // A press opens a selection rather than acting. Which gesture it turns out
        // to be is not knowable until the button comes back up: a click and the
        // first cell of a drag are the same event.
        if (event.kind === "down") {
          // SNAPSHOT NOW, before the overlay exists. The highlight is drawn as a
          // layer on top, so re-reading the screen mid-drag would read bough's own
          // inverse video back and highlight the highlight. The content under a
          // drag does not change while the button is held, so one read is right.
          paintedRef.current = screenRows();
          return write({ anchor: at, focus: at });
        }
        if (event.kind === "drag") {
          const s = selRef.current;
          return s ? write({ ...s, focus: at }) : undefined;
        }
        if (event.kind !== "up") return;
        const s = selRef.current;
        if (s && !isEmptySelection({ ...s, focus: at })) {
          // A DRAG. Copy on release, the way a terminal's own selection does —
          // requiring a second keystroke to keep what you just highlighted is a
          // step nobody expects.
          //
          // The TRANSCRIPT's own styled rows first, because they carry the ANSI a
          // span is sliced out of; anything the transcript does not own — the panel
          // and its tabs, the rail, the composer, the status row — falls back to
          // what is painted on screen. That fallback is what makes a drag over the
          // mcp tab copy the mcp tab.
          // The TRANSCRIPT's own line first, because it carries the unwrapped
          // source a paste should actually contain; anything the transcript does
          // not own — the panel, the rail, the composer — has only what is painted,
          // and that is answered from the snapshot.
          const painted = paintedRef.current;
          const text = selectedCopy({ ...s, focus: at }, (y) => {
            const line = rowAt(y);
            if (line) return line;
            const row = painted[y - 1];
            return row === undefined ? null : { text: row };
          });
          if (text.trim()) {
            copyText?.(text);
            store.notify(`copied ${text.length} character${text.length === 1 ? "" : "s"}`);
          }
          // AND DROPPED. The highlight is stored in SCREEN coordinates, and the
          // notice this just raised takes a row from the transcript — so keeping it
          // would slide every row under it up by one and leave the inverse video
          // sitting on text nobody selected until the notice expired. The notice is
          // the feedback; the highlight has done its job.
          return write(null);
        }
        write(null);
        // A CLICK. Links first: a URL under this exact column beats the fold the row
        // belongs to, because a URL is the more specific thing to have aimed at.
        //
        // TWO READINGS, because there are two kinds of link on screen. The
        // transcript emits OSC 8, so `linkAt` answers from the markers. Everything
        // else — a panel message, a rail row, a job card — is plain text, and
        // `urlAcross` reads the characters and rejoins the rows a long URL was
        // wrapped onto. Without the second, the mcp tab's authorization link — the
        // one URL in the product nobody can retype — was the only thing on screen
        // that looked like a link and was not one.
        if (openUrl) {
          const marked = linkAt(rowAt(at.y)?.text ?? "", at.x - 1);
          if (marked) {
            openUrl(marked);
            return;
          }
          const rows = paintedRef.current.map(rowContent);

          const here = rows[at.y - 1];
          const bare = here
            ? urlAcross(rows.map((r) => r.content), at.y - 1, at.x - 1 - here.offset)
            : null;
          if (bare) {
            openUrl(bare);
            return;
          }
        }
        clickAt(at.y, at.x);
      }),
      hooks.onNavKey?.((key) => {
        // Backtab is not line editing: it is the panel's "previous tab". No
        // keypress reaches it — the stdin filter eats `CSI Z` (`mouse.ts`).
        if (key === "shiftTab") return void run("panel.prev", "");
        // `chordOf` cannot carry this one: macOS Backspace is reported as
        // `key.delete`, so the flag is not the key (`mouse.ts`).
        if (key === "forwardDelete") return setLine((s) => editLine(s, "delete.forward"));
        setLine((s) =>
          editLine(s, key === "home" || key === "cmdHome" ? "cursor.home" : "cursor.end")
        );
      }),
    ];
    return () => {
      for (const stop of off) stop?.();
    };
    // `clickAt` closes over the transcript, the scroll offset and the geometry, so
    // it MUST be a dep: a stale one would hit-test the screen as it was when the
    // listener was registered and fold a group the user is no longer looking at.
  }, [hooks, run, clickAt, rowAt, screenRows, copyText, openUrl, store]);
  runRef.current = run;

  useKeyboard((event) => {
    const { input, key } = inkKey(event);
    const chord = chordOf(input, key);
    if (chord !== "ctrl+c" && quitArmedRef.current) setQuitArmed(false);

    // The two guards derived from React state are read from their REFS, because a
    // burst of keypresses is processed before any of them re-renders. See `lineRef`.
    const ctxWith = (doubleEsc: boolean): KeyContext => ({
      mode: uiMode,
      // The panel's open tab is the SCOPE a bare letter resolves in (`keys.ts`): `x`
      // stops a run in `workflows` and arms a revert in `changes`. Omitting it is a
      // safe degrade — every tab-local letter resolves to nothing — which is exactly
      // why it must be passed.
      tab: panel.open ? panel.tab : null,
      // While the panel's filter buffer has the keyboard, every bare letter is text —
      // otherwise typing "opus" into the model tab pauses a workflow on the way past.
      panelFiltering: panel.filtering,
      emptyDraft: lineRef.current.text === "",
      // Only true when there is somewhere to go back to, so `←` keeps meaning the
      // cursor in a root session.
      inSubagent: Boolean(state.session?.originId),
      multiline: lineRef.current.text.includes("\n"),
      busy,
      doubleEsc,
      quitArmed: quitArmedRef.current,
      railLive: units.length > 0,
      completing,
    });

    // ---- Escape: the one chord whose meaning depends on the NEXT keypress ----
    // Two escapes 250ms apart used to mean something different from two escapes in
    // one read. The burst carries `\x1b\x1b` in a single event, so `doubleEsc` was
    // already true on the first one and `draft.clear` won and the turn survived;
    // typed by hand, the first Escape saw `doubleEsc: false`, resolved to
    // `turn.interrupt`, and the user who meant "clear my typo" killed the turn as
    // well. One gesture, two opposite outcomes, decided by inter-key milliseconds.
    //
    // So the AMBIGUOUS case — and only that case, a running turn with text in the
    // composer, where Escape could honestly mean either — is HELD for the double-tap
    // window and resolved once the gesture is complete. Every unambiguous case still
    // fires on the keystroke: with nothing typed there is nothing to clear, so a
    // stop key at an empty composer is never delayed, which is the state a user is
    // actually in when they reach for it.
    if (chord === "esc") {
      if (escHold.current !== null) {
        clearTimeout(escHold.current);
        escHold.current = null;
        lastEsc.current = now();
        return run("draft.clear", "");
      }
      const doubleEsc = event.sequence === DOUBLE_ESC_SEQ ||
        now() - lastEsc.current < DOUBLE_ESC_MS;
      lastEsc.current = now();
      const command = lookup(ctxWith(doubleEsc), "esc");
      if (command === "turn.interrupt" && !doubleEsc && lineRef.current.text !== "") {
        escHold.current = setTimeout(() => {
          escHold.current = null;
          run("turn.interrupt", "");
        }, DOUBLE_ESC_MS);
        return;
      }
      return command ? run(command, "") : undefined;
    }

    const command = lookup(ctxWith(false), chord);
    if (command) return run(command, input);
    if (!isTextInput(input, key)) return;
    if (uiMode === "ask") return setAskText((t) => t + stripCtl(input));
    // The panel's `/` buffer is the other surface that takes typing. It is reached only
    // while `panelFiltering` is true, and while that is true `lookup` returns null for
    // every bare letter and digit in the panel — so this is the whole of the modal
    // half: one rule, every tab.
    if (uiMode === "panel" && panel.filtering) return panel.filterInput(stripCtl(input));
    if (uiMode !== "chat") return;
    // Under ink a fast typist's keystrokes and their Return arrived in ONE read,
    // so a newline could be data rather than a keypress and had to be split back
    // out (`chunkInput`). OpenTUI's parser emits one event per key, so a Return
    // is always a Return and never rides along inside typed text.
    // Typing re-opens a popup an earlier esc closed: esc dismisses THIS token, not
    // completion in general.
    setDismissed(false);
    setLine((s) => insertText(s, stripCtl(input)));
  });

  // ---- render -------------------------------------------------------------

  if (uiMode === "help") return <Help rows={rows} offset={scrollOff} />;

  const title = state.session
    ? sessionLabel(state.session.title, state.session.workspace)
    : "new conversation";
  // The workspace and the `? help` hint live on the METER, at the bottom, beside
  // the composer — not up here. A status bar a screen away from the input is one
  // the user has to go looking for; every other harness keeps this next to where
  // you type. The top line is the conversation's title and nothing else.
  const workspace = shortenPath(state.session?.workspace ?? defaultWorkspace ?? "", home);
  const header = (
    <text wrapMode="none">
      <b>{title}</b>
      <span fg={UI.warn}>{state.connected ? "" : "  · disconnected"}</span>
    </text>
  );

  const status = (
    <StatusLine
      width={cols}
      meter={{
        model: state.session?.model ?? state.effectiveModel,
        costUsd: state.usage?.tree.costUsd ?? state.usage?.costUsd ?? null,
        contextTokens: state.session?.contextTokens ?? null,
        contextLimit: state.contextLimit,
        workspace,
        ...(state.session?.effort ? { effort: state.session.effort } : {}),
        shells: state.jobs.filter((j) => j.status === "running").length,
        help: true,
      }}
    />
  );

  // The one non-chat surface. It DISPLACES THE TRANSCRIPT and nothing else: the
  // rail, the composer and the status row stay pinned under it, because a panel
  // that blanks the status row means the mode, the workspace, the model, the cost
  // and the context budget are all unreadable exactly while you are picking one
  // of them. Every tab it holds is `PanelHost`'s business rather than this file's.
  if (panel.view) {
    return (
      <box flexDirection="column">
        {header}
        {/* Fixed height, so the rows below stay on the LAST rows of the screen.
            A tab with little in it (`no MCP servers configured`) is five rows, and
            without this the composer and the status row rode up under it and the
            rest of the terminal was void. `Panel` sizes its body from the same
            number, so its content is always shorter than this box. */}
        <box flexDirection="column" height={panelBodyH}>
          {panel.view}
        </box>
        {/* The notice belongs here as well as in `Chat`. Copying from a panel row
            said nothing at all, because the only place a notice was painted is the
            surface the panel had just displaced — so the one action with no other
            feedback was silent exactly where it was newly possible. */}
        {state.notice
          ? <text fg={UI.warn} wrapMode="none">{state.notice}</text>
          : null}
        {state.replay
          ? <text attributes={TextAttributes.DIM} wrapMode="none">{state.replay.line}</text>
          : null}
        <SubagentRail
          units={units}
          sel={mode === "rail" ? railSel : null}
          width={cols}
          armedId={armedStop}
        />
        {/* No completion popup here: the panel owns the keyboard, so the draft
            cannot change and a menu over it would be a control you cannot reach. */}
        <Composer
          input={line.text}
          cursor={line.cursor}
          busy={busy}
          width={cols}
          maxRows={composerRows}
        />
        {status}
        <SelectionLayer />
      </box>
    );
  }

  // One job's output, in the transcript's place. Everything pinned stays pinned —
  // the rail (so the row you came from is still under the cursor), the composer and
  // the status line — for the same reason the panel keeps them: a surface that
  // blanks the status row hides the model, the cost and the context budget exactly
  // while you are watching something run.
  if (uiMode === "job" && state.jobView) {
    return (
      <box flexDirection="column">
        {header}
        <JobOutput
          id={state.jobView.id}
          job={state.jobView.job}
          output={state.jobView.output}
          error={state.jobView.error}
          scroll={jobScroll}
          width={cols}
          height={chatH}
          now={now()}
          armed={armedStop === state.jobView.id}
        />
        <SubagentRail units={units} sel={null} width={cols} armedId={armedStop} />
        <Composer
          input={line.text}
          cursor={line.cursor}
          busy={busy}
          width={cols}
          maxRows={composerRows}
        />
        {status}
        <SelectionLayer />
      </box>
    );
  }

  return (
    <box flexDirection="column">
      {header}
      <Chat
        lines={lines}
        width={cols}
        height={chatH}
        scrollOff={scrollOff}
        activity={state.activity}
        busy={busy}
        elapsedMs={elapsedMs}
        tick={tick}
        turnTokens={turn?.tokens ?? null}
        queued={state.queued}
        notice={state.notice}
      />
      <SubagentRail
        units={units}
        sel={uiMode === "rail" ? railSel : null}
        width={cols}
        armedId={armedStop}
      />
      {ask
        ? <AskCard prompt={ask.question} options={ask.options ?? []} typed={askText} />
        : (
          <Composer
            input={line.text}
            cursor={line.cursor}
            busy={busy}
            width={cols}
            maxRows={composerRows}
            trigger={trigger}
            completions={completion.items}
            completionSel={selAt}
            completionMore={Math.max(0, completion.total - completion.items.length)}
          />
        )}
      {status}
      <SelectionLayer />
    </box>
  );
}

/**
 * The status line, BELOW the composer.
 *
 * Below, because that is where every comparable harness puts it and where the eye
 * already is: Claude Code prints `cwd git:(branch) | model ctx:N% | in/out` under
 * its input rule, Codex and OpenCode the same. bough had it above the input, so
 * the last thing before the box you type into was a row of numbers, and the row
 * that told you where the turn would run sat on the far side of the transcript.
 *
 * The live spinner stays ABOVE, inside `Chat` — that one belongs with the output
 * it is narrating, not with the static facts about the session.
 */
function StatusLine({ width, meter }: { width: number; meter: ChatMeter }) {
  const text = meterLine({ ...meter, width });
  if (!text) return null;
  return <text attributes={TextAttributes.DIM} wrapMode="none">{text}</text>;
}

/**
 * A held `ask()`, in place of the composer.
 *
 * Deliberately tiny and deliberately here: it is four rows of text with no state
 * of its own, and giving it a file would imply there is something to test.
 */
function AskCard(
  { prompt, options, typed }: { prompt: string; options: string[]; typed: string },
) {
  return (
    <box flexDirection="column" borderStyle="rounded" borderColor={UI.warn} paddingX={1}>
      <text wrapMode="none">{prompt}</text>
      {options.map((option, i) => (
        <text key={option + i} wrapMode="none">
          <span fg={UI.accent}>{` ${i + 1} `}</span>
          <span>{option}</span>
        </text>
      ))}
      <text wrapMode="none">
        <span attributes={TextAttributes.DIM}>{"› "}</span>
        <span>{typed}</span>
      </text>
      <text attributes={TextAttributes.DIM} wrapMode="none">
        {options.length > 0 ? "1-9 pick · " : ""}type an answer · ⏎ send · esc decline
      </text>
    </box>
  );
}

/**
 * The `?` overlay, rendered from the keymap so it can never drift out of date.
 *
 * A WINDOW, not a page. The keymap is ~50 rows and a terminal is 24, so this
 * renders `body` rows starting at `offset` and says so in the header. It must
 * never hand yoga more children than fit: the previous version pinned a column to
 * `height={rows}` and let flexbox absorb the overflow, which silently deleted
 * every section header (see `HelpLine`).
 */
function Help({ rows, offset }: { rows: number; offset: number }) {
  const all = helpLines();
  // Two chrome rows: the header and the position footer.
  const body = Math.max(1, rows - 2);
  const start = clampHelpOffset(offset, all.length, rows);
  const visible = all.slice(start, start + body);
  const more = all.length - (start + visible.length);
  return (
    <box flexDirection="column">
      <text><b>{"keys · esc closes"}</b></text>
      {visible.map((l, i) => {
        if (l.kind === "blank") return <text key={i}>{" "}</text>;
        if (l.kind === "header") {
          return (
            <text
              key={i}
              fg={l.muted ? undefined : UI.accent}
              attributes={l.muted ? TextAttributes.DIM : undefined}
            >
              {l.desc}
            </text>
          );
        }
        return (
          <text key={i} wrapMode="none" attributes={l.muted ? TextAttributes.DIM : undefined}>
            {l.prose
              ? <span attributes={TextAttributes.DIM}>{"  · "}</span>
              : <span fg={l.muted ? undefined : UI.info}>{`  ${l.chord.padEnd(12)}`}</span>}
            <span attributes={l.muted ? TextAttributes.DIM : undefined}>{l.desc}</span>
          </text>
        );
      })}
      <text attributes={TextAttributes.DIM}>
        {/* The legend names both, because the page keys are why the last section is
            reachable at all — ↑↓ alone put it forty presses down. */}
        {more > 0
          ? `↑↓ pgup/pgdn scroll · ${more} more below`
          : start > 0
          ? "↑↓ pgup/pgdn scroll · end"
          : "↑↓ pgup/pgdn scroll"}
      </text>
    </box>
  );
}

/**
 * Clamp the overlay's scroll so the last page is the last page.
 *
 * Exported-ish (module-local, but used by the key handler too) because the clamp
 * and the render must agree: if the handler let the offset run past the end the
 * overlay would go blank, which is exactly the class of bug that shipped here once.
 */
function clampHelpOffset(offset: number, total: number, rows: number): number {
  const body = Math.max(1, rows - 2);
  return Math.max(0, Math.min(offset, Math.max(0, total - body)));
}
