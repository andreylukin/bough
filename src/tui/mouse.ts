/**
 * The stdin filter: everything the terminal sends that is not a keystroke gets
 * taken out of the stream before ink ever sees it.
 *
 * THE INVARIANT THIS HOLDS: **ink's input parser only ever receives keystrokes.**
 * Ink has no notion of mouse reporting, bracketed paste, focus events or OSC
 * replies, so every one of them would otherwise arrive as a burst of garbage
 * "keys" — an SGR mouse drag types `[<32;40;12M` into the composer, and the
 * terminal's answer to the background-colour query types `]11;rgb:…` into it. We
 * therefore sit between the real stdin and ink: a `PassThrough` carries what is
 * left after `createInputFilter` has consumed the sequences it recognises and
 * dispatched them to handlers.
 *
 * SECOND INVARIANT — **the filter is a pure state machine over strings.**
 * `createInputFilter` takes chunks and returns the bytes to forward; it touches no
 * process, no stream and no terminal, so a paste split across three reads and a
 * mouse drag arriving mid-paste are testable directly (plan §7). `filteredStdin`
 * is the ten lines that bolt it to `process.stdin`.
 *
 * THIRD — **only sequences that can actually split across reads are held.** The
 * partial-tail pattern deliberately requires the distinguishing third byte (`[<`
 * mouse, `[2` paste marker, `[1;9` cmd-arrow): holding a bare ESC would swallow
 * the Escape KEY until the next keypress, and Escape is how you leave every panel
 * in this TUI. Terminals emit key sequences atomically; in practice only these
 * multi-byte reports ever arrive in pieces.
 *
 * FOURTH — **Home/End and Cmd+←/→ are intercepted here, not bound in ink.** Ink's
 * parser drops the Home/End sequences, and on a terminal without the kitty
 * keyboard protocol it misparses Cmd+←/→ (`CSI 1;9 C/D`) as meta+arrow, because
 * bit 3 of the modifier field leaks into the meta flag. Both are dispatched like
 * paste, and `keys.ts` binds the same commands to the kitty-protocol chords, so
 * the two paths converge (`term.ts` says which one is live).
 */
import { PassThrough } from "node:stream";
import { Buffer } from "node:buffer";
import process from "node:process";

/** A mouse report, in 1-based terminal cells. */
export interface MouseEvent {
  x: number;
  y: number;
  /**
   * The left button reports its whole press/drag/release cycle, so the app can
   * tell a click (down+up in place) from a drag selection.
   */
  kind: "down" | "drag" | "up" | "right-click" | "wheel-up" | "wheel-down";
}

/** Keys ink does not deliver, or delivers wrong. See the fourth invariant. */
export type NavKey = "home" | "end" | "cmdHome" | "cmdEnd" | "shiftTab";

/** Where the filter sends what it consumes. Every handler is optional. */
export interface InputSinks {
  mouse?: (event: MouseEvent) => void;
  /** Bracketed pastes arrive WHOLE here, newlines normalized, never through ink. */
  paste?: (text: string) => void;
  navKey?: (key: NavKey) => void;
  /** Terminal focus in/out (mode 1004) — gates desktop notifications. */
  focus?: (focused: boolean) => void;
  /** The OSC 11 background report's payload, e.g. `"rgb:1e1e/1e1e/2e2e"`. */
  bgReport?: (spec: string) => void;
}

// deno-lint-ignore no-control-regex -- ESC is the point: SGR mouse sequences
const SGR = /\x1b\[<(\d+);(\d+);(\d+)([Mm])/g;
// Home = [H | OH | [1~ · End = [F | OF | [4~
// deno-lint-ignore no-control-regex -- ESC is the point
const NAV_KEY = /\x1b(?:\[(?:[HF]|[14]~)|O[HF])/g;
// Cmd+←/→ = [1;9D / [1;9C (xterm modifier 9 = super; iTerm2 and others)
// deno-lint-ignore no-control-regex -- ESC is the point
const CMD_ARROW = /\x1b\[1;9([CD])/g;
/**
 * `CSI Z` — backtab, which is the ONLY thing a terminal sends for shift+tab.
 *
 * ink does not decode it, so it fell through and was read as a plain tab: the
 * panel's ⇧⇥ moved FORWARD, and `panel.prev` — bound and documented as
 * "next / previous tab" — was unreachable by any keypress. Delivered as a nav key
 * for the same reason cmd+arrow is: the app must be able to bind it, and ink has
 * no flag that would carry it.
 */
// deno-lint-ignore no-control-regex -- ESC is the point
const BACKTAB = /\x1b\[Z/g;
// deno-lint-ignore no-control-regex -- ESC is the point
const FOCUS = /\x1b\[([IO])/g;
// deno-lint-ignore no-control-regex -- ESC is the point
const BG_REPORT = /\x1b\]11;([^\x07\x1b]*)(?:\x07|\x1b\\)/g;
const PASTE_START = "\x1b[200~";
const PASTE_END = "\x1b[201~";
/**
 * The `CSI 27;<mods>;<code>~` form — modifyOtherKeys / the kitty keyboard protocol's
 * legacy encoding for a key that has no plain byte of its own.
 *
 * ink cannot parse it, and forwarding it unchanged is worse than dropping it: ink
 * splits the escape byte from the rest and delivers the remainder as ordinary
 * text, so pressing ⌥⏎ — a documented binding — typed its own encoding into the
 * draft:
 *
 *   › and then say done[27;3;13~
 *
 * So it is decoded here, into the bytes ink already understands, and a form that
 * cannot be decoded is swallowed rather than typed.
 */
// deno-lint-ignore no-control-regex -- ESC is the point
const MODIFY_OTHER = /\x1b\[27;(\d+);(\d+)~/g;

/**
 * `CSI 27;mods;code~` → the bytes ink parses, or "" when there is no equivalent.
 *
 * `mods` is a 1-based bitfield: subtract one, then bit 0 is shift, 1 is alt, 2 is
 * ctrl, 3 is super. ink reads a leading ESC as meta and a C0 byte as ctrl, which
 * covers every combination bough actually binds.
 */
export function decodeModifyOther(mods: number, code: number): string {
  const bits = Math.max(0, mods - 1);
  const alt = (bits & 2) !== 0;
  const ctrl = (bits & 4) !== 0;
  // Only the codes that are a real character or a key ink knows by byte. A
  // function key or a keypad code has no byte form and is dropped.
  if (code < 1 || code > 0x10ffff) return "";
  let base: string;
  if (code === 13) base = "\r";
  else if (code === 9) base = "\t";
  else if (code === 27) base = "\x1b";
  else if (code === 127 || code === 8) base = "\x7f";
  else if (code >= 32 && code < 127) base = String.fromCharCode(code);
  else return "";
  // Ctrl folds a letter down to its C0 byte, which is how ink reports ctrl at all.
  if (ctrl && base >= "a" && base <= "z") base = String.fromCharCode(base.charCodeAt(0) - 96);
  else if (ctrl && base >= "A" && base <= "Z") {
    base = String.fromCharCode(base.charCodeAt(0) - 64);
  }
  return alt ? `\x1b${base}` : base;
}

// A trailing fragment that could grow into one of the above on the next read.
//
// Every alternative is INCOMPLETE by construction: a mouse report ends in M/m and
// a cmd-arrow in C/D, neither of which the classes admit, so a sequence that
// arrived whole is dispatched now rather than held until the next keypress. The
// old tree wrote `1;9[CD]?` here, which held a COMPLETE cmd-arrow and delivered it
// one keystroke late.
// deno-lint-ignore no-control-regex -- ESC is the point
const PARTIAL_TAIL = /\x1b\[(<[\d;]*|20[01]?|1(;9?)?|2(7(;\d*(;\d*)?)?)?)$/;

export interface InputFilter {
  /** One raw chunk in, the bytes ink should see out. */
  feed(chunk: string): string;
}

export function createInputFilter(sinks: InputSinks = {}): InputFilter {
  let carry = "";
  let inPaste = false;
  let pasteBuf = "";

  /** Terminal REPLIES and the keys ink mishandles. Consumed, never forwarded. */
  function dispatchReports(s: string): string {
    return s
      .replace(FOCUS, (_all, io) => {
        sinks.focus?.(io === "I");
        return "";
      })
      .replace(BG_REPORT, (_all, spec) => {
        sinks.bgReport?.(spec);
        return "";
      })
      // Before NAV_KEY: `[1;9D` would otherwise be matched by neither, but the
      // ordering states the intent — the modifier form is the more specific one.
      .replace(BACKTAB, () => {
        sinks.navKey?.("shiftTab");
        return "";
      })
      .replace(CMD_ARROW, (_all, dir) => {
        sinks.navKey?.(dir === "D" ? "cmdHome" : "cmdEnd");
        return "";
      })
      .replace(NAV_KEY, (m) => {
        sinks.navKey?.(m.includes("H") || m.includes("1") ? "home" : "end");
        return "";
      })
      // Last: everything above is a MORE specific shape, and this one must never
      // fall through to ink as text.
      .replace(MODIFY_OTHER, (_all, mods, code) => {
        // Shift+Tab arrives HERE under the kitty protocol (`CSI 27;2;9~`), not as
        // `CSI Z`, and decoding it by the general rule yields a bare tab — which is
        // why ⇧⇥ moved FORWARD through the panel instead of back. It is a nav key
        // for the same reason backtab is: ink has no flag that carries it.
        if (Number(code) === 9 && ((Number(mods) - 1) & 1) !== 0) {
          sinks.navKey?.("shiftTab");
          return "";
        }
        return decodeModifyOther(Number(mods), Number(code));
      });
  }

  function dispatchMouse(s: string): string {
    return s.replace(SGR, (_all, b, x, y, fin) => {
      const btn = Number(b);
      const at = { x: Number(x), y: Number(y) };
      const emit = sinks.mouse;
      if (!emit) return "";
      // 64/65 = wheel; bit 32 = motion while held (mode 1002 drag reports).
      if (fin === "M") {
        if (btn === 64) emit({ ...at, kind: "wheel-up" });
        else if (btn === 65) emit({ ...at, kind: "wheel-down" });
        else if (btn & 32) {
          if ((btn & 3) === 0) emit({ ...at, kind: "drag" });
        } else if ((btn & 3) === 0) emit({ ...at, kind: "down" });
        else if ((btn & 3) === 2) emit({ ...at, kind: "right-click" });
      } else if ((btn & 3) === 0 && !(btn & 32)) {
        emit({ ...at, kind: "up" });
      }
      return "";
    });
  }

  return {
    feed(chunk: string): string {
      let s = carry + chunk;
      carry = "";
      let forwarded = "";
      while (s.length > 0) {
        if (inPaste) {
          const end = s.indexOf(PASTE_END);
          if (end < 0) {
            // Hold back anything that could be the start of the end marker.
            const tail = PARTIAL_TAIL.exec(s)?.[0] ?? "";
            pasteBuf += s.slice(0, s.length - tail.length);
            carry = tail;
            break;
          }
          pasteBuf += s.slice(0, end);
          s = s.slice(end + PASTE_END.length);
          inPaste = false;
          sinks.paste?.(pasteBuf.replace(/\r\n?/g, "\n"));
          pasteBuf = "";
          continue;
        }
        const start = s.indexOf(PASTE_START);
        if (start < 0) {
          const tail = PARTIAL_TAIL.exec(s)?.[0] ?? "";
          forwarded += dispatchReports(dispatchMouse(s.slice(0, s.length - tail.length)));
          carry = tail;
          break;
        }
        forwarded += dispatchReports(dispatchMouse(s.slice(0, start)));
        s = s.slice(start + PASTE_START.length);
        inPaste = true;
      }
      return forwarded;
    },
  };
}

/**
 * An ink-compatible stdin that carries only keystrokes.
 *
 * Bytes are decoded as latin1 both ways so the filter's arithmetic is over single
 * code units and a multi-byte UTF-8 character survives the round trip untouched.
 */
export function filteredStdin(sinks: InputSinks = {}): typeof process.stdin {
  const out = new PassThrough();
  const filter = createInputFilter(sinks);
  process.stdin.on("data", (chunk: Buffer | string) => {
    const forwarded = filter.feed(
      typeof chunk === "string" ? chunk : chunk.toString("latin1"),
    );
    if (forwarded) out.write(Buffer.from(forwarded, "latin1"));
  });
  // Ink probes these on whatever stream it is handed.
  const fake = out as unknown as typeof process.stdin;
  (fake as unknown as { isTTY: boolean }).isTTY = process.stdin.isTTY;
  (fake as unknown as { setRawMode: (v: boolean) => void }).setRawMode = (v: boolean) => {
    if (process.stdin.isTTY) process.stdin.setRawMode(v);
  };
  (fake as unknown as { ref: () => void }).ref = () => process.stdin.ref?.();
  (fake as unknown as { unref: () => void }).unref = () => process.stdin.unref?.();
  return fake;
}

// ---------------------------------------------------------------------------
// Entering and leaving the alternate screen
// ---------------------------------------------------------------------------

const enc = new TextEncoder();

function writeOut(seq: string) {
  try {
    Deno.stdout.writeSync(enc.encode(seq));
  } catch {
    // stdout already gone — nothing to set up or restore onto.
  }
}

/**
 * Alt screen + SGR mouse tracking + bracketed paste + focus reporting.
 *
 * 1002 (button-event) adds the drag motion that text selection needs; 1000 stays
 * as the fallback for terminals without it. 1004 reports focus in/out, which gates
 * desktop notifications. `CSI 22;0t` pushes the terminal's current title so
 * `leaveTui` can pop it back rather than leaving a session id in the tab.
 */
export function enterTui() {
  writeOut("\x1b[22;0t\x1b[?1049h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[?1004h");
}

/**
 * Restore the normal buffer: mouse, paste and focus modes off, cursor visible,
 * the pushed title popped back.
 *
 * `cleanup` clears whatever sticky state `term.ts` set (progress, tab tint). It is
 * a parameter rather than an import so this module holds no reference to the
 * process-wide terminal, and so a caller that never built one can still leave.
 */
export function leaveTui(cleanup?: () => void) {
  try {
    cleanup?.();
  } catch {
    // Leaving must not throw: this runs on the unload path too.
  }
  writeOut("\x1b[?1004l\x1b[?2004l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?1049l\x1b[?25h\x1b[23;0t");
}
