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
import type { SessionRow } from "../api.ts";
import type { MouseEvent, NavKey } from "../mouse.ts";
import { buildLines } from "../lines.ts";
import { headerContext, sessionLabel, UI } from "../format.ts";
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
import { Chat } from "./Chat.tsx";
import { Composer } from "./Composer.tsx";
import { type PanelControls, type PanelHostDeps, usePanelHost } from "./PanelHost.tsx";
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
  /** Injected so a render is reproducible and the esc double-tap is testable. */
  now?: () => number;
}

/** Rows a wheel tick moves. Three, like every other pager. */
const WHEEL_ROWS = 3;

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

  // `|| 80`, not `?? 80`: a stdout with no size reports 0, and a zero-width
  // viewport wraps the transcript one character per line rather than falling back.
  const cols = Math.max(20, stdout?.columns || 80);
  const rows = Math.max(8, stdout?.rows || 24);

  const ask = currentAsk(state);
  const busy = isBusy(state);
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
  const panel = usePanelHost({
    store,
    state,
    rows,
    cols,
    now: now(),
    controls,
    models,
    ...(theme ? { theme } : {}),
    tree,
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
      hooks.onNavKey?.((key) =>
        setLine((s) =>
          editLine(s, key === "home" || key === "cmdHome" ? "cursor.home" : "cursor.end")
        )
      ),
    ];
    return () => {
      for (const stop of off) stop?.();
    };
  }, [hooks]);

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
    setLine((s) => insertText(s, stripCtl(input)));
  });

  // ---- render -------------------------------------------------------------

  if (uiMode === "help") return <Help rows={rows} offset={scrollOff} />;

  const title = state.session
    ? sessionLabel(state.session.title, state.session.workspace)
    : "new conversation";
  // Where this conversation runs, and the one hint that the keymap exists. Both
  // are on EVERY screen including the empty one, because both are things you need
  // before you press enter rather than after (`headerContext`).
  const workspace = state.session?.workspace ?? defaultWorkspace ?? null;
  const header = (
    <Text wrap="truncate">
      <Text bold>{title}</Text>
      <Text dimColor>{`  ${headerContext(workspace, home)}`}</Text>
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
        meter={{
          model: state.session?.model ?? state.effectiveModel,
          costUsd: state.usage?.costUsd ?? null,
          contextTokens: state.session?.contextTokens ?? null,
        }}
        activity={state.activity}
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
          />
        )}
    </Box>
  );
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
