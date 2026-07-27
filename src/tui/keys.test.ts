/**
 * Tests for the keymap.
 *
 * The point of `keys.ts` is that the keyboard is DATA, so this file asserts the
 * data: that no binding is shadowed by another, that the help overlay says exactly
 * what the table binds and nothing else, that the "not bound" section is TRUE, and
 * that a chord whose meaning depends on context resolves both ways. None of it
 * mounts a renderer or opens a terminal — that is the task's acceptance criterion
 * and it is also what makes these assertions worth anything: a keymap you can only
 * check by pressing keys is a keymap nobody checks.
 *
 * `node:assert/strict` — jsr.io is unreachable in this environment.
 */
import assert from "node:assert/strict";
import {
  type Binding,
  BINDINGS,
  chordLabel,
  chordOf,
  chunkInput,
  deadBindings,
  editLine,
  EMPTY_LINE,
  helpLines,
  helpSections,
  insertText,
  isTextInput,
  type KeyContext,
  type LineState,
  lookup,
  PANEL_TABS,
  PANEL_TOGGLE,
  resolve,
  stripCtl,
  tabForChord,
  tabForCommand,
  TABS,
  type UiMode,
  UNAVAILABLE,
} from "./keys.ts";

const ctx = (over: Partial<KeyContext> = {}): KeyContext => ({
  mode: "chat",
  emptyDraft: true,
  multiline: false,
  busy: false,
  doubleEsc: false,
  quitArmed: false,
  railLive: false,
  completing: false,
  ...over,
});

// ---------------------------------------------------------------------------
// Chords
// ---------------------------------------------------------------------------

Deno.test("chordOf canonicalizes modifiers, named keys and plain characters", () => {
  assert.equal(chordOf("p", { ctrl: true }), "ctrl+p");
  assert.equal(chordOf("", { escape: true }), "esc");
  assert.equal(chordOf("", { upArrow: true }), "up");
  assert.equal(chordOf("", { pageUp: true }), "pageup");
  assert.equal(chordOf("\r", { return: true }), "enter");
  assert.equal(chordOf("\r", { return: true, meta: true }), "meta+enter");
  assert.equal(chordOf("\r", { return: true, shift: true }), "shift+enter");
  assert.equal(chordOf("", { leftArrow: true, super: true }), "super+left");
  assert.equal(chordOf("", { backspace: true, meta: true }), "meta+backspace");
  assert.equal(chordOf("?"), "?");
  assert.equal(chordOf(" "), "space");
});

Deno.test("^j is one chord however the terminal spells it", () => {
  // Kitty-protocol terminals report the modifier…
  assert.equal(chordOf("j", { ctrl: true }), "ctrl+j");
  // …everyone else sends a bare newline with no return flag. Treating that as
  // Return is the bug that used to send half-written messages.
  assert.equal(chordOf("\n", {}), "ctrl+j");
  // A real Return is \r WITH the flag, and stays distinct.
  assert.equal(chordOf("\r", { return: true }), "enter");
});

Deno.test("a coalesced chunk is not a chord — it is text", () => {
  assert.equal(chordOf("hello world"), "");
  assert.equal(chordOf(""), "");
  assert.equal(lookup(ctx(), chordOf("hello")), null);
});

Deno.test("chordLabel prints what the overlay shows", () => {
  assert.equal(chordLabel("ctrl+p"), "^p");
  assert.equal(chordLabel("meta+enter"), "⌥⏎");
  assert.equal(chordLabel("super+backspace"), "⌘⌫");
  assert.equal(chordLabel("pageup"), "pgup");
  assert.equal(chordLabel("?"), "?");
});

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

Deno.test("no binding is dead — nothing is shadowed by an earlier row", () => {
  assert.deepEqual(deadBindings(), []);
});

Deno.test("deadBindings catches the two ways a row goes dead", () => {
  const identical: Binding[] = [
    { mode: "chat", chord: "ctrl+g", command: "cancel" },
    { mode: "chat", chord: "ctrl+g", command: "quit" },
  ];
  assert.deepEqual(deadBindings(identical), ["chat ctrl+g"]);

  // An unguarded row ahead of a guarded one: the guard can never be reached.
  const shadowed: Binding[] = [
    { mode: "chat", chord: "ctrl+g", command: "cancel" },
    { mode: "chat", chord: "ctrl+g", command: "quit", when: ["busy"] },
  ];
  assert.equal(deadBindings(shadowed).length, 1);

  // Complementary guards are the design, not a bug.
  const complementary: Binding[] = [
    { mode: "chat", chord: "ctrl+g", command: "cancel", when: ["emptyDraft"] },
    { mode: "chat", chord: "ctrl+g", command: "quit" },
  ];
  assert.deepEqual(deadBindings(complementary), []);
});

Deno.test("every binding is reachable — some real context resolves to it", () => {
  // The complement of `deadBindings`, checked the expensive way: walk the whole
  // guard space and record which ROW `lookup` actually picks. A binding no context
  // reaches is a key the user can never press, however plausible the table looks.
  const modes: UiMode[] = ["chat", "rail", "ask", "panel", "help"];
  const flags = [
    "emptyDraft",
    "multiline",
    "busy",
    "doubleEsc",
    "quitArmed",
    "railLive",
    "completing",
  ] as const;
  const reached = new Set<number>();
  const firstMatch = (c: KeyContext, chord: string): number =>
    BINDINGS.findIndex((b) =>
      (b.mode === c.mode || b.mode === "*") && b.chord === chord &&
      (b.when ?? []).every((g) => c[g]) && (b.not ?? []).every((g) => !c[g])
    );
  const chords = [...new Set(BINDINGS.map((b) => b.chord))];
  for (const mode of modes) {
    for (let mask = 0; mask < 1 << flags.length; mask++) {
      const c = ctx({ mode });
      flags.forEach((f, i) => (c[f] = (mask & (1 << i)) !== 0));
      // `multiline` implies a non-empty draft; that context cannot occur.
      if (c.multiline && c.emptyDraft) continue;
      for (const chord of chords) {
        const at = firstMatch(c, chord);
        if (at < 0) continue;
        reached.add(at);
        // The table and the resolver must agree about which row won.
        assert.equal(lookup(c, chord), BINDINGS[at].command);
      }
    }
  }
  const unreachable = BINDINGS
    .map((b, i) => (reached.has(i) ? null : `${b.mode} ${b.chord} → ${b.command}`))
    .filter(Boolean);
  assert.deepEqual(unreachable, []);
});

Deno.test("the same chord means two things, and the guard decides which", () => {
  assert.equal(lookup(ctx({ emptyDraft: true }), "ctrl+f"), "tab.tree");
  assert.equal(lookup(ctx({ emptyDraft: false }), "ctrl+f"), "cursor.right");
  assert.equal(lookup(ctx({ emptyDraft: true }), "ctrl+e"), "fold.all");
  assert.equal(lookup(ctx({ emptyDraft: false }), "ctrl+e"), "cursor.end");
  assert.equal(lookup(ctx({ multiline: true, emptyDraft: false }), "up"), "cursor.up");
  assert.equal(lookup(ctx({ multiline: false }), "up"), "history.prev");
});

Deno.test("↓ enters the rail only when a subagent is actually working", () => {
  assert.equal(lookup(ctx({ emptyDraft: true, railLive: true }), "down"), "rail.enter");
  assert.equal(lookup(ctx({ emptyDraft: true, railLive: false }), "down"), "history.next");
  assert.equal(lookup(ctx({ emptyDraft: false, railLive: true }), "down"), "history.next");
});

Deno.test("a single ^c arms, a second quits — in every mode", () => {
  for (const mode of ["chat", "panel", "help", "rail", "ask"] as UiMode[]) {
    assert.equal(lookup(ctx({ mode, quitArmed: false }), "ctrl+c"), "quit.arm", mode);
    assert.equal(lookup(ctx({ mode, quitArmed: true }), "ctrl+c"), "quit", mode);
  }
});

Deno.test("esc alone cancels; esc esc clears the draft", () => {
  assert.equal(lookup(ctx({ doubleEsc: false }), "esc"), "cancel");
  assert.equal(lookup(ctx({ doubleEsc: true }), "esc"), "draft.clear");
});

Deno.test("the panel binds its own keys and nothing from chat", () => {
  assert.equal(lookup(ctx({ mode: "panel" }), "j"), "move.down");
  assert.equal(lookup(ctx({ mode: "panel" }), "enter"), "panel.confirm");
  assert.equal(lookup(ctx({ mode: "panel" }), "esc"), "panel.close");
  assert.equal(lookup(ctx({ mode: "panel" }), "tab"), "panel.next");
  assert.equal(lookup(ctx({ mode: "panel" }), "shift+tab"), "panel.prev");
  // Composer chords do NOT leak in — ^a/^u are line editing and chat's alone.
  assert.equal(lookup(ctx({ mode: "panel" }), "ctrl+a"), null);
  assert.equal(lookup(ctx({ mode: "panel" }), "ctrl+u"), null);
  // …but the direct jumps do, because a jump that only works from chat is not one.
  assert.equal(lookup(ctx({ mode: "panel" }), "ctrl+k"), "tab.skills");
  assert.equal(lookup(ctx({ mode: "panel" }), "ctrl+t"), "panel.toggle");
  assert.equal(lookup(ctx({ mode: "panel" }), "p"), "wf.pause");
  assert.equal(lookup(ctx({ mode: "panel" }), "x"), "wf.stop");
  assert.equal(lookup(ctx({ mode: "panel" }), "r"), "wf.rerun");
  assert.equal(lookup(ctx({ mode: "ask" }), "3"), "ask.pick");
  assert.equal(lookup(ctx({ mode: "ask" }), "esc"), "ask.decline");
  // A held question owns the keyboard: no tab chord steals it (spec §6).
  assert.equal(lookup(ctx({ mode: "ask" }), "ctrl+s"), null);
});

Deno.test("every tab has exactly one chord, and ^t names no tab", () => {
  const chords = [PANEL_TOGGLE, ...TABS.map((t) => t.chord)];
  assert.equal(new Set(chords).size, chords.length, chords.join(","));
  assert.equal(new Set(PANEL_TABS).size, TABS.length);
  assert.equal(tabForChord(PANEL_TOGGLE), null);
  assert.equal(tabForChord("ctrl+zzz"), null);
  // Every chord resolves to its own tab from chat, and the command names it back.
  for (const tab of TABS) {
    const command = lookup(ctx({ mode: "chat", emptyDraft: true }), tab.chord);
    assert.equal(command, `tab.${tab.id}`, tab.chord);
    assert.equal(tabForCommand(command!), tab.id);
    assert.equal(tabForChord(tab.chord), tab.id);
  }
  assert.equal(tabForCommand("panel.toggle"), null);
  assert.equal(tabForCommand("send"), null);
});

Deno.test("resolve is lookup straight off an ink keypress", () => {
  assert.equal(resolve(ctx(), "", { escape: true, ...{} }), "cancel");
  assert.equal(resolve(ctx({ mode: "panel" }), "", { downArrow: true }), "move.down");
  assert.equal(resolve(ctx(), "x"), null);
});

// ---------------------------------------------------------------------------
// The overlay
// ---------------------------------------------------------------------------

Deno.test("the overlay documents the table and only the table", () => {
  const documented = BINDINGS.filter((b) => b.section && b.desc);
  const sections = helpSections();
  const rows = sections
    .filter((s) => !s.limits && !s.unavailable)
    .flatMap((s) => s.keys);
  assert.equal(rows.length, documented.length);
  // Every documented row carries a description; an empty one would render a key
  // with nothing beside it, which is how the old overlay rotted.
  for (const [chord, desc] of rows) {
    assert.ok(chord.length > 0);
    assert.ok(desc.length > 0);
  }
});

Deno.test('the "not bound" section is true — none of those chords is bound', () => {
  // ^y used to be listed here; it is the theme tab's chord now, so it is gone from
  // the section. That is the section's whole job: it must stay TRUE.
  const unbound = ["ctrl+r", "ctrl+z", "meta+d"];
  for (const chord of unbound) {
    assert.equal(
      BINDINGS.some((b) => b.chord === chord),
      false,
      `${chord} is listed as not bound but the table binds it`,
    );
  }
  assert.equal(UNAVAILABLE.keys.length, unbound.length);
});

Deno.test("every section header survives flattening, and carries its rows", () => {
  // The regression this pins: the overlay used to nest a Box per section under a
  // parent pinned to the terminal height, yoga absorbed the overflow, and every
  // header plus one row per section vanished from the screen. A flat list cannot
  // lose a row to layout, so the fix is asserted where the fix lives.
  const sections = helpSections();
  const flat = helpLines(sections);
  const headers = flat.filter((l) => l.kind === "header").map((l) => l.desc);
  assert.deepEqual(headers, sections.map((s) => s.section));
  assert.equal(
    flat.filter((l) => l.kind === "row").length,
    sections.reduce((n, s) => n + s.keys.length, 0),
  );
  // Each header is immediately followed by its own first row, never by another
  // header — the shape that made the squashed render unreadable.
  for (let i = 0; i < flat.length; i++) {
    if (flat[i].kind !== "header") continue;
    assert.equal(flat[i + 1]?.kind, "row", `section "${flat[i].desc}" rendered no rows`);
  }
});

Deno.test("the overlay is taller than a terminal, which is why it scrolls", () => {
  // If this ever stops being true the windowing in `Help` is dead code and should
  // go. While it IS true, a renderer that does not window is a broken renderer.
  assert.ok(helpLines().length > 24);
});

Deno.test("the prose sections carry no key column of their own", () => {
  const limits = helpSections().find((s) => s.limits);
  assert.ok(limits);
  for (const [chord] of limits.keys) assert.equal(chord, "");
});

// ---------------------------------------------------------------------------
// Line editing
// ---------------------------------------------------------------------------

const line = (text: string, cursor = text.length): LineState => ({ text, cursor });

Deno.test("cursor motion clamps at both ends and returns the same object on a no-op", () => {
  const start = line("abc", 0);
  assert.equal(editLine(start, "cursor.left"), start);
  const end = line("abc", 3);
  assert.equal(editLine(end, "cursor.right"), end);
  assert.deepEqual(editLine(line("abc", 1), "cursor.right"), { text: "abc", cursor: 2 });
});

Deno.test("home/end are the LOGICAL line's, not the whole draft's", () => {
  const s = line("first\nsecond", 8); // inside "second"
  assert.deepEqual(editLine(s, "cursor.home"), { text: "first\nsecond", cursor: 6 });
  assert.deepEqual(editLine(s, "cursor.end"), { text: "first\nsecond", cursor: 12 });
});

Deno.test("↑/↓ hold the column against the line they land on, and stop at the ends", () => {
  const text = "hello\nhi\nworld";
  const up = editLine(line(text, 13), "cursor.up"); // column 4 of "world"
  assert.deepEqual(up, { text, cursor: 8 }); // "hi" is shorter: land on its end
  // No goal-column memory, deliberately: the column is read from where the cursor
  // IS, so a short line in the middle is not a trap the cursor has to remember its
  // way out of. Anything else needs state that survives every other edit.
  assert.deepEqual(editLine(up, "cursor.up"), { text, cursor: 2 });
  assert.deepEqual(editLine(up, "cursor.down"), { text, cursor: 11 });
  const only = line("one", 1);
  assert.equal(editLine(only, "cursor.up"), only);
  assert.equal(editLine(only, "cursor.down"), only);
});

Deno.test("word motion and word delete agree on where a word starts", () => {
  const s = line("alpha beta gamma", 16);
  const back = editLine(s, "cursor.wordLeft");
  assert.equal(back.cursor, 11);
  assert.deepEqual(editLine(s, "delete.wordBack"), { text: "alpha beta ", cursor: 11 });
});

Deno.test("the kill keys cut to the ends of the current line only", () => {
  const s = line("first\nsecond half", 12); // inside "second half"
  assert.deepEqual(editLine(s, "delete.toEnd"), { text: "first\nsecond", cursor: 12 });
  assert.deepEqual(editLine(s, "delete.toStart"), { text: "first\n half", cursor: 6 });
  assert.deepEqual(editLine(s, "delete.line"), EMPTY_LINE);
});

Deno.test("backspace and delete-forward move the cursor the way each should", () => {
  assert.deepEqual(editLine(line("abc", 2), "delete.back"), { text: "ac", cursor: 1 });
  assert.deepEqual(editLine(line("abc", 1), "delete.forward"), { text: "ac", cursor: 1 });
  const atStart = line("abc", 0);
  assert.equal(editLine(atStart, "delete.back"), atStart);
  const atEnd = line("abc", 3);
  assert.equal(editLine(atEnd, "delete.forward"), atEnd);
});

Deno.test("newline inserts at the cursor rather than sending", () => {
  assert.deepEqual(editLine(line("ab", 1), "newline"), { text: "a\nb", cursor: 2 });
  assert.deepEqual(insertText(line("ab", 1), "XY"), { text: "aXYb", cursor: 3 });
});

// ---------------------------------------------------------------------------
// Raw input
// ---------------------------------------------------------------------------

Deno.test("only a trailing \\r sends a coalesced chunk", () => {
  assert.deepEqual(chunkInput("hello\r"), { body: "hello", send: true });
  // ^j after fast typing arrives in the same read and must NOT send.
  assert.deepEqual(chunkInput("hello\n"), { body: "hello\n", send: false });
  assert.deepEqual(chunkInput("two\r\nlines\r"), { body: "two\nlines", send: true });
});

Deno.test("stripCtl removes invisible bytes but keeps newlines and tabs out of harm", () => {
  assert.equal(stripCtl("a\x00b\x07c"), "abc");
  assert.equal(stripCtl("keep\nthe newline"), "keep\nthe newline");
  // This line used to assert "[31mred" — it pinned the bug. Removing the escape
  // byte and keeping the rest of the sequence is what typed a terminal's own key
  // encoding into the user's draft; see the escape-sequence test below.
  assert.equal(stripCtl("\x1b[31mred"), "red");
});

Deno.test("isTextInput tells typing from a chord", () => {
  assert.equal(isTextInput("a"), true);
  assert.equal(isTextInput("a", { ctrl: true }), false);
  assert.equal(isTextInput("", { upArrow: true }), false);
  assert.equal(isTextInput("\r", { return: true }), false);
  assert.equal(isTextInput(""), false);
});

Deno.test("an escape sequence is dropped whole, never typed into the draft", () => {
  // Alt+Enter under the kitty / modifyOtherKeys encoding. Stripping only the ESC
  // byte left "[27;3;13~" behind, which the composer then inserted as text —
  // observed live as `› and then say done[27;3;13~`.
  assert.equal(stripCtl("\x1b[27;3;13~"), "");
  assert.equal(stripCtl("hi\x1b[27;3;13~there"), "hithere");
  // The other shapes a terminal emits, none of which are things a user typed.
  assert.equal(stripCtl("\x1b[1;5D"), ""); // CSI, ctrl+left
  assert.equal(stripCtl("\x1bOP"), ""); // SS3, F1
  assert.equal(stripCtl("\x1b[200~pasted\x1b[201~"), "pasted"); // bracketed paste
  assert.equal(stripCtl("\x1b[31mred\x1b[39m"), "red"); // SGR from a paste
  // Ordinary text, including punctuation that merely LOOKS like a sequence, is kept.
  assert.equal(stripCtl("a[27;3;13~b"), "a[27;3;13~b");
  assert.equal(stripCtl("emoji 🎉 and 日本語"), "emoji 🎉 and 日本語");
  // Newlines and tabs are content, not control noise.
  assert.equal(stripCtl("one\ntwo\tthree"), "one\ntwo\tthree");
});
