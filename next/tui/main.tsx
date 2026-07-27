/**
 * The TUI entry point: reach the server, take the terminal, mount the app, and
 * give the terminal back whatever happens.
 *
 * THE INVARIANT THIS HOLDS: **the terminal is restored on every exit path.** A
 * TUI that dies without popping the alternate screen leaves the user in a shell
 * with mouse reporting still on, no cursor, and a title from a session that ended
 * — and the paths out are not only the tidy one: an uncaught throw, a signal, a
 * crash inside ink's render. So `leaveTui` is registered on `unload` as well as
 * being called in the `finally`, and it is written to swallow its own errors,
 * because a restore that throws restores nothing.
 *
 * SECOND — **this file is the only place that knows the process exists.** It reads
 * the environment, opens the client, builds the store, wires the stdin filter, and
 * hands `App` a set of plain values and callbacks. Everything below it — the
 * store, the components, the keymap — is testable without a terminal precisely
 * because none of it reaches for `Deno`, `process` or a socket. That is the same
 * split `server/main.ts` has, for the same reason.
 *
 * THIRD — **preflight fails with a sentence, not a stack trace.** A server that is
 * not running is the single most common thing to go wrong here and it is not an
 * error in the program; the message names the address it tried and the command
 * that fixes it, and exits 2, which is what `bough exec` uses for "connection
 * problem" (spec §15).
 *
 * The stdin filter, the synchronized-output wrapper and the forced kitty keyboard
 * push all exist for reasons documented where they are implemented (`mouse.ts`,
 * `term.ts`); this file only assembles them.
 */
import { render } from "ink";
import { api } from "./api.ts";
import { createStore } from "./store.ts";
import { enterTui, filteredStdin, leaveTui, type MouseEvent, type NavKey } from "./mouse.ts";
import { kittyKeyboardMode, syncedStdout, term } from "./term.ts";
import { App, type AppControls, type InputHooks } from "./components/App.tsx";

/**
 * A one-listener fan-out for the events the stdin filter pulls out of the stream.
 *
 * One listener and not a set: there is exactly one mounted app, and a registry
 * that silently allowed two would make a stale handler from a previous mount look
 * like a duplicated keystroke.
 */
function hub<T>() {
  let handler: ((value: T) => void) | null = null;
  return {
    emit: (value: T) => handler?.(value),
    on: (next: (value: T) => void) => {
      handler = next;
      return () => {
        if (handler === next) handler = null;
      };
    },
  };
}

/** Reach the server once before taking the screen, so a failure is readable. */
async function preflight(): Promise<void> {
  try {
    await api.listSessions();
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    console.error(`bough tui: cannot reach the server at ${api.base}`);
    console.error(`  ${detail}`);
    console.error("  start it with:  deno task dev");
    Deno.exit(2);
  }
}

async function main() {
  await preflight();

  // New conversations default to where `bough` was launched, not to wherever the
  // entry point happens to be resolved from.
  const defaultWorkspace = Deno.env.get("BOUGH_TUI_CWD") ?? Deno.cwd();
  const terminal = term();

  const pastes = hub<string>();
  const mice = hub<MouseEvent>();
  const navKeys = hub<NavKey>();
  const hooks: InputHooks = { onPaste: pastes.on, onMouse: mice.on, onNavKey: navKeys.on };

  // Focus and the background report are terminal REPLIES: they belong to `term.ts`
  // and must never reach the app as keystrokes.
  const stdin = filteredStdin({
    paste: pastes.emit,
    mouse: mice.emit,
    navKey: navKeys.emit,
    focus: (focused) => terminal.setFocused(focused),
    bgReport: (spec) => terminal.reportTermBg(spec),
  });

  const store = createStore();
  const controls: AppControls = {
    listChildren: (originId) => api.listSessions(originId),
    pauseWorkflow: async (id) => void (await api.pauseWorkflow(id)),
    resumeWorkflow: async (id) => void (await api.resumeWorkflow(id)),
    stopWorkflow: async (id) => void (await api.stopWorkflow(id)),
    rerunWorkflow: async (id) => void (await api.rerunWorkflow(id)),
  };

  enterTui();
  globalThis.addEventListener("unload", () => leaveTui(() => terminal.cleanup()));

  // exitOnCtrlC off: the app implements the double-^c itself, and a single ^c
  // would otherwise unmount ink out from under the quit hint it just printed.
  const instance = render(
    <App
      store={store}
      defaultWorkspace={defaultWorkspace}
      controls={controls}
      input={hooks}
    />,
    {
      exitOnCtrlC: false,
      stdin,
      stdout: syncedStdout(),
      kittyKeyboard: { mode: kittyKeyboardMode() },
    },
  );

  // Only now is stdin in raw mode, so only now can the terminal's reply to the
  // background query be read — and the filter is already in place to catch it.
  terminal.queryTermBg();
  store.start();

  try {
    await instance.waitUntilExit();
  } finally {
    await store.stop();
    leaveTui(() => terminal.cleanup());
    // The stdin data listener would otherwise hold the event loop open forever.
    Deno.exit(0);
  }
}

if (import.meta.main) await main();
