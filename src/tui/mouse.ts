// Mouse + paste support. Ink has no handling for either, so we sit between the
// real stdin and ink: a PassThrough stream receives everything the terminal
// sends; SGR mouse sequences (\x1b[<b;x;yM / m) and bracketed pastes
// (\x1b[200~ … \x1b[201~) are consumed here and dispatched to handlers, and
// every other byte is forwarded to the stream ink reads from.
import { PassThrough } from "node:stream";
import process from "node:process";
import { reportTermBg, setFocused, termCleanup } from "./term.ts";

export interface MouseEvent {
  /** 1-based terminal column/row of the event. */
  x: number;
  y: number;
  /** Left button reports the full press/drag/release cycle so the app can
   * distinguish a click (down+up in place) from a drag selection. */
  kind: "down" | "drag" | "up" | "right-click" | "wheel-up" | "wheel-down";
}

type Handler = (ev: MouseEvent) => void;
let handler: Handler | null = null;
export function onMouse(h: Handler | null) {
  handler = h;
}

/** Bracketed pastes arrive whole here (newlines normalized), never through ink. */
type PasteHandler = (text: string) => void;
let pasteHandler: PasteHandler | null = null;
export function onPaste(h: PasteHandler | null) {
  pasteHandler = h;
}

/** Physical Home/End keys: ink's parser drops their sequences, so they're
 * decoded here (all three encodings terminals use) and dispatched like paste. */
type NavKeyHandler = (k: "home" | "end") => void;
let navKeyHandler: NavKeyHandler | null = null;
export function onNavKey(h: NavKeyHandler | null) {
  navKeyHandler = h;
}

// deno-lint-ignore no-control-regex -- ESC is the point: SGR mouse sequences
const SGR = /\x1b\[<(\d+);(\d+);(\d+)([Mm])/g;
// Home = \x1b[H | \x1bOH | \x1b[1~ · End = \x1b[F | \x1bOF | \x1b[4~
// deno-lint-ignore no-control-regex -- ESC is the point
const NAV_KEY = /\x1b(?:\[(?:[HF]|[14]~)|O[HF])/g;
const PASTE_START = "\x1b[200~";
const PASTE_END = "\x1b[201~";
// A trailing fragment that could grow into one of our sequences next chunk.
// Deliberately requires the distinguishing third byte ("[<" mouse, "[2" paste
// marker): holding a bare ESC would swallow the Escape KEY until the next
// keypress (terminals send key sequences atomically; only our two multi-byte
// sequences ever split across reads in practice).
// deno-lint-ignore no-control-regex -- ESC is the point
const PARTIAL_TAIL = /\x1b\[(<[\d;]*|20[01]?)$/;

// Focus in/out (mode 1004) and the OSC 11 background report are terminal
// REPLIES, not keystrokes — consumed here (term.ts keeps the state) so they
// never leak into ink's input parser as garbage keys. Like the mouse sequences
// above, terminals send them atomically, so no cross-chunk reassembly.
// deno-lint-ignore no-control-regex -- ESC is the point
const FOCUS = /\x1b\[([IO])/g;
// deno-lint-ignore no-control-regex -- ESC is the point
const BG_REPORT = /\x1b\]11;([^\x07\x1b]*)(?:\x07|\x1b\\)/g;

function dispatchReports(s: string): string {
  return s
    .replace(FOCUS, (_all, io) => {
      setFocused(io === "I");
      return "";
    })
    .replace(BG_REPORT, (_all, spec) => {
      reportTermBg(spec);
      return "";
    })
    .replace(NAV_KEY, (m) => {
      navKeyHandler?.(m.includes("H") || m.includes("1") ? "home" : "end");
      return "";
    });
}

function dispatchMouse(s: string): string {
  return s.replace(SGR, (_all, b, x, y, fin) => {
    const btn = Number(b);
    if (handler) {
      // 64/65 = wheel; bit 32 = motion while held (mode 1002 drag events).
      if (fin === "M") {
        if (btn === 64) handler({ x: Number(x), y: Number(y), kind: "wheel-up" });
        else if (btn === 65) handler({ x: Number(x), y: Number(y), kind: "wheel-down" });
        else if (btn & 32) {
          if ((btn & 3) === 0) handler({ x: Number(x), y: Number(y), kind: "drag" });
        } else if ((btn & 3) === 0) handler({ x: Number(x), y: Number(y), kind: "down" });
        else if ((btn & 3) === 2) handler({ x: Number(x), y: Number(y), kind: "right-click" });
      } else if ((btn & 3) === 0 && !(btn & 32)) {
        handler({ x: Number(x), y: Number(y), kind: "up" });
      }
    }
    return "";
  });
}

/** ink-compatible stdin: filters mouse + paste sequences out of the real stdin. */
export function filteredStdin(): typeof process.stdin {
  const out = new PassThrough();
  let carry = "";
  let inPaste = false;
  let pasteBuf = "";
  process.stdin.on("data", (chunk: Buffer | string) => {
    let s = carry + chunk.toString("latin1");
    carry = "";
    let forwarded = "";
    while (s.length > 0) {
      if (inPaste) {
        const end = s.indexOf(PASTE_END);
        if (end < 0) {
          // Keep a possible partial end-marker for the next chunk.
          const tail = PARTIAL_TAIL.exec(s)?.[0] ?? "";
          pasteBuf += s.slice(0, s.length - tail.length);
          carry = tail;
          s = "";
          break;
        }
        pasteBuf += s.slice(0, end);
        s = s.slice(end + PASTE_END.length);
        inPaste = false;
        pasteHandler?.(pasteBuf.replace(/\r\n?/g, "\n"));
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
    if (forwarded) out.write(Buffer.from(forwarded, "latin1"));
  });
  // ink probes these on its stdin.
  const fake = out as unknown as typeof process.stdin;
  (fake as unknown as { isTTY: boolean }).isTTY = process.stdin.isTTY;
  (fake as unknown as { setRawMode: (v: boolean) => void }).setRawMode = (v: boolean) => {
    if (process.stdin.isTTY) process.stdin.setRawMode(v);
  };
  (fake as unknown as { ref: () => void }).ref = () => process.stdin.ref?.();
  (fake as unknown as { unref: () => void }).unref = () => process.stdin.unref?.();
  return fake;
}

const enc = new TextEncoder();
/** Alt screen + SGR mouse tracking + bracketed paste on. 1002 (button-event)
 * adds drag motion for text selection; 1000 stays as a fallback for terminals
 * without it. 1004 reports focus in/out (gates desktop notifications); 22;0t
 * pushes the terminal's current title so leaveTui can restore it. */
export function enterTui() {
  Deno.stdout.writeSync(
    enc.encode("\x1b[22;0t\x1b[?1049h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[?1004h"),
  );
}
/** Restore the normal buffer, mouse + paste + focus modes off, cursor visible,
 * the pushed title popped back, progress/tab-color cleared. */
export function leaveTui() {
  try {
    termCleanup();
    Deno.stdout.writeSync(
      enc.encode(
        "\x1b[?1004l\x1b[?2004l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?1049l\x1b[?25h\x1b[23;0t",
      ),
    );
  } catch {
    // stdout already gone — nothing to restore onto.
  }
}
