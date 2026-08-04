/**
 * The TUI entry point: reach the server, take the terminal, mount the app, and
 * give the terminal back whatever happens.
 *
 * THE INVARIANT THIS HOLDS: **the terminal is restored on every exit path.** A
 * TUI that dies without popping the alternate screen leaves the user in a shell
 * with mouse reporting still on, no cursor, and a title from a session that ended
 * — and the paths out are not only the tidy one: an uncaught throw, a signal, a
 * crash inside the renderer. So `leaveTui` is registered on the process `exit`
 * event as well as being called in the `finally`, and it is written to swallow its
 * own errors, because a restore that throws restores nothing.
 *
 * SECOND — **this file is the only place that knows the process exists.** It reads
 * the environment, opens the client, builds the store, wires the stdin filter, and
 * hands `App` a set of plain values and callbacks. Everything below it — the
 * store, the components, the keymap — is testable without a terminal precisely
 * because none of it reaches for `process` or a socket. That is the same split
 * `server/main.ts` has, for the same reason.
 *
 * THIRD — **preflight fails with a sentence, not a stack trace.** A server that is
 * not running is the single most common thing to go wrong here and it is not an
 * error in the program; the message names the address it tried and the command
 * that fixes it, and exits 2, which is what `bough exec` uses for "connection
 * problem" (spec §15).
 *
 * FOURTH — **the terminal has two writers now, and the split is written down.**
 * OpenTUI's renderer sets up the screen itself, where ink was handed a stdout and
 * left the modes to us. Which sequence belongs to which owner is spelled out at the
 * `createCliRenderer` call below; the stdin filter is documented in `mouse.ts`.
 */
import process from "node:process";
import { mkdir, stat, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import { createCliRenderer } from "@opentui/core";
import { createRoot } from "@opentui/react";
// The one value imported from the model layer, and the reason it is imported HERE:
// `llm/client.ts` pulls the provider SDK, so a component that reached for the catalog
// would drag the whole model layer into the component graph. The composition root is
// allowed to know about both sides; nothing below it is.
import { MODELS, type ModelRow } from "../llm/client.ts";
import { api } from "./api.ts";
import { createStore, type TuiState } from "./store.ts";
import { enterTui, filteredStdin, leaveTui, type MouseEvent, type NavKey } from "./mouse.ts";
import { applyTheme, type ThemePreset, type ThemeState } from "./theme.ts";
import { isTuiHelpRequest, isTuiUsageError, parseTuiArgs, USAGE as TUI_USAGE } from "./args.ts";
import { boughTitle, term } from "./term.ts";
import { type Clipboard, clipboardFromText } from "./clipboard.ts";
import { App, type AppControls, type InputHooks } from "./components/App.tsx";

/**
 * Hand a clicked link to the OS.
 *
 * HTTP(S) ONLY, and that is a security boundary rather than a convenience. The
 * transcript is written by a model and by tool output, so the URL under the cursor
 * is untrusted text; `open` on macOS will happily launch a `file://` path, and
 * other schemes are how a click becomes an arbitrary application launch. A link
 * that is not http(s) is left as text — visible, copyable, and inert.
 *
 * Detached and ignored: a browser that is slow to start, or absent, must not stall
 * the render loop or take the TUI down with it.
 */
function openUrl(url: string): void {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return;
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return;
  const argv = process.platform === "darwin"
    ? ["open", parsed.href]
    : ["xdg-open", parsed.href];
  try {
    Bun.spawn(argv, { stdout: "ignore", stderr: "ignore", stdin: "ignore" }).unref();
  } catch {
    // No opener on this machine. The URL is still on screen to copy.
  }
}

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
    // `detail` already carries the remedy (`OfflineError`). This used to add a
    // second "start it with: …" underneath it, so the same failure gave two
    // different commands and the user had to guess which one was meant.
    console.error(`bough tui: ${detail}`);
    process.exit(2);
  }
}

/** Derive terminal chrome from store state; no terminal dependency enters the store. */
function tabTitle(state: TuiState, spinnerFrame = 0): string {
  const session = state.session ?? state.sessions.find((row) => row.id === state.currentId) ?? null;
  const turn = state.turn?.sessionId === state.currentId ? state.turn : null;
  return boughTitle(
    session?.title ?? null,
    turn ? turn.endedAt === null ? "running" : "complete" : null,
    spinnerFrame,
  );
}

function tabTurnRunning(state: TuiState): boolean {
  return state.turn?.sessionId === state.currentId && state.turn.endedAt === null;
}

/** The title moves slowly enough not to make terminal chrome noisy. */
const TITLE_SPINNER_MS = 120;

/**
 * The image on the pasteboard wins over the text beside it; see `clipboard.ts` for
 * why that order is the fix and not a preference. Compiles the macOS image
 * extractor only once.
 */
async function pasteClipboard(): Promise<Clipboard> {
  if (process.platform !== "darwin") return null;
  const bytes = await pasteboardPng();
  if (bytes) return { image: new Blob([bytes], { type: "image/png" }) };
  const textProcess = Bun.spawn(["pbpaste"], { stdout: "pipe", stderr: "ignore" });
  const text = await new Response(textProcess.stdout).text();
  if ((await textProcess.exited) !== 0 || text === "") return null;
  return await clipboardFromText(text);
}

/** The pasteboard's image data as PNG bytes, or `null` if it holds none. */
async function pasteboardPng(): Promise<Uint8Array<ArrayBuffer> | null> {
  const dir = join(process.env.BOUGH_HOME ?? join(homedir(), ".bough"), "bin");
  const helper = join(dir, "pasteboard-png");
  try { await stat(helper); } catch {
    await mkdir(dir, { recursive: true });
    const source = join(dir, "pasteboard-png.swift");
    await writeFile(source, "import AppKit\nlet p = NSPasteboard.general\nguard let d = p.data(forType: .tiff), let i = NSImage(data: d), let b = i.tiffRepresentation, let r = NSBitmapImageRep(data: b), let png = r.representation(using: .png, properties: [:]) else { exit(1) }\nFileHandle.standardOutput.write(png)\n");
    const compile = Bun.spawn(["swiftc", "-O", source, "-o", helper], { stdout: "ignore", stderr: "ignore" });
    if ((await compile.exited) !== 0) return null;
  }
  const imageProcess = Bun.spawn([helper], { stdout: "pipe", stderr: "ignore" });
  const bytes = new Uint8Array(await new Response(imageProcess.stdout).arrayBuffer());
  return (await imageProcess.exited) === 0 && bytes.length > 0 ? bytes : null;
}

async function main() {
  // The command line is parsed BEFORE the terminal is taken, so a usage error is
  // printed to a normal screen rather than into the alternate buffer. `-w` used to
  // be discarded in silence, which pointed the agent at a repository the user did
  // not choose — and there is no sandbox to make that harmless (`args.ts`).
  const args = parseTuiArgs(process.argv.slice(2));
  if (isTuiHelpRequest(args)) {
    console.log(TUI_USAGE);
    process.exit(0);
  }
  if (isTuiUsageError(args)) {
    console.error(args.usageError);
    process.exit(2);
  }
  await preflight();

  // New conversations default to where `bough` was launched, not to wherever the
  // entry point happens to be resolved from.
  const defaultWorkspace = args.workspace ?? process.env.BOUGH_TUI_CWD ?? process.cwd();
  const terminal = term();

  const pastes = hub<string>();
  const mice = hub<MouseEvent>();
  const navKeys = hub<NavKey>();
  const hooks: InputHooks = { onPaste: pastes.on, onMouse: mice.on, onNavKey: navKeys.on, pasteClipboard };

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
  // Built once, outside the render, so the panel's "re-read on entry" effect depends
  // on stable references and does not re-fire on every keystroke.
  const controls: AppControls = {
    listChildren: (originId) => api.listSessions(originId),
    // Never cached by the caller: grants and connections change between turns, so the
    // panel re-reads this every time the MCP tab is entered (plan §6.13).
    loadMcp: (sessionId) => api.mcpStatus(sessionId),
    setMcpEnabled: (name, on, sessionId) => api.setMcpEnabled(name, on, sessionId),
    beginMcpAuth: (name) => api.beginMcpAuth(name),
    clearMcpAuth: (name) => api.clearMcpAuth(name),
    mcpAuthStatus: (name) => api.mcpAuthStatus(name),
    putMcpServer: (name, config) => api.putMcpServer(name, config),
    deleteMcpServer: (name) => api.deleteMcpServer(name),
    connectMcpServer: (name, sessionId) => api.connectMcpServer(name, sessionId),
    restartMcpServer: (name, sessionId) => api.restartMcpServer(name, sessionId),
    pauseWorkflow: async (id) => void (await api.pauseWorkflow(id)),
    resumeWorkflow: async (id) => void (await api.resumeWorkflow(id)),
    stopWorkflow: async (id) => void (await api.stopWorkflow(id)),
    rerunWorkflow: async (id) => void (await api.rerunWorkflow(id)),
    // Re-read on every entry into the tab, never cached: a skill is a folder on disk
    // that the user or the agent may have written since the panel was last open, and
    // the route is a fresh walk of the source directories (`server/skills.ts`).
    loadSkills: () => api.listSkills(),
  };

  // The theme, fetched BEFORE the first frame and painted into the palette (spec
  // §16: "the TUI fetches it at boot and paints truecolor"). Two things depend on
  // it landing here rather than in an effect: `palette` is a mutable singleton read
  // at render time, so a late fetch would repaint mid-frame; and the picker's
  // baseline — what leaving the tab REVERTS to — is exactly this value, so without
  // it browsing a theme and pressing escape would revert a stored theme off the
  // screen.
  //
  // Best-effort by construction. A server that cannot answer leaves the built-in
  // FALLBACK painted, which is a complete, contrast-checked palette, not terminal
  // grey — a theme is decoration and must never be the reason the TUI does not start.
  let theme: ThemeState | null = null;
  try {
    theme = await api.getTheme();
    applyTheme(theme);
  } catch {
    // Reported by its absence: the default palette is what the user sees.
  }

  /**
   * The picker's catalog, fetched for the same reason and on the same terms as the
   * theme above: before the first frame, and best-effort.
   *
   * `MODELS` alone was what the picker used to get, so the rows were whatever was
   * compiled in and a working `OPENAI_API_KEY` bought nothing — every model OpenAI
   * shipped after that array was written was unreachable from the product. The server
   * answers instead because it is the process that HOLDS the key (`~/.bough/env`); this
   * one does not have it, and a catalog discovered here would offer rows the server
   * cannot bill.
   *
   * The fallback is `MODELS` and not an empty list: a picker with no rows is a tab that
   * looks broken, and the static ids are exactly as valid as they were before this
   * fetch existed.
   */
  let models = MODELS as readonly ModelRow[];
  try {
    const catalog = await api.getModels();
    if (catalog.models.length > 0) models = catalog.models;
  } catch {
    // Reported by its absence: the built-in rows are what the picker shows.
  }

  /**
   * Keeping a theme writes it through. `DELETE` and not `PUT` for the empty partial,
   * because "Default" IS the reset (`tui/theme.ts`'s `stateFor`), and PUTting an
   * empty colour map would store a named theme that overrides nothing — which reads
   * identically on screen and survives as a row nobody can explain.
   *
   * The promise is returned so `commit()` can swallow it in one place; a failed save
   * must not unpaint the screen the user just chose.
   */
  const persistTheme = (preset: ThemePreset, state: ThemeState) =>
    state.theme === null
      ? api.deleteTheme()
      : api.putTheme({ name: preset.name, colors: state.theme.colors });

  enterTui();
  // Deno fired `unload` here. Under Bun the last-chance hook is the process `exit`
  // event, which runs on a normal return and on `process.exit` and — exactly like
  // `unload` before it — not on a signal. It fires again after the `finally` below
  // has already restored; `leaveTui` is idempotent, so the second pass costs a
  // duplicate mode-reset and nothing else.
  process.on("exit", () => leaveTui(() => terminal.cleanup()));

  // ink's `instance.waitUntilExit()`, in the shape OpenTUI offers. Declared before
  // the renderer because the renderer may destroy itself during its own setup, and
  // that path must find a resolver rather than a variable in its dead zone.
  let exited!: () => void;
  const untilExit = new Promise<void>((resolve) => (exited = resolve));

  /**
   * WHO OWNS WHAT ON THE TERMINAL. ink was handed a stdout and left every mode to
   * us; OpenTUI's renderer sets the screen up itself, so the two would fight unless
   * the split is stated. This is what the renderer was observed to write:
   *
   *   OPENTUI — `?1049h`/`?1049l` (the alternate screen: NOT idempotent, it saves
   *     the cursor and clears, so `enterTui` no longer writes it), `?25l`/`?25h`,
   *     `?2026h`…`?2026l` around every frame, `?2004h` (bracketed paste),
   *     `>4;1m` (modifyOtherKeys), the capability and OSC colour queries — and, on
   *     the way out, `]0;`, which BLANKS the window title. That last one is why the
   *     title pop in `leaveTui` has to run after `destroy`, not before.
   *   BOUGH — `CSI 22;0t`/`CSI 23;0t` (push and pop the title), the SGR mouse modes,
   *     and `?1004h` (focus reporting). The renderer emits none of those three.
   *
   * The frame guard is the reason `syncedStdout()` is gone: OpenTUI already wraps
   * each frame in DEC 2026, and a stdout Proxy that wrapped every write would nest
   * synchronized-update spans inside one another. A Proxy is also not `===
   * process.stdout`, and the renderer installs its SIGWINCH handler only for the
   * real one — so passing it would have cost resize handling as well.
   */
  const renderer = await createCliRenderer({
    stdin,
    // Stated rather than defaulted: this is the line that says who owns `?1049`.
    screenMode: "alternate-screen",
    // exitOnCtrlC off: the app implements the double-^c itself, and a single ^c
    // would otherwise tear the renderer down out from under the quit hint it just
    // printed.
    exitOnCtrlC: false,
    // OpenTUI's mouse model is hit-testing against renderables; bough's is a wheel
    // and a drag selection over a text transcript, parsed out of the stream by
    // `mouse.ts` and delivered through `hooks.onMouse`. Turning OpenTUI's on would
    // put two parsers and two enable/disable owners on the same modes.
    useMouse: false,
    enableMouseMovement: false,
    // Teardown is teardown, whether the app asked for it, an uncaught throw caused
    // it, or a signal did.
    onDestroy: () => exited(),
  });

  createRoot(renderer).render(
    <App
      store={store}
      defaultWorkspace={defaultWorkspace}
      home={process.env.HOME ?? ""}
      controls={controls}
      input={hooks}
      models={models}
      theme={{ current: theme, persist: persistTheme }}
      notifyDesktop={(body) => terminal.notifyDesktop(body)}
      copyText={(text) => terminal.osc52Copy(text)}
      openUrl={openUrl}
      uploadImage={api.uploadImage}
    />,
  );

  // Only now is stdin in raw mode, so only now can the terminal's reply to the
  // background query be read — and the filter is already in place to catch it.
  terminal.queryTermBg();
  store.start();
  let previousTabTitle = "";
  let spinnerFrame = 0;
  let spinnerTimer: ReturnType<typeof setInterval> | null = null;
  const updateTabTitle = (state: TuiState) => {
    const title = tabTitle(state, spinnerFrame);
    if (title !== previousTabTitle) {
      previousTabTitle = title;
      terminal.setTitle(title);
    }
    if (tabTurnRunning(state) === (spinnerTimer !== null)) return;
    if (spinnerTimer !== null) {
      clearInterval(spinnerTimer);
      spinnerTimer = null;
      return;
    }
    spinnerTimer = setInterval(() => {
      spinnerFrame++;
      updateTabTitle(store.getState());
    }, TITLE_SPINNER_MS);
    spinnerTimer.unref?.();
  };
  const unsubscribeTabTitle = store.subscribe(updateTabTitle);
  updateTabTitle(store.getState());

  try {
    await untilExit;
  } finally {
    if (spinnerTimer !== null) clearInterval(spinnerTimer);
    unsubscribeTabTitle();
    await store.stop();
    leaveTui(() => terminal.cleanup());
    // The stdin data listener would otherwise hold the event loop open forever.
    process.exit(0);
  }
}

if (import.meta.main) await main();
