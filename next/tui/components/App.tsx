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
 * rather than a flag threaded through six components.
 *
 * FOURTH — **the transcript is built once.** `buildLines` produces the `VLine[]`
 * that `Chat` paints, and it is memoized here rather than inside `Chat`, because
 * the same array is what a click, a search mark or a drag selection would be
 * addressed against. Two derivations would be two answers to "which row is that".
 */
import { Box, Text, useApp, useInput, useStdout } from "ink";
import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import type { SessionRow } from "../api.ts";
import type { MouseEvent, NavKey } from "../mouse.ts";
import { buildLines } from "../lines.ts";
import { sessionLabel, UI } from "../format.ts";
import {
  chordOf,
  type Command,
  chunkInput,
  editLine,
  EMPTY_LINE,
  helpSections,
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
import { liveSubagents, SubagentRail } from "./SubagentRail.tsx";
import { Tree, treeItems } from "./Tree.tsx";
import { Workflows } from "./Workflows.tsx";

/** How long a second Escape still counts as a double-tap. */
const DOUBLE_ESC_MS = 600;

/**
 * The operations the store does not expose yet.
 *
 * Every one of these is a REST call the TUI's client already has a method for
 * (`api.ts`) and the store has no action for — workflow steering (spec §8) and the
 * `?originId=` drill-in the tree and the subagent rail are built on. They are
 * injected rather than imported so this component keeps its "no I/O" property and
 * so a test drives them with three lines of fakes. They belong in `store.ts`
 * eventually; that file is not this task's to change.
 */
export interface AppControls {
  /** `GET /sessions?originId=` — delegated children, for the rail and drill-in. */
  listChildren?: (originId: string) => Promise<SessionRow[]>;
  pauseWorkflow?: (id: string) => Promise<void>;
  resumeWorkflow?: (id: string) => Promise<void>;
  stopWorkflow?: (id: string) => Promise<void>;
  rerunWorkflow?: (id: string) => Promise<void>;
}

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
  controls?: AppControls;
  input?: InputHooks;
  /** Injected so a render is reproducible and the esc double-tap is testable. */
  now?: () => number;
}

/** Rows a wheel tick moves. Three, like every other pager. */
const WHEEL_ROWS = 3;

export function App(
  { store, defaultWorkspace, controls = {}, input: hooks = {}, now = Date.now }: AppProps,
) {
  const { exit } = useApp();
  const { stdout } = useStdout();
  const state = useSyncExternalStore<TuiState>(store.subscribe, store.getState, store.getState);

  const [mode, setMode] = useState<UiMode>("chat");
  const [line, setLine] = useState<LineState>(EMPTY_LINE);
  const [scrollOff, setScrollOff] = useState(0);
  const [foldAll, setFoldAll] = useState(false);
  const [railSel, setRailSel] = useState(0);
  const [listSel, setListSel] = useState(0);
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

  // A held question owns the keyboard: the card replaces the composer (spec §6).
  const uiMode: UiMode = mode === "chat" && ask ? "ask" : mode;

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

  const drillIn = useCallback((originId: string) => {
    setExpanded((set) => new Set([...set, originId]));
    if (children[originId] || !controls.listChildren) return;
    void controls.listChildren(originId)
      .then((rows) => setChildren((c) => ({ ...c, [originId]: rows })))
      .catch(() => store.notify("could not load delegated work for that conversation"));
  }, [children, controls, store]);

  const steer = useCallback((run: (id: string) => Promise<void> | undefined, verb: string) => {
    const target = state.workflows[listSel];
    if (!target) return;
    const started = run(target.id);
    if (!started) return store.notify(`${verb} is not wired up in this client yet`);
    void started.then(() => store.refreshWorkflows()).catch((e: unknown) =>
      store.notify(e instanceof Error ? e.message : String(e))
    );
  }, [state.workflows, listSel, store]);

  /** One command → one effect. Nothing here decides anything; it only dispatches. */
  const run = useCallback((command: Command, input: string) => {
    const page = Math.max(1, rows - 8);
    const last = (n: number) => Math.max(0, n - 1);
    switch (command) {
      case "quit.arm":
        setQuitArmed(true);
        return store.notify("^c again to quit — subagents and workflows keep running");
      case "quit":
        return exit();
      case "help.open":
        return setMode("help");
      case "help.close":
        return setMode("chat");
      case "view.chat":
        return setMode("chat");
      case "view.tree":
        setListSel(0);
        return setMode("tree");
      case "view.workflows":
        setListSel(0);
        void store.refreshWorkflows();
        return setMode("workflows");

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
      case "scroll.up":
        return setScrollOff((o) => Math.min(Math.max(0, lines.length - 1), o + page));
      case "scroll.down":
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

      case "move.up":
        return setListSel((i) => Math.max(0, i - 1));
      case "move.down": {
        const len = uiMode === "tree" ? tree.length : state.workflows.length;
        return setListSel((i) => Math.min(last(len), i + 1));
      }
      case "move.in": {
        const item = tree[listSel];
        return item ? drillIn(item.type === "session" ? item.session.id : item.originId) : undefined;
      }
      case "move.out": {
        const item = tree[listSel];
        if (!item) return;
        const id = item.type === "session" ? item.session.id : item.originId;
        return setExpanded((set) => new Set([...set].filter((x) => x !== id)));
      }
      case "open": {
        if (uiMode === "workflows") {
          // Spec §8: replay is ALWAYS reported. This is the client half of that.
          const target = state.workflows[listSel];
          return target ? void store.refreshReplay(target.id) : undefined;
        }
        const item = tree[listSel];
        if (!item || item.type !== "session") return;
        setMode("chat");
        return void store.open(item.session.id);
      }

      case "wf.pause":
        return steer(controls.pauseWorkflow ?? (() => undefined), "pause");
      case "wf.resume":
        return steer(controls.resumeWorkflow ?? (() => undefined), "resume");
      case "wf.stop":
        return steer(controls.stopWorkflow ?? (() => undefined), "stop");
      case "wf.rerun":
        return steer(controls.rerunWorkflow ?? (() => undefined), "relaunch");

      default:
        // Everything left is line editing, which is a pure function of the draft.
        return setLine((s) => editLine(s, command));
    }
  }, [
    ask,
    askText,
    controls,
    drillIn,
    exit,
    histAt,
    lines.length,
    listSel,
    rail,
    railSel,
    rows,
    sent,
    state.workflows,
    steer,
    store,
    submit,
    tree,
    uiMode,
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

  if (uiMode === "help") return <Help rows={rows} />;

  const title = state.session
    ? sessionLabel(state.session.title, state.session.workspace)
    : "new conversation";
  const header = (
    <Text wrap="truncate">
      <Text bold>{title}</Text>
      <Text dimColor>{`  ${state.connected ? "" : "· disconnected "}`}</Text>
    </Text>
  );

  if (uiMode === "tree") {
    return (
      <Box flexDirection="column">
        {header}
        <Tree items={tree} selected={listSel} rows={rows - 2} />
      </Box>
    );
  }

  if (uiMode === "workflows") {
    return (
      <Box flexDirection="column">
        {header}
        <Workflows
          runs={state.workflows}
          sel={listSel}
          level={0}
          detail={null}
          phaseSel={0}
          agentSel={0}
          scroll={0}
          filter={null}
          promptOpen={false}
          rows={rows - 3}
          cols={cols}
          now={now()}
        />
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
          model: state.session?.model ?? null,
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

/** The `?` overlay, rendered from the keymap so it can never drift out of date. */
function Help({ rows }: { rows: number }) {
  const sections = helpSections();
  return (
    <Box flexDirection="column" height={rows}>
      <Text bold>keys · esc closes</Text>
      {sections.map((section) => (
        <Box key={section.section} flexDirection="column" marginTop={1}>
          <Text color={section.unavailable ? undefined : UI.accent} dimColor={section.unavailable}>
            {section.section}
          </Text>
          {section.keys.map(([chord, desc], i) => (
            <Text key={`${chord}-${i}`} wrap="truncate" dimColor={section.unavailable}>
              {section.limits ? <Text dimColor>{"  · "}</Text> : (
                <Text color={section.unavailable ? undefined : UI.info}>
                  {`  ${chord.padEnd(12)}`}
                </Text>
              )}
              <Text dimColor={section.limits || section.unavailable}>{desc}</Text>
            </Text>
          ))}
        </Box>
      ))}
    </Box>
  );
}
