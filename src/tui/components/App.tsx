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
import { Box, Text, useApp, useInput, useStdout } from "ink";
import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import type { ModelRow } from "../../llm/client.ts";
import { api, type SessionRow } from "../api.ts";
import type { MouseEvent, NavKey } from "../mouse.ts";
import { buildLines } from "../lines.ts";
import {
  activeTrigger,
  applyCompletion,
  meterLine,
  rankCompletions,
  sessionLabel,
  shortenPath,
  SPINNER_MS,
  UI,
} from "../format.ts";
import { onResize, type TermSize } from "../term.ts";
import {
  chordOf,
  chunkInput,
  type Command,
  editLine,
  EMPTY_LINE,
  helpLines,
  insertText,
  isTextInput,
  type KeyContext,
  type LineState,
  lookup,
  stripCtl,
  type UiMode,
} from "../keys.ts";
import { currentAsk, isBusy, type Store, type TuiState } from "../store.ts";
import { Chat, type ChatMeter } from "./Chat.tsx";
import { Composer } from "./Composer.tsx";
import { type PanelControls, type PanelHostDeps, usePanelHost } from "./PanelHost.tsx";
import { historyTreeRows } from "../historytree.ts";
import { liveSubagents, SubagentRail } from "./SubagentRail.tsx";
import { treeItems } from "./Tree.tsx";

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
 * What the stdin filter (`mouse.ts`) took out of the stream before ink saw it.
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
  /** Injected so a render is reproducible and the esc double-tap is testable. */
  now?: () => number;
}

/** Rows a wheel tick moves. Three, like every other pager. */
const WHEEL_ROWS = 3;

/** How often the rail re-reads the session's children while a turn is running. */
const RAIL_POLL_MS = 1500;

/** Rows the `?` overlay moves per ↑/↓. A keymap is scanned, not paged. */
const HELP_STEP = 3;

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
    now = Date.now,
  }: AppProps,
) {
  const { exit } = useApp();
  const { stdout } = useStdout();
  const state = useSyncExternalStore<TuiState>(store.subscribe, store.getState, store.getState);

  const [mode, setMode] = useState<UiMode>("chat");
  const [line, setLine] = useState<LineState>(EMPTY_LINE);
  const [scrollOff, setScrollOff] = useState(0);
  const [foldAll, setFoldAll] = useState(false);
  const [railSel, setRailSel] = useState(0);
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set<string>());
  const [children, setChildren] = useState<Record<string, SessionRow[]>>({});
  const [sent, setSent] = useState<string[]>([]);
  const [histAt, setHistAt] = useState<number | null>(null);
  const [askText, setAskText] = useState("");
  const [quitArmed, setQuitArmed] = useState(false);
  const lastEsc = useRef(0);

  // The size is TRACKED, not sampled once. ink re-rendered the tree for the first
  // resize and then never again, so a terminal that grew left the bottom of the
  // screen unused until restart; `onResize` makes this app own the signal. The
  // initial read still prefers ink's stdout so a test can mount with a fake one.
  // `|| 80`, not `?? 80`: a stdout with no size reports 0, and a zero-width
  // viewport wraps the transcript one character per line rather than falling back.
  const [size, setSize] = useState<TermSize>(() => ({
    cols: Math.max(20, stdout?.columns || 80),
    rows: Math.max(8, stdout?.rows || 24),
  }));
  useEffect(() => onResize(setSize), []);
  const { cols, rows } = size;

  const ask = currentAsk(state);
  const busy = isBusy(state);

  // A spinner needs a clock, and a clock that runs when nothing is happening is a
  // wakeup per frame forever. This one exists only while a turn does: `busy` gates
  // the interval, and the turn's start is stamped the moment it goes true.
  const [tick, setTick] = useState(0);
  const busySince = useRef<number | null>(null);
  if (busy && busySince.current === null) busySince.current = now();
  if (!busy && busySince.current !== null) busySince.current = null;
  useEffect(() => {
    if (!busy) return;
    const id = setInterval(() => setTick((t) => t + 1), SPINNER_MS);
    return () => clearInterval(id);
  }, [busy]);
  const elapsedMs = busy && busySince.current !== null ? now() - busySince.current : 0;

  // ---- live delegated work -------------------------------------------------
  // The rail reads `children[currentId]`, and the ONLY thing that ever filled
  // `children` was the tree tab's drill-in. So in chat it was permanently empty:
  // no rail ever rendered, `railLive` was always false, and ↓ — documented as
  // "into the live subagent rail" — did nothing while subagents were running.
  // Delegation is a primary capability (spec §2, §6) and it was invisible.
  //
  // Polled rather than evented because a subagent is a SESSION and its lifecycle
  // arrives as `session.created` on a stream the store does not index by origin.
  // Only while a turn runs, plus one pull when it stops so finished rows clear.
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
    if (!busy) return () => void (alive = false);
    const timer = setInterval(pull, RAIL_POLL_MS);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, [state.currentId, busy, controls.listChildren]);

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
  const rail = useMemo(
    () => liveSubagents(children[state.currentId ?? ""] ?? []),
    [children, state.currentId],
  );
  const lines = useMemo(
    () =>
      buildLines(state.thread, () => foldAll, () => foldAll, cols, {
        streaming: state.streaming,
        toolLogs: state.toolLogs,
        jobs: state.jobs,
        now: now(),
      }),
    [state.thread, state.streaming, state.toolLogs, state.jobs, cols, foldAll],
  );
  const tree = useMemo(
    () => treeItems({ roots: state.sessions, childrenByOrigin: children, expanded }),
    [state.sessions, children, expanded],
  );
  // pi's `/tree`: the OPEN CONVERSATION as a tree — every turn, and every branch
  // that cut from a turn. The session list already lives in its own tab, so the
  // tree tab is the place this belongs (`historytree.ts`).
  const [userOnly, setUserOnly] = useState(false);
  const conversation = useMemo(
    () =>
      historyTreeRows({
        thread: state.thread,
        branches: children[state.currentId ?? ""] ?? [],
        userOnly,
      }),
    [state.thread, children, state.currentId, userOnly],
  );

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

  const panel = usePanelHost({
    store,
    state,
    rows,
    cols,
    now: now(),
    controls: { ...controls, forkAt },
    models,
    ...(theme ? { theme } : {}),
    tree,
    conversation,
    drillIn,
    collapse,
  });

  // A held question owns the keyboard: the card replaces the composer (spec §6).
  // The panel outranks both — while it is open it IS the surface with the keyboard.
  const uiMode: UiMode = panel.open ? "panel" : mode === "chat" && ask ? "ask" : mode;

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
    // and workflow-steering command is its own, and none of them appears below.
    if (panel.handle(command)) return;
    const page = Math.max(1, rows - 8);
    const last = (n: number) => Math.max(0, n - 1);
    switch (command) {
      case "quit.arm":
        setQuitArmed(true);
        return store.notify("^c again to quit — subagents and workflows keep running");
      case "quit":
        return exit();
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
        return setFoldAll((v) => !v);
      // Two surfaces, opposite senses. The transcript's offset counts BACKWARDS
      // from the bottom, so scrolling up raises it; the overlay is a document read
      // top-down, so scrolling up lowers it. Same command, because it is the same
      // key doing the same thing to whatever is on screen.
      case "scroll.up":
        if (uiMode === "help") return setScrollOff((o) => Math.max(0, o - HELP_STEP));
        return setScrollOff((o) => Math.min(Math.max(0, lines.length - 1), o + page));
      case "scroll.down":
        if (uiMode === "help") {
          const max = Math.max(0, helpLines().length - Math.max(1, rows - 2));
          return setScrollOff((o) => Math.min(max, o + HELP_STEP));
        }
        return setScrollOff((o) => Math.max(0, o - page));

      case "rail.enter":
        setRailSel(0);
        return setMode("rail");
      case "rail.up":
        return railSel === 0 ? setMode("chat") : setRailSel((i) => i - 1);
      case "rail.down":
        return setRailSel((i) => Math.min(last(rail.length), i + 1));
      case "rail.open": {
        const target = rail[railSel];
        setMode("chat");
        return target ? void store.open(target.id) : undefined;
      }
      case "rail.exit":
        return setMode("chat");

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
    ask,
    askText,
    exit,
    histAt,
    lines.length,
    uiMode,
    panel,
    rail,
    railSel,
    rows,
    sent,
    store,
    submit,
  ]);

  // A paste, a wheel tick and the Home/End keys ink does not deliver. Registered
  // once; each handler is one line, because none of them is a decision.
  useEffect(() => {
    const off = [
      hooks.onPaste?.((text) => setLine((s) => insertText(s, stripCtl(text)))),
      hooks.onMouse?.((event) => {
        if (event.kind === "wheel-up") setScrollOff((o) => o + WHEEL_ROWS);
        if (event.kind === "wheel-down") setScrollOff((o) => Math.max(0, o - WHEEL_ROWS));
      }),
      hooks.onNavKey?.((key) => {
        // Backtab is not line editing: it is the panel's "previous tab", which no
        // keypress could reach because ink does not decode `CSI Z` (`mouse.ts`).
        if (key === "shiftTab") return void run("panel.prev", "");
        // `chordOf` cannot carry this one: ink reports macOS Backspace as
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
  }, [hooks, run]);

  useInput((input, key) => {
    const chord = chordOf(input, key);
    const doubleEsc = chord === "esc" && now() - lastEsc.current < DOUBLE_ESC_MS;
    if (chord === "esc") lastEsc.current = now();
    if (chord !== "ctrl+c" && quitArmed) setQuitArmed(false);

    const ctx: KeyContext = {
      mode: uiMode,
      emptyDraft: line.text === "",
      multiline: line.text.includes("\n"),
      busy,
      doubleEsc,
      quitArmed,
      railLive: rail.length > 0,
      completing,
    };
    const command = lookup(ctx, chord);
    if (command) return run(command, input);
    if (!isTextInput(input, key)) return;
    if (uiMode === "ask") return setAskText((t) => t + stripCtl(input));
    if (uiMode !== "chat") return;
    // A fast typist's keystrokes and their Return can arrive in one read, so a
    // newline may be data rather than a keypress (`chunkInput`).
    if (/[\r\n]/.test(input)) {
      const { body, send } = chunkInput(input);
      if (body) setLine((s) => insertText(s, body));
      if (send) submit(false);
      return;
    }
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
    <Text wrap="truncate">
      <Text bold>{title}</Text>
      <Text color={UI.warn}>{state.connected ? "" : "  · disconnected"}</Text>
    </Text>
  );

  // The one non-chat surface. It takes the screen while it is open, and every tab
  // it holds is `PanelHost`'s business rather than this file's.
  if (panel.view) {
    return (
      <Box flexDirection="column">
        {header}
        {panel.view}
        {state.replay ? <Text dimColor wrap="truncate">{state.replay.line}</Text> : null}
      </Box>
    );
  }

  const composerRows = Math.min(8, Math.max(3, Math.floor(rows / 4)));
  return (
    <Box flexDirection="column">
      {header}
      <Chat
        lines={lines}
        width={cols}
        height={Math.max(1, rows - composerRows - 4 - rail.length)}
        scrollOff={scrollOff}
        activity={state.activity}
        busy={busy}
        elapsedMs={elapsedMs}
        tick={tick}
        queued={state.queued}
        notice={state.notice}
      />
      <SubagentRail branches={rail} sel={uiMode === "rail" ? railSel : null} />
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
      <StatusLine
        width={cols}
        meter={{
          model: state.session?.model ?? state.effectiveModel,
          costUsd: state.usage?.tree.costUsd ?? state.usage?.costUsd ?? null,
          contextTokens: state.session?.contextTokens ?? null,
          contextLimit: state.contextLimit,
          workspace,
          help: true,
        }}
      />
    </Box>
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
  return <Text dimColor wrap="truncate">{text}</Text>;
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
    <Box flexDirection="column" borderStyle="round" borderColor={UI.warn} paddingX={1}>
      <Text wrap="truncate">{prompt}</Text>
      {options.map((option, i) => (
        <Text key={option + i} wrap="truncate">
          <Text color={UI.accent}>{` ${i + 1} `}</Text>
          <Text>{option}</Text>
        </Text>
      ))}
      <Text wrap="truncate">
        <Text dimColor>{"› "}</Text>
        <Text>{typed}</Text>
      </Text>
      <Text dimColor wrap="truncate">
        {options.length > 0 ? "1-9 pick · " : ""}type an answer · ⏎ send · esc decline
      </Text>
    </Box>
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
    <Box flexDirection="column">
      <Text bold>{"keys · esc closes"}</Text>
      {visible.map((l, i) => {
        if (l.kind === "blank") return <Text key={i}>{" "}</Text>;
        if (l.kind === "header") {
          return (
            <Text key={i} color={l.muted ? undefined : UI.accent} dimColor={l.muted}>
              {l.desc}
            </Text>
          );
        }
        return (
          <Text key={i} wrap="truncate" dimColor={l.muted}>
            {l.prose
              ? <Text dimColor>{"  · "}</Text>
              : <Text color={l.muted ? undefined : UI.info}>{`  ${l.chord.padEnd(12)}`}</Text>}
            <Text dimColor={l.muted}>{l.desc}</Text>
          </Text>
        );
      })}
      <Text dimColor>
        {more > 0 ? `↑↓ scroll · ${more} more below` : start > 0 ? "↑↓ scroll · end" : "↑↓ scroll"}
      </Text>
    </Box>
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
