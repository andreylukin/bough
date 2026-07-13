// TUI entry: preflight the server (logging in if it wants a password), then hand
// off to the Ink app. Run via `bough` (scripts/bough auto-starts the server) or
// `deno task tui`.
import { render } from "ink";
import { api, AuthError, BASE, setCookie } from "./api.ts";
import { loadCookie, saveCookie } from "./state.ts";
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
  // exitOnCtrlC off: the app implements double-ctrl+c itself (a single ^c would
  // otherwise unmount ink out from under the quit-hint).
  render(<App initialSessions={sessions} />, { exitOnCtrlC: false });
}

main();
