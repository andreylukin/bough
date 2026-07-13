// Mouse support. Ink has no mouse handling, so we sit between the real stdin and
// ink: a PassThrough stream receives everything the terminal sends, SGR mouse
// sequences (\x1b[<b;x;yM / m) are consumed here and dispatched to a handler,
// and every other byte is forwarded to the stream ink reads from.
import { PassThrough } from "node:stream";
import process from "node:process";

export interface MouseEvent {
  /** 1-based terminal column/row of the event. */
  x: number;
  y: number;
  kind: "click" | "wheel-up" | "wheel-down";
}

type Handler = (ev: MouseEvent) => void;
let handler: Handler | null = null;
export function onMouse(h: Handler | null) {
  handler = h;
}

// deno-lint-ignore no-control-regex -- ESC is the point: SGR mouse sequences
const SGR = /\x1b\[<(\d+);(\d+);(\d+)([Mm])/g;

/** ink-compatible stdin: filters mouse sequences out of the real stdin. */
export function filteredStdin(): typeof process.stdin {
  const out = new PassThrough();
  let carry = "";
  process.stdin.on("data", (chunk: Buffer | string) => {
    let s = carry + chunk.toString("latin1");
    carry = "";
    // Hold back a trailing partial mouse sequence for the next chunk.
    const tailStart = s.lastIndexOf("\x1b[<");
    if (tailStart >= 0 && !/[Mm]/.test(s.slice(tailStart))) {
      carry = s.slice(tailStart);
      s = s.slice(0, tailStart);
    }
    const forwarded = s.replace(SGR, (_all, b, x, y, fin) => {
      const btn = Number(b);
      if (fin === "M" && handler) {
        // Press events only (releases are `m`); 64/65 = wheel.
        if (btn === 64) handler({ x: Number(x), y: Number(y), kind: "wheel-up" });
        else if (btn === 65) handler({ x: Number(x), y: Number(y), kind: "wheel-down" });
        else if ((btn & 3) === 0) handler({ x: Number(x), y: Number(y), kind: "click" });
      }
      return "";
    });
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
/** Alt screen + SGR mouse tracking on. */
export function enterTui() {
  Deno.stdout.writeSync(enc.encode("\x1b[?1049h\x1b[?1000h\x1b[?1006h"));
}
/** Restore the normal buffer, mouse off, cursor visible. */
export function leaveTui() {
  try {
    Deno.stdout.writeSync(enc.encode("\x1b[?1006l\x1b[?1000l\x1b[?1049l\x1b[?25h"));
  } catch {
    // stdout already gone — nothing to restore onto.
  }
}
