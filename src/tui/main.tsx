// TUI entry: preflight the server (logging in if it wants a password), then hand
// off to the Ink app. Run via `bough` (scripts/bough auto-starts the server) or
// `deno task tui`.
import { render } from "ink";
import { api, AuthError, BASE, setCookie } from "./api.ts";
import { loadTheme } from "./theme.ts";
import { loadCookie, saveCookie } from "./state.ts";
import { enterTui, filteredStdin, leaveTui } from "./mouse.ts";
import { queryTermBg, syncedStdout } from "./term.ts";
import { App } from "./components/App.tsx";

// Read a password from the tty without echoing (raw mode, pre-Ink).
async function promptPassword(label: string): Promise<string> {
  const enc = new TextEncoder();
  await Deno.stdout.write(enc.encode(label));
  Deno.stdin.setRaw(true);
  let pw = "";
  const buf = new Uint8Array(64);
  try {
    for (;;) {
      const n = await Deno.stdin.read(buf);
      if (n === null) break;
      for (const b of buf.subarray(0, n)) {
        if (b === 0x03) {
          // ctrl+c
          Deno.stdin.setRaw(false);
          await Deno.stdout.write(enc.encode("\n"));
          Deno.exit(130);
        }
        if (b === 0x0d || b === 0x0a) {
          Deno.stdin.setRaw(false);
          await Deno.stdout.write(enc.encode("\n"));
          return pw;
        }
        if (b === 0x7f || b === 0x08) pw = pw.slice(0, -1);
        else if (b >= 0x20) pw += String.fromCharCode(b);
      }
    }
  } finally {
    Deno.stdin.setRaw(false);
  }
  return pw;
}

async function preflight() {
  setCookie(loadCookie());
  for (let attempt = 0;; attempt++) {
    try {
      return await api.listSessions();
    } catch (e) {
      if (e instanceof AuthError && attempt < 3) {
        const pw = await promptPassword(
          attempt === 0 ? "bough password: " : "wrong password, try again: ",
        );
        try {
          saveCookie(await api.login(pw));
        } catch {
          // wrong password — loop prompts again (attempt counts the 401s)
        }
        continue;
      }
      console.error(
        `bough tui: can't reach the server at ${BASE} (${e instanceof Error ? e.message : e})`,
      );
      console.error("start it with: bough start   (or check: bough status / bough logs)");
      Deno.exit(1);
    }
  }
}

async function main() {
  const sessions = await preflight();
  // The stored web-UI theme also colors the TUI (accents/borders) — load it
  // before first paint so the initial frame is already themed.
  await loadTheme();
  // New conversations default to where `bough` was launched (scripts/bough exports
  // this before cd'ing to the repo root).
  const defaultWorkspace = Deno.env.get("BOUGH_TUI_CWD") ?? Deno.cwd();
  // Alternate screen + mouse tracking; always restored on the way out (including
  // the unload backstop for uncaught crashes).
  enterTui();
  globalThis.addEventListener("unload", leaveTui);
  // exitOnCtrlC off: the app implements double-ctrl+c itself (a single ^c would
  // otherwise unmount ink out from under the quit-hint). stdin goes through the
  // mouse filter so ink never sees SGR mouse sequences; stdout wraps each frame
  // in synchronized-update guards (DEC 2026) so terminals repaint atomically.
  // kittyKeyboard 'enabled' (not 'auto': detection fails under tmux) makes
  // modified keys like shift+enter distinguishable where the terminal supports
  // the protocol; elsewhere the push is ignored harmlessly.
  const inst = render(
    <App initialSessions={sessions} defaultWorkspace={defaultWorkspace} />,
    {
      exitOnCtrlC: false,
      stdin: filteredStdin(),
      stdout: syncedStdout(),
      kittyKeyboard: { mode: "enabled" },
    },
  );
  // Ask for the terminal's background color now that ink has stdin in raw mode;
  // the reply is consumed by the stdin filter (never seen as keystrokes).
  queryTermBg();
  try {
    await inst.waitUntilExit();
  } finally {
    leaveTui();
    Deno.exit(0); // the stdin data listener would otherwise keep the loop alive
  }
}

main();
