/**
 * Terminal capability detection, and the zero-width escape sequences that act on
 * the terminal itself: tab title, desktop notification, tab/taskbar progress,
 * iTerm2 tab tint, OSC 52 clipboard, focus tracking, the background-colour query,
 * and synchronized-output framing.
 *
 * THE INVARIANT THIS HOLDS: **what a terminal can do is a pure function of its
 * environment, and every decision that depends on it is taken here.** `termCaps`
 * takes an env record and returns booleans; it reads no globals, writes nothing,
 * and needs no TTY — so the whole capability matrix is asserted in `term.test.ts`
 * by handing it fixture environments (plan §7). The old tree computed these as
 * module-level consts off `Deno.env` at import time, which made "does kitty get
 * the protocol pushed" a fact you could only discover by running the TUI inside
 * that terminal.
 *
 * WHY THE GATING EXISTS AT ALL. A sequence one terminal renders is another's
 * garbage, or worse: OSC 9;4 is taskbar progress in Ghostty and iTerm2, and a
 * DESKTOP NOTIFICATION in kitty — an ungated progress keep-alive pops a banner
 * every five seconds. Terminal.app accepts OSC 9 and displays nothing, so it gets
 * BEL instead. tmux swallows OSC it does not recognise, so anything aimed at the
 * outer terminal is passthrough-wrapped, and anything whose outer terminal is
 * unknowable (the iTerm2 tab tint) is simply not sent.
 *
 * THE KITTY FLAG IS ABOUT TRUST, NOT ABOUT PUSHING. `kittyKeyboardMode()` is
 * unconditionally `"enabled"` and says why: ink's `"auto"` probes with a
 * round-trip that tmux swallows, so auto-detection silently loses the protocol on
 * exactly the setup that needs it, while an unsupported push is ignored. What
 * `caps.kitty` decides is whether `key.super` can be BELIEVED — without the
 * protocol a terminal reports Cmd+←/→ as CSI 1;9 C/D and ink leaks bit 3 of the
 * modifier field into the meta flag, so `mouse.ts` intercepts those sequences and
 * `keys.ts` reads the flag to know which of the two paths is live.
 *
 * Effects are behind `createTerm(...)`: the writer and the capability set are
 * injected, so a test drives every sequence into a string buffer. `term()` is the
 * process-wide instance the TUI entry point uses, built lazily so importing this
 * module never touches `Deno.env` or stdout.
 */
import process from "node:process";
import { Buffer } from "node:buffer";

// ---------------------------------------------------------------------------
// Capabilities (pure)
// ---------------------------------------------------------------------------

/** Just enough of an environment to classify a terminal. `Deno.env.toObject()` fits. */
export type TermEnv = Record<string, string | undefined>;

export interface TermCaps {
  /** `TERM_PROGRAM`, verbatim. Kept so a caller can log what was detected. */
  program: string;
  /** `TERM`, verbatim. */
  term: string;
  /** Inside tmux: the outer terminal is unknowable and OSC needs wrapping. */
  tmux: boolean;
  /**
   * The terminal is known to implement the kitty keyboard protocol, so `key.super`
   * is a real modifier rather than a misparse. False under tmux — not because tmux
   * cannot pass it, but because we cannot tell from here whether it will.
   */
  kitty: boolean;
  /** OSC 9;4 is rendered as progress rather than popped as a notification. */
  progress: boolean;
  /** iTerm2's proprietary tab tint is available (and the outer terminal is known). */
  tabColor: boolean;
  /** How a desktop notification is delivered: OSC 9, or a bell for the dock badge. */
  notify: "osc9" | "bell";
}

/** Terminals that ship the kitty keyboard protocol. Membership, not version maths. */
const KITTY_PROGRAMS = ["ghostty", "WezTerm", "iTerm.app", "rio"];
/** …and the ones identifiable only by `TERM`. */
const KITTY_TERMS = ["xterm-kitty", "foot", "foot-extra"];
/** Terminals that render OSC 9;4. kitty parses OSC 9 as a notification — never here. */
const PROGRESS_PROGRAMS = ["ghostty", "iTerm.app", "WezTerm"];

export function termCaps(env: TermEnv): TermCaps {
  const program = env.TERM_PROGRAM ?? "";
  const term = env.TERM ?? "";
  const tmux = !!env.TMUX;
  const kitty = !tmux &&
    (KITTY_PROGRAMS.includes(program) || KITTY_TERMS.includes(term) || !!env.KITTY_WINDOW_ID);
  return {
    program,
    term,
    tmux,
    kitty,
    progress: PROGRESS_PROGRAMS.includes(program),
    tabColor: program === "iTerm.app" && !tmux,
    notify: program === "Apple_Terminal" ? "bell" : "osc9",
  };
}

/**
 * The mode to hand ink's `kittyKeyboard` option — always on, never `"auto"`.
 *
 * `"auto"` asks the terminal and waits for an answer; tmux eats the query, so the
 * one configuration where the extra modifiers matter most is the one auto turns
 * them off for. A terminal without the protocol ignores the push, so forcing it
 * costs a handful of bytes and nothing else.
 */
export function kittyKeyboardMode(): "enabled" {
  return "enabled";
}

/** Titles and notification bodies must not smuggle control bytes into the stream. */
export function sanitize(text: string): string {
  // deno-lint-ignore no-control-regex -- stripping control bytes is the point
  return text.replace(/[\x00-\x1f\x7f]/g, " ");
}

/** tmux swallows unknown OSC — passthrough-wrap so it reaches the outer terminal. */
export function tmuxWrap(seq: string, inTmux: boolean): string {
  return inTmux ? `\x1bPtmux;${seq.replaceAll("\x1b", "\x1b\x1b")}\x1b\\` : seq;
}

/** "rgb:1e1e/1e1e/2e2e" (1–4 hex digits per channel) → "#rrggbb", else null. */
export function parseBgSpec(spec: string): string | null {
  const m = /^rgb:([0-9a-f]{1,4})\/([0-9a-f]{1,4})\/([0-9a-f]{1,4})$/i.exec(spec.trim());
  if (!m) return null;
  // The report is 4/8/12/16-bit per channel; scale each to 8-bit.
  const chan = (h: string) => Math.round((parseInt(h, 16) / (16 ** h.length - 1)) * 255);
  const hex = (v: number) => v.toString(16).padStart(2, "0");
  return `#${hex(chan(m[1]))}${hex(chan(m[2]))}${hex(chan(m[3]))}`;
}

/** Rec. 709 luma, split at the midpoint. Pure, so the boundary is testable. */
export function classifyBg(hex: string): { hex: string; scheme: "dark" | "light" } {
  const [r, g, b] = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16));
  const luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
  return { hex, scheme: luma < 128 ? "dark" : "light" };
}

// ---------------------------------------------------------------------------
// Effects
// ---------------------------------------------------------------------------

export interface TermOptions {
  caps: TermCaps;
  /** Where sequences go. A test passes a collector; production writes to stdout. */
  write: (seq: string) => void;
  /** Absent = the real timers. Injected so a test never waits on the keep-alive. */
  setInterval?: (fn: () => void, ms: number) => number;
  clearInterval?: (handle: number) => void;
  setTimeout?: (fn: () => void, ms: number) => number;
  clearTimeout?: (handle: number) => void;
}

export interface Term {
  caps: TermCaps;
  /** Name the tab (and, under tmux, the window). */
  setTitle(title: string): void;
  /** Only while unfocused: a banner about the screen you are looking at is noise. */
  notifyDesktop(body: string): void;
  /** Indeterminate progress while a turn runs, kept alive until `progressEnd`. */
  progressStart(): void;
  progressEnd(error?: boolean): void;
  /** Tint the iTerm2 tab; null resets. No-op anywhere else. */
  tabColor(hex: string | null): void;
  /** Escape-sequence clipboard write — reaches the LOCAL terminal over SSH/tmux. */
  osc52Copy(text: string): void;
  /** Ask for the background colour; the reply arrives on stdin, via `mouse.ts`. */
  queryTermBg(): void;
  /** Fed by the stdin filter. A malformed report never clobbers a good one. */
  reportTermBg(spec: string): void;
  termBackground(): { hex: string; scheme: "dark" | "light" } | null;
  /** Fed by the stdin filter (`\x1b[I` / `\x1b[O`). */
  setFocused(v: boolean): void;
  isFocused(): boolean;
  /** Clear every sticky state this object set. Called on the way out. */
  cleanup(): void;
}

export function createTerm(options: TermOptions): Term {
  const { caps, write } = options;
  const every = options.setInterval ?? ((fn, ms) => setInterval(fn, ms) as unknown as number);
  const stopEvery = options.clearInterval ?? ((h) => clearInterval(h));
  const after = options.setTimeout ?? ((fn, ms) => setTimeout(fn, ms) as unknown as number);
  const stopAfter = options.clearTimeout ?? ((h) => clearTimeout(h));

  let progressTimer: number | null = null;
  let progressErrTimer: number | null = null;
  let focused = true;
  let bg: string | null = null;

  const term: Term = {
    caps,

    setTitle(title) {
      const t = sanitize(title);
      write(`\x1b]0;${t}\x07`);
      // Under tmux OSC 0 names the pane; \x1bk additionally names the window.
      if (caps.tmux) write(`\x1bk${t}\x1b\\`);
    },

    notifyDesktop(body) {
      if (focused) return;
      if (caps.notify === "bell") {
        write("\x07");
        return;
      }
      write(tmuxWrap(`\x1b]9;${sanitize(body)}\x07`, caps.tmux));
    },

    progressStart() {
      if (!caps.progress) return;
      if (progressErrTimer !== null) {
        stopAfter(progressErrTimer);
        progressErrTimer = null;
      }
      write("\x1b]9;4;3\x07");
      // Ghostty expires stale progress after ~15s, so re-assert it until the end.
      progressTimer ??= every(() => write("\x1b]9;4;3\x07"), 5000);
    },

    progressEnd(error = false) {
      if (!caps.progress) return;
      if (progressTimer !== null) {
        stopEvery(progressTimer);
        progressTimer = null;
      }
      if (!error) {
        write("\x1b]9;4;0\x07");
        return;
      }
      write("\x1b]9;4;2;100\x07");
      progressErrTimer ??= after(() => {
        progressErrTimer = null;
        write("\x1b]9;4;0\x07");
      }, 4000);
    },

    tabColor(hex) {
      if (!caps.tabColor) return;
      if (!hex) {
        write("\x1b]6;1;bg;*;default\x07");
        return;
      }
      const m = /^#?([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex);
      if (!m) return;
      const [r, g, b] = [m[1], m[2], m[3]].map((h) => parseInt(h, 16));
      write(
        `\x1b]6;1;bg;red;brightness;${r}\x07` +
          `\x1b]6;1;bg;green;brightness;${g}\x07` +
          `\x1b]6;1;bg;blue;brightness;${b}\x07`,
      );
    },

    osc52Copy(text) {
      // Capped well under xterm's ~100KB whole-sequence limit. No tmux wrap: tmux
      // translates OSC 52 itself when set-clipboard is on.
      write(`\x1b]52;c;${Buffer.from(text.slice(0, 72_000)).toString("base64")}\x07`);
    },

    queryTermBg() {
      write("\x1b]11;?\x07");
    },

    reportTermBg(spec) {
      bg = parseBgSpec(spec) ?? bg;
    },

    termBackground() {
      return bg ? classifyBg(bg) : null;
    },

    setFocused(v) {
      focused = v;
    },
    isFocused() {
      return focused;
    },

    cleanup() {
      if (progressTimer !== null) {
        stopEvery(progressTimer);
        progressTimer = null;
      }
      if (progressErrTimer !== null) {
        stopAfter(progressErrTimer);
        progressErrTimer = null;
      }
      if (caps.progress) write("\x1b]9;4;0\x07");
      if (caps.tabColor) write("\x1b]6;1;bg;*;default\x07");
    },
  };
  return term;
}

// ---------------------------------------------------------------------------
// The process-wide instance
// ---------------------------------------------------------------------------

const enc = new TextEncoder();

function readEnv(): TermEnv {
  try {
    return Deno.env.toObject();
  } catch {
    // No --allow-env (a test, a sandboxed child): an unknown terminal is the
    // conservative answer, and every capability above defaults to off.
    return {};
  }
}

function stdoutIsTty(): boolean {
  try {
    return Deno.stdout.isTerminal();
  } catch {
    return false;
  }
}

let instance: Term | null = null;

/**
 * The TUI's terminal, built on first use.
 *
 * Lazy on purpose: importing this module must not read the environment or touch
 * stdout, or `keys.test.ts` and every other headless test would need permissions
 * they have no reason to hold.
 */
export function term(): Term {
  if (instance) return instance;
  const isTty = stdoutIsTty();
  instance = createTerm({
    caps: termCaps(readEnv()),
    write: (seq) => {
      if (!isTty) return;
      try {
        Deno.stdout.writeSync(enc.encode(seq));
      } catch {
        // stdout gone (exiting) — there is nothing left to signal to.
      }
    },
  });
  return instance;
}

// ---------------------------------------------------------------------------
// Synchronized output (DEC 2026)
// ---------------------------------------------------------------------------

/**
 * A stdout for ink whose every write is wrapped in begin/end-synchronized-update,
 * so the terminal repaints each frame atomically instead of mid-erase. Ink writes
 * one string per frame, so no partial frame can slip out between the guards; a
 * terminal without mode 2026 ignores them.
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
