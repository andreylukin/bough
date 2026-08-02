/**
 * Tests for terminal capability detection and the sequences it gates.
 *
 * The gating is not cosmetic. OSC 9;4 is taskbar progress in Ghostty and a
 * DESKTOP NOTIFICATION in kitty, so an ungated progress keep-alive pops a banner
 * every five seconds; the iTerm2 tab tint aimed at an unknown outer terminal
 * prints garbage. Each of those is one boolean in `termCaps`, and each boolean is
 * asserted here against a fixture environment — no terminal, no `process.env`, no
 * stdout (`createTerm` takes its writer).
 *
 * `node:assert/strict` — jsr.io is unreachable in this environment.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import {
  boughTitle,
  classifyBg,
  createTerm,
  kittyKeyboardMode,
  parseBgSpec,
  sanitize,
  type TermCaps,
  termCaps,
  tmuxWrap,
} from "./term.ts";

/** A term wired to a string log, with timers that never fire on their own. */
function harness(caps: TermCaps) {
  const out: string[] = [];
  const timers = new Map<number, () => void>();
  let next = 1;
  const term = createTerm({
    caps,
    write: (seq) => out.push(seq),
    setInterval: (fn) => {
      timers.set(next, fn);
      return next++;
    },
    clearInterval: (h) => void timers.delete(h),
    setTimeout: (fn) => {
      timers.set(next, fn);
      return next++;
    },
    clearTimeout: (h) => void timers.delete(h),
  });
  return { term, out, timers, text: () => out.join("") };
}

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

test("kitty support is detected by program, by TERM, and by kitty's own env var", () => {
  assert.equal(termCaps({ TERM_PROGRAM: "ghostty" }).kitty, true);
  assert.equal(termCaps({ TERM_PROGRAM: "WezTerm" }).kitty, true);
  assert.equal(termCaps({ TERM: "xterm-kitty" }).kitty, true);
  assert.equal(termCaps({ KITTY_WINDOW_ID: "3" }).kitty, true);
  assert.equal(termCaps({ TERM_PROGRAM: "Apple_Terminal" }).kitty, false);
  assert.equal(termCaps({}).kitty, false);
});

test("under tmux the outer terminal is unknowable, so super is not trusted", () => {
  // Not "tmux cannot pass it" — "we cannot tell from here whether it will", which
  // is why `mouse.ts` intercepts CSI 1;9 C/D regardless.
  assert.equal(termCaps({ TERM_PROGRAM: "ghostty", TMUX: "/tmp/x,1,0" }).kitty, false);
  assert.equal(termCaps({ TERM_PROGRAM: "ghostty", TMUX: "/tmp/x,1,0" }).tmux, true);
});

test("the keyboard protocol is pushed unconditionally, never probed", () => {
  // "auto" costs a round trip that tmux eats — the one setup that needs it most.
  assert.equal(kittyKeyboardMode(), "enabled");
});

test("OSC 9;4 progress is only sent to terminals that render it", () => {
  assert.equal(termCaps({ TERM_PROGRAM: "ghostty" }).progress, true);
  assert.equal(termCaps({ TERM_PROGRAM: "iTerm.app" }).progress, true);
  // kitty parses OSC 9 as a notification: progress here is banner spam.
  assert.equal(termCaps({ TERM: "xterm-kitty" }).progress, false);
  assert.equal(termCaps({}).progress, false);
});

test("the tab tint is iTerm2's alone, and not under tmux", () => {
  assert.equal(termCaps({ TERM_PROGRAM: "iTerm.app" }).tabColor, true);
  assert.equal(termCaps({ TERM_PROGRAM: "iTerm.app", TMUX: "x" }).tabColor, false);
  assert.equal(termCaps({ TERM_PROGRAM: "ghostty" }).tabColor, false);
});

test("Terminal.app gets a bell, because it accepts OSC 9 and shows nothing", () => {
  assert.equal(termCaps({ TERM_PROGRAM: "Apple_Terminal" }).notify, "bell");
  assert.equal(termCaps({ TERM_PROGRAM: "ghostty" }).notify, "osc9");
  assert.equal(termCaps({ ZELLIJ: "0" }).zellij, true);
  assert.equal(termCaps({}).zellij, false);
});

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

test("sanitize strips control bytes from titles and notification bodies", () => {
  assert.equal(sanitize("ok\x07\x1b]0;evil\x07"), "ok  ]0;evil ");
  assert.equal(sanitize("plain title"), "plain title");
});

test("bough title marks active and completed work", () => {
  assert.equal(boughTitle(null, null), "bough");
  assert.equal(boughTitle("Fix parser", "running"), "bough · Fix parser · running");
  assert.equal(boughTitle("Fix\x07 parser", "complete"), "bough · Fix  parser · complete");
});

test("tmuxWrap doubles every ESC and wraps in the passthrough DCS", () => {
  assert.equal(tmuxWrap("\x1b]9;hi\x07", true), "\x1bPtmux;\x1b\x1b]9;hi\x07\x1b\\");
  assert.equal(tmuxWrap("\x1b]9;hi\x07", false), "\x1b]9;hi\x07");
});

test("parseBgSpec scales 16/8/4-bit channels to #rrggbb", () => {
  assert.equal(parseBgSpec("rgb:1e1e/1e1e/2e2e"), "#1e1e2e");
  assert.equal(parseBgSpec("rgb:fa/fa/fa"), "#fafafa");
  assert.equal(parseBgSpec("rgb:f/0/f"), "#ff00ff");
  assert.equal(parseBgSpec("not-a-color"), null);
});

test("classifyBg splits dark from light on Rec. 709 luma", () => {
  assert.deepEqual(classifyBg("#1e1e2e"), { hex: "#1e1e2e", scheme: "dark" });
  assert.deepEqual(classifyBg("#fafafa"), { hex: "#fafafa", scheme: "light" });
});

// ---------------------------------------------------------------------------
// Effects
// ---------------------------------------------------------------------------

test("a malformed background report never clobbers a good one", () => {
  const { term } = harness(termCaps({}));
  assert.equal(term.termBackground(), null); // null until the terminal answers
  term.reportTermBg("rgb:1e1e/1e1e/2e2e");
  assert.deepEqual(term.termBackground(), { hex: "#1e1e2e", scheme: "dark" });
  term.reportTermBg("garbage");
  assert.equal(term.termBackground()?.hex, "#1e1e2e");
});

test("a notification fires only while the terminal is unfocused", () => {
  const { term, text, out } = harness(termCaps({ TERM_PROGRAM: "ghostty" }));
  term.notifyDesktop("done"); // focused by default: a banner about this screen is noise
  assert.equal(text(), "");
  term.setFocused(false);
  assert.equal(term.isFocused(), false);
  term.notifyDesktop("done");
  assert.equal(out.at(-1), "\x1b]9;done\x07");
});

test("progress is a no-op where it would be read as a notification", () => {
  const kitty = harness(termCaps({ TERM: "xterm-kitty" }));
  kitty.term.progressStart();
  kitty.term.progressEnd();
  assert.equal(kitty.text(), "");
  assert.equal(kitty.timers.size, 0); // and no keep-alive left running

  const ghostty = harness(termCaps({ TERM_PROGRAM: "ghostty" }));
  ghostty.term.progressStart();
  assert.equal(ghostty.out[0], "\x1b]9;4;3\x07");
  assert.equal(ghostty.timers.size, 1); // Ghostty expires stale progress; re-assert
  ghostty.term.progressEnd();
  assert.equal(ghostty.out.at(-1), "\x1b]9;4;0\x07");
  assert.equal(ghostty.timers.size, 0);
});

test("an errored turn flashes the error state, then clears it on a timer", () => {
  const { term, out, timers } = harness(termCaps({ TERM_PROGRAM: "ghostty" }));
  term.progressStart();
  term.progressEnd(true);
  assert.equal(out.at(-1), "\x1b]9;4;2;100\x07");
  assert.equal(timers.size, 1);
  for (const fire of [...timers.values()]) fire();
  assert.equal(out.at(-1), "\x1b]9;4;0\x07");
});

test("the tab tint parses a hex colour and resets on null", () => {
  const { term, out } = harness(termCaps({ TERM_PROGRAM: "iTerm.app" }));
  term.tabColor("#ff8800");
  assert.equal(
    out.at(-1),
    "\x1b]6;1;bg;red;brightness;255\x07\x1b]6;1;bg;green;brightness;136\x07" +
      "\x1b]6;1;bg;blue;brightness;0\x07",
  );
  term.tabColor("not a colour");
  assert.equal(out.length, 1); // unparseable: nothing written at all
  term.tabColor(null);
  assert.equal(out.at(-1), "\x1b]6;1;bg;*;default\x07");
});

test("cleanup clears every sticky state and cancels every timer", () => {
  const { term, out, timers } = harness(termCaps({ TERM_PROGRAM: "iTerm.app" }));
  term.progressStart();
  term.cleanup();
  assert.equal(timers.size, 0);
  assert.ok(out.includes("\x1b]9;4;0\x07"));
  assert.ok(out.includes("\x1b]6;1;bg;*;default\x07"));
});

test("the title names the terminal pane", () => {
  const plain = harness(termCaps({}));
  plain.term.setTitle("bough · fix\x07 the parser");
  assert.deepEqual(plain.out, ["\x1b]0;bough · fix  the parser\x07"]);
});

test("a tmux session also names its current window", () => {
  const titles: string[] = [];
  const terminal = createTerm({
    caps: termCaps({ TMUX: "1" }),
    write: () => {},
    renameTmuxWindow: (title) => titles.push(title),
  });
  terminal.setTitle("bough\x07 running");
  assert.deepEqual(titles, ["bough  running"]);
});

test("a zellij session also names its focused multiplexer tab", () => {
  const titles: string[] = [];
  const terminal = createTerm({ caps: termCaps({ ZELLIJ: "1" }), write: () => {}, renameZellijTab: (title) => titles.push(title) });
  terminal.setTitle("bough\x07 running");
  assert.deepEqual(titles, ["bough  running"]);
});

test("OSC 52 base64-encodes the clipboard payload and caps it", () => {
  const { term, out } = harness(termCaps({}));
  term.osc52Copy("hi");
  assert.equal(out.at(-1), `\x1b]52;c;${btoa("hi")}\x07`);
  term.osc52Copy("x".repeat(200_000));
  // Well under xterm's whole-sequence limit: 72_000 bytes → 96_000 base64 chars.
  assert.equal(out.at(-1)!.length, 96_000 + "\x1b]52;c;\x07".length);
});
