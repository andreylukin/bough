/**
 * Tests for the stdin filter.
 *
 * One rule, checked from several directions: **ink must only ever receive
 * keystrokes.** Everything else the terminal sends — SGR mouse reports, bracketed
 * pastes, focus events, the OSC 11 background reply, the Home/End and Cmd+←/→
 * sequences ink mishandles — is consumed here and dispatched, and what is left
 * over is forwarded byte for byte.
 *
 * The second rule is subtler and has a bug attached to it: **a bare ESC must not
 * be held back**. The partial-tail pattern only holds a fragment that already
 * carries the distinguishing third byte, because holding `\x1b` waiting for a
 * possible `[` would swallow the Escape KEY until the next keypress — and Escape
 * is how you leave every panel in this TUI.
 *
 * `createInputFilter` is a pure state machine over strings, so none of this needs
 * a process, a stream or a terminal. `node:assert/strict` — jsr.io is unreachable.
 */
import assert from "node:assert/strict";
import { createInputFilter, type MouseEvent, type NavKey } from "./mouse.ts";

function harness() {
  const mouse: MouseEvent[] = [];
  const pastes: string[] = [];
  const navKeys: NavKey[] = [];
  const focus: boolean[] = [];
  const bg: string[] = [];
  const filter = createInputFilter({
    mouse: (e) => mouse.push(e),
    paste: (t) => pastes.push(t),
    navKey: (k) => navKeys.push(k),
    focus: (f) => focus.push(f),
    bgReport: (s) => bg.push(s),
  });
  return { filter, mouse, pastes, navKeys, focus, bg };
}

Deno.test("ordinary keystrokes pass through untouched", () => {
  const h = harness();
  assert.equal(h.filter.feed("hello"), "hello");
  assert.equal(h.filter.feed("\r"), "\r");
  // A bare Escape is a KEY, not the start of something. It must not be held.
  assert.equal(h.filter.feed("\x1b"), "\x1b");
  assert.equal(h.mouse.length, 0);
});

Deno.test("SGR mouse reports are consumed, never forwarded", () => {
  const h = harness();
  assert.equal(h.filter.feed("a\x1b[<0;10;5Mb"), "ab");
  assert.deepEqual(h.mouse, [{ x: 10, y: 5, kind: "down" }]);
});

Deno.test("the left button reports its whole press/drag/release cycle", () => {
  const h = harness();
  h.filter.feed("\x1b[<0;3;4M"); // press
  h.filter.feed("\x1b[<32;6;4M"); // motion while held (mode 1002)
  h.filter.feed("\x1b[<0;9;7m"); // release
  assert.deepEqual(h.mouse.map((e) => e.kind), ["down", "drag", "up"]);
  assert.deepEqual(h.mouse.at(-1), { x: 9, y: 7, kind: "up" });
});

Deno.test("wheel and right-click are their own kinds", () => {
  const h = harness();
  h.filter.feed("\x1b[<64;1;1M\x1b[<65;1;1M\x1b[<2;4;4M");
  assert.deepEqual(h.mouse.map((e) => e.kind), ["wheel-up", "wheel-down", "right-click"]);
});

Deno.test("a bracketed paste arrives whole, with newlines normalized", () => {
  const h = harness();
  assert.equal(h.filter.feed("\x1b[200~one\r\ntwo\rthree\x1b[201~"), "");
  assert.deepEqual(h.pastes, ["one\ntwo\nthree"]);
});

Deno.test("a paste split across three reads is reassembled, not leaked", () => {
  const h = harness();
  // The start marker itself is split, then the body, then the end marker.
  assert.equal(h.filter.feed("x\x1b[20"), "x");
  assert.equal(h.filter.feed("0~hello "), "");
  assert.equal(h.filter.feed("world\x1b[201"), "");
  assert.equal(h.filter.feed("~y"), "y");
  assert.deepEqual(h.pastes, ["hello world"]);
});

Deno.test("a mouse report split across reads is held and then consumed", () => {
  const h = harness();
  assert.equal(h.filter.feed("a\x1b[<0;10"), "a");
  assert.equal(h.mouse.length, 0);
  assert.equal(h.filter.feed(";5Mb"), "b");
  assert.deepEqual(h.mouse, [{ x: 10, y: 5, kind: "down" }]);
});

Deno.test("terminal REPLIES are consumed — they are not keystrokes", () => {
  const h = harness();
  assert.equal(h.filter.feed("\x1b[Ihi\x1b[O"), "hi");
  assert.deepEqual(h.focus, [true, false]);
  assert.equal(h.filter.feed("\x1b]11;rgb:1e1e/1e1e/2e2e\x07"), "");
  assert.deepEqual(h.bg, ["rgb:1e1e/1e1e/2e2e"]);
});

Deno.test("Home/End and Cmd+←/→ are intercepted rather than left to ink", () => {
  const h = harness();
  // Ink drops the Home/End forms; all three spellings must land the same way.
  assert.equal(h.filter.feed("\x1b[H\x1bOH\x1b[1~"), "");
  assert.deepEqual(h.navKeys, ["home", "home", "home"]);
  assert.equal(h.filter.feed("\x1b[F\x1bOF\x1b[4~"), "");
  assert.deepEqual(h.navKeys.slice(3), ["end", "end", "end"]);
});

Deno.test("Cmd+←/→ is caught before ink can misparse it as meta+arrow", () => {
  const h = harness();
  assert.equal(h.filter.feed("\x1b[1;9D\x1b[1;9C"), "");
  assert.deepEqual(h.navKeys, ["cmdHome", "cmdEnd"]);
});

Deno.test("a mouse drag during a paste does not corrupt either", () => {
  const h = harness();
  // Inside a paste everything is literal text, including what looks like a report.
  assert.equal(h.filter.feed("\x1b[200~a\x1b[<0;1;1Mb\x1b[201~"), "");
  assert.deepEqual(h.pastes, ["a\x1b[<0;1;1Mb"]);
  assert.equal(h.mouse.length, 0);
});

Deno.test("a filter with no sinks still strips what it recognises", () => {
  const filter = createInputFilter();
  assert.equal(filter.feed("a\x1b[<0;1;1Mb\x1b[200~junk\x1b[201~c"), "abc");
});
