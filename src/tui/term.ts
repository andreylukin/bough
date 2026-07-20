/**
 * Terminal integration beyond ink's renderer: tab title, desktop notifications,
 * tab/taskbar progress, iTerm2 tab color, OSC 52 clipboard, focus tracking, and
 * the terminal-background query. Everything here is a zero-width OSC/CSI write
 * straight to stdout — sequences that print nothing and move no cursor, so they
 * can't disturb ink's frame bookkeeping — gated per-terminal (a sequence one
 * terminal renders is another's garbage or, worse, kitty popping OSC 9;4
 * progress as a desktop notification) and wrapped for tmux where tmux would
 * otherwise swallow them. Focus/background REPORTS arrive on stdin and are
 * stripped + dispatched here by the mouse.ts filter, never reaching ink.
 */
import process from "node:process";
import { Buffer } from "node:buffer";

const enc = new TextEncoder();
const isTty = (() => {
  try {
    return Deno.stdout.isTerminal();
  } catch {
    return false;
  }
})();

const TERM_PROGRAM = Deno.env.get("TERM_PROGRAM") ?? "";
const TMUX = !!Deno.env.get("TMUX");
const ITERM = TERM_PROGRAM === "iTerm.app";
const APPLE_TERMINAL = TERM_PROGRAM === "Apple_Terminal";
// OSC 9;4 progress: only terminals known to render it. kitty (and unknown
// terminals) parse OSC 9 as a desktop notification, so a stray "9;4;3" payload
// would pop notification spam once a second.
const PROGRESS_OK = ["ghostty", "iTerm.app", "WezTerm"].includes(TERM_PROGRAM);

function emit(seq: string) {
  if (!isTty) return;
  try {
    Deno.stdout.writeSync(enc.encode(seq));
  } catch {
    // stdout gone (exiting) — nothing to signal to
  }
}

/** Titles/notification bodies must not smuggle control bytes into the stream. */
export function sanitize(text: string): string {
  // deno-lint-ignore no-control-regex -- stripping control bytes is the point
  return text.replace(/[\x00-\x1f\x7f]/g, " ");
}

/** tmux swallows unknown OSC — passthrough-wrap so it reaches the outer terminal. */
export function tmuxWrap(seq: string, inTmux = TMUX): string {
  return inTmux ? `\x1bPtmux;${seq.replaceAll("\x1b", "\x1b\x1b")}\x1b\\` : seq;
}

// ---- tab / window title ------------------------------------------------------

/** OSC 0 names the tab (under tmux: the pane; \x1bk additionally names the tmux
 * window, gated user-side by allow-rename). enterTui pushed the old title
 * (CSI 22;0t); leaveTui pops it back. */
export function setTitle(title: string) {
  const t = sanitize(title);
  emit(`\x1b]0;${t}\x07`);
  if (TMUX) emit(`\x1bk${t}\x1b\\`);
}

// ---- desktop notifications ---------------------------------------------------

/** Fire a desktop notification — only while the terminal is unfocused (a banner
 * about the screen you're looking at is noise). OSC 9 is what iTerm2/Ghostty/
 * kitty/VS Code render; Terminal.app accepts but doesn't display it, so it gets
 * BEL (dock badge) instead. */
export function notifyDesktop(body: string) {
  if (focused) return;
  if (APPLE_TERMINAL) {
    emit("\x07");
    return;
  }
  emit(tmuxWrap(`\x1b]9;${sanitize(body)}\x07`));
}

// ---- tab/taskbar progress (OSC 9;4) ------------------------------------------

let progressTimer: ReturnType<typeof setInterval> | null = null;
let progressErrTimer: ReturnType<typeof setTimeout> | null = null;

/** Indeterminate progress on the tab while a turn runs. Ghostty expires stale
 * progress after ~15s, so a keep-alive re-emits until progressEnd. */
export function progressStart() {
  if (!PROGRESS_OK) return;
  if (progressErrTimer) {
    clearTimeout(progressErrTimer);
    progressErrTimer = null;
  }
  emit("\x1b]9;4;3\x07");
  progressTimer ??= setInterval(() => emit("\x1b]9;4;3\x07"), 5000);
}

/** Clear the progress state; an errored turn flashes the error state first. */
export function progressEnd(error = false) {
  if (!PROGRESS_OK) return;
  if (progressTimer) {
    clearInterval(progressTimer);
    progressTimer = null;
  }
  if (error) {
    emit("\x1b]9;4;2;100\x07");
    progressErrTimer ??= setTimeout(() => {
      progressErrTimer = null;
      emit("\x1b]9;4;0\x07");
    }, 4000);
  } else {
    emit("\x1b]9;4;0\x07");
  }
}

// ---- iTerm2 tab color --------------------------------------------------------

/** Tint the iTerm2 tab (e.g. amber while an approval waits); null resets. No-op
 * elsewhere, and under tmux (the outer terminal is unknowable). */
export function tabColor(hex: string | null) {
  if (!ITERM || TMUX) return;
  if (!hex) {
    emit("\x1b]6;1;bg;*;default\x07");
    return;
  }
  const m = /^#?([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex);
  if (!m) return;
  const [r, g, b] = [m[1], m[2], m[3]].map((h) => parseInt(h, 16));
  emit(
    `\x1b]6;1;bg;red;brightness;${r}\x07\x1b]6;1;bg;green;brightness;${g}\x07\x1b]6;1;bg;blue;brightness;${b}\x07`,
  );
}

// ---- OSC 52 clipboard --------------------------------------------------------

/** Escape-sequence clipboard write — reaches the LOCAL terminal even over
 * SSH/tmux (tmux translates it when set-clipboard is on, so no passthrough
 * wrap). Fallback path when pbcopy isn't reachable; capped well under xterm's
 * ~100KB whole-sequence limit. */
export function osc52Copy(text: string) {
  emit(`\x1b]52;c;${Buffer.from(text.slice(0, 72_000)).toString("base64")}\x07`);
}

// ---- focus tracking (CSI ?1004) ----------------------------------------------

let focused = true;

/** Fed by the mouse.ts stdin filter (\x1b[I / \x1b[O). */
export function setFocused(v: boolean) {
  focused = v;
}
export function isFocused(): boolean {
  return focused;
}

// ---- terminal background (OSC 11) --------------------------------------------

let termBg: string | null = null; // "#rrggbb" once the terminal reports it

/** Ask the terminal for its background color; the reply arrives on stdin and is
 * routed to reportTermBg by the mouse.ts filter. */
export function queryTermBg() {
  emit("\x1b]11;?\x07");
}

/** "rgb:1e1e/1e1e/2e2e" (1/2/4 hex digits per channel) → "#rrggbb", else null. */
export function parseBgSpec(spec: string): string | null {
  const m = /^rgb:([0-9a-f]{1,4})\/([0-9a-f]{1,4})\/([0-9a-f]{1,4})$/i.exec(spec.trim());
  if (!m) return null;
  // Scale each channel to 8-bit: the report is 4/8/12/16-bit per channel.
  const chan = (h: string) => Math.round((parseInt(h, 16) / (16 ** h.length - 1)) * 255);
  const hex = (v: number) => v.toString(16).padStart(2, "0");
  return `#${hex(chan(m[1]))}${hex(chan(m[2]))}${hex(chan(m[3]))}`;
}

export function reportTermBg(spec: string) {
  termBg = parseBgSpec(spec) ?? termBg;
}

/** The terminal's own background, classified — null until (unless) it reports. */
export function termBackground(): { hex: string; scheme: "dark" | "light" } | null {
  if (!termBg) return null;
  const [r, g, b] = [1, 3, 5].map((i) => parseInt(termBg!.slice(i, i + 2), 16));
  // Rec. 709 luma; 128 splits dark from light.
  const luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
  return { hex: termBg, scheme: luma < 128 ? "dark" : "light" };
}

// ---- synchronized output (DEC 2026) ------------------------------------------

/**
 * A stdout for ink whose every write is wrapped in begin/end-synchronized-update
 * — the terminal repaints each frame atomically instead of mid-erase, killing
 * streaming flicker. One write() call per frame, so no partial frame can slip
 * out between the guards. Terminals without mode 2026 ignore the guards.
 */
export function syncedStdout(): typeof process.stdout {
  const real = process.stdout;
  return new Proxy(real, {
    get(target, prop) {
      if (prop === "write") {
        return (data: string | Uint8Array, ...rest: unknown[]) =>
          typeof data === "string"
            // deno-lint-ignore no-explicit-any -- variadic write(cb?/encoding?) passthrough
            ? (target.write as any)(`\x1b[?2026h${data}\x1b[?2026l`, ...rest)
            // deno-lint-ignore no-explicit-any
            : (target.write as any)(data, ...rest);
      }
      const v = Reflect.get(target, prop, target);
      return typeof v === "function" ? v.bind(target) : v;
    },
  }) as typeof process.stdout;
}

// ---- teardown ----------------------------------------------------------------

/** Clear every sticky terminal state this module set; called from leaveTui. */
export function termCleanup() {
  if (progressTimer) {
    clearInterval(progressTimer);
    progressTimer = null;
  }
  if (progressErrTimer) {
    clearTimeout(progressErrTimer);
    progressErrTimer = null;
  }
  if (PROGRESS_OK) emit("\x1b]9;4;0\x07");
  if (ITERM && !TMUX) emit("\x1b]6;1;bg;*;default\x07");
}
