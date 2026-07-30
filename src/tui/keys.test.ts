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
import { test } from "bun:test";
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
  FILTER_TABS,
  type KeyContext,
  type LineState,
  lookup,
  PANEL_TABS,
  type PanelTab,
  PANEL_TOGGLE,
  resolve,
  stripCtl,
  tabForChord,
  tabForCommand,
  TABS,
  type UiMode,
  SLASH_COMMANDS,
  slashCommandFor,
  slashInvocation,
  unknownCommand,
  UNAVAILABLE,
} from "./keys.ts";

const ctx = (over: Partial<KeyContext> = {}): KeyContext => ({
  mode: "chat",
  tab: null,
  emptyDraft: true,
  multiline: false,
  busy: false,
  doubleEsc: false,
  quitArmed: false,
  railLive: false,
  completing: false,
  panelFiltering: false,
  ...over,
});

// ---------------------------------------------------------------------------
// Chords
// ---------------------------------------------------------------------------

test("chordOf canonicalizes modifiers, named keys and plain characters", () => {
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

test("^j is one chord however the terminal spells it", () => {
  // Kitty-protocol terminals report the modifier…
  assert.equal(chordOf("j", { ctrl: true }), "ctrl+j");
  // …everyone else sends a bare newline with no return flag. Treating that as
  // Return is the bug that used to send half-written messages.
  assert.equal(chordOf("\n", {}), "ctrl+j");
  // A real Return is \r WITH the flag, and stays distinct.
  assert.equal(chordOf("\r", { return: true }), "enter");
});

test("a coalesced chunk is not a chord — it is text", () => {
  assert.equal(chordOf("hello world"), "");
  assert.equal(chordOf(""), "");
  assert.equal(lookup(ctx(), chordOf("hello")), null);
});

test("chordLabel prints what the overlay shows", () => {
  assert.equal(chordLabel("ctrl+p"), "^p");
  assert.equal(chordLabel("meta+enter"), "⌥⏎");
  assert.equal(chordLabel("super+backspace"), "⌘⌫");
  assert.equal(chordLabel("pageup"), "pgup");
  assert.equal(chordLabel("?"), "?");
});

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

test("no binding is dead — nothing is shadowed by an earlier row", () => {
  assert.deepEqual(deadBindings(), []);
});

test("deadBindings catches the two ways a row goes dead", () => {
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

test("every binding is reachable — some real context resolves to it", () => {
  // The complement of `deadBindings`, checked the expensive way: walk the whole
  // guard space and record which ROW `lookup` actually picks. A binding no context
  // reaches is a key the user can never press, however plausible the table looks.
  const modes: UiMode[] = ["chat", "rail", "ask", "panel", "help", "job"];
  const flags = [
    "emptyDraft",
    "multiline",
    "busy",
    "doubleEsc",
    "quitArmed",
    "railLive",
    "completing",
    "panelFiltering",
    "inSubagent",
  ] as const;
  // The open tab is part of the context a panel row is matched against, so it is
  // part of the space this walk covers: a row scoped to a tab is only reachable
  // from that tab, and `null` (the panel closed) must reach none of them.
  const tabs: (PanelTab | null)[] = [null, ...PANEL_TABS];
  const reached = new Set<number>();
  const firstMatch = (c: KeyContext, chord: string): number =>
    BINDINGS.findIndex((b) =>
      (b.mode === c.mode || b.mode === "*") && b.chord === chord &&
      (b.when ?? []).every((g) => c[g]) && (b.not ?? []).every((g) => !c[g]) &&
      (!b.tab || (c.tab !== null && c.tab !== undefined && b.tab.includes(c.tab)))
    );
  const chords = [...new Set(BINDINGS.map((b) => b.chord))];
  for (const mode of modes) {
    for (const tab of tabs) {
      for (let mask = 0; mask < 1 << flags.length; mask++) {
        const c = ctx({ mode, tab });
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
  }
  const unreachable = BINDINGS
    .map((b, i) => (reached.has(i) ? null : `${b.mode} ${b.chord} → ${b.command}`))
    .filter(Boolean);
  assert.deepEqual(unreachable, []);
});

test("the same chord means two things, and the guard decides which", () => {
  assert.equal(lookup(ctx({ emptyDraft: true }), "ctrl+f"), "tab.tree");
  assert.equal(lookup(ctx({ emptyDraft: false }), "ctrl+f"), "cursor.right");
  assert.equal(lookup(ctx({ emptyDraft: true }), "ctrl+e"), "fold.all");
  assert.equal(lookup(ctx({ emptyDraft: false }), "ctrl+e"), "cursor.end");
  assert.equal(lookup(ctx({ multiline: true, emptyDraft: false }), "up"), "cursor.up");
  assert.equal(lookup(ctx({ multiline: false }), "up"), "history.prev");
});

test("↓ enters the rail only when a subagent is actually working", () => {
  assert.equal(lookup(ctx({ emptyDraft: true, railLive: true }), "down"), "rail.enter");
  assert.equal(lookup(ctx({ emptyDraft: true, railLive: false }), "down"), "history.next");
  assert.equal(lookup(ctx({ emptyDraft: false, railLive: true }), "down"), "history.next");
});

test("a single ^c arms, a second quits — in every mode", () => {
  for (const mode of ["chat", "panel", "help", "rail", "ask"] as UiMode[]) {
    assert.equal(lookup(ctx({ mode, quitArmed: false }), "ctrl+c"), "quit.arm", mode);
    assert.equal(lookup(ctx({ mode, quitArmed: true }), "ctrl+c"), "quit", mode);
  }
});

test("esc alone cancels; esc esc clears the draft", () => {
  assert.equal(lookup(ctx({ doubleEsc: false }), "esc"), "cancel");
  assert.equal(lookup(ctx({ doubleEsc: true, emptyDraft: false }), "esc"), "draft.clear");
  // With nothing typed there is nothing to clear, so the double-tap must FALL
  // THROUGH rather than swallow the gesture: hammering Escape at a running turn
  // used to resolve to "cleared an empty draft" and leave the turn running.
  assert.equal(lookup(ctx({ doubleEsc: true, emptyDraft: true, busy: true }), "esc"), "turn.interrupt");
});

test("esc unwinds exactly one level: popup, then turn, then notice", () => {
  // The picker's own legend row says `esc closes`, so it must close — even mid-turn.
  assert.equal(
    lookup(ctx({ completing: true, busy: true }), "esc"),
    "complete.dismiss",
  );
  assert.equal(lookup(ctx({ busy: true }), "esc"), "turn.interrupt");
  assert.equal(lookup(ctx({}), "esc"), "cancel");
});

test("⏎ commits the highlighted completion before it sends", () => {
  assert.equal(lookup(ctx({ completing: true }), "enter"), "complete.accept");
  assert.equal(lookup(ctx({ completing: true }), "tab"), "complete.accept");
  assert.equal(lookup(ctx({ completing: false }), "enter"), "send");
});

test("the panel binds its own keys and nothing from chat", () => {
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
  // The steering letters are the WORKFLOWS tab's, and the table says so
  // structurally: from any other tab they are not bound at all.
  assert.equal(lookup(ctx({ mode: "panel", tab: "workflows" }), "p"), "wf.pause");
  assert.equal(lookup(ctx({ mode: "panel", tab: "workflows" }), "x"), "wf.stop");
  assert.equal(lookup(ctx({ mode: "panel", tab: "workflows" }), "r"), "wf.rerun");
  assert.equal(lookup(ctx({ mode: "ask" }), "3"), "ask.pick");
  assert.equal(lookup(ctx({ mode: "ask" }), "esc"), "ask.decline");
  // A HELD QUESTION DOES NOT LOCK YOU OUT OF THE PANEL. It used to: `ask` was left out
  // of the jump rows, so every tab chord was swallowed and the answer had to be given
  // blind — worst of all on the workflow approval card, whose own text says "`x` in the
  // workflows tab (^w) stops a run at any point" about a tab you could not open until
  // you had answered. (The comment here used to cite spec §6 for the exclusivity; §6
  // says only that `ask` parks the program until the human answers, nothing about the
  // keyboard.) The hold survives the detour — it lives in the store, not in the mode.
  assert.equal(lookup(ctx({ mode: "ask" }), "ctrl+s"), "tab.tree");
  assert.equal(lookup(ctx({ mode: "ask" }), "ctrl+w"), "tab.workflows");
  assert.equal(lookup(ctx({ mode: "ask" }), "ctrl+d"), "tab.changes");
  // The question's own keys still win where they overlap: a digit picks an option.
  assert.equal(lookup(ctx({ mode: "ask" }), "3"), "ask.pick");
  assert.equal(lookup(ctx({ mode: "ask" }), "esc"), "ask.decline");
});

test("a bare letter means what the OPEN TAB says it means", () => {
  // The bug this replaces: `x` was `wf.stop` everywhere and the panel host re-routed
  // it to the changes tab by hand, so the binding and the behaviour disagreed and
  // `X` could not be routed at all. Now the table decides, per tab.
  assert.equal(lookup(ctx({ mode: "panel", tab: "changes" }), "x"), "changes.revert");
  assert.equal(lookup(ctx({ mode: "panel", tab: "changes" }), "X"), "changes.revertAll");
  assert.equal(lookup(ctx({ mode: "panel", tab: "workflows" }), "x"), "wf.stop");
  assert.equal(lookup(ctx({ mode: "panel", tab: "tree" }), "s"), "panel.confirmSummarize");
  // `e` splits a thread in the tree and is bound NOWHERE else — the server op had no
  // key at all until this, and a stray `e` must not reach it from the model picker.
  assert.equal(lookup(ctx({ mode: "panel", tab: "tree" }), "e"), "tree.extract");
  assert.equal(lookup(ctx({ mode: "panel", tab: "changes" }), "e"), null);
  // `m` is extract's mirror and lives in the same scope.
  assert.equal(lookup(ctx({ mode: "panel", tab: "tree" }), "m"), "tree.moveInto");
  assert.equal(lookup(ctx({ mode: "panel", tab: "model" }), "m"), null);
  assert.equal(lookup(ctx({ mode: "panel", tab: "tree", panelFiltering: true }), "e"), null);
  // …and outside its tab a scoped letter is not bound. `s` in the model picker used
  // to commit a model — a bare letter silently changing a setting.
  assert.equal(lookup(ctx({ mode: "panel", tab: "model" }), "s"), null);
  assert.equal(lookup(ctx({ mode: "panel", tab: "model" }), "x"), null);
  assert.equal(lookup(ctx({ mode: "panel", tab: "skills" }), "p"), null);
  // A context that names no tab is the panel closed: no tab-local row can fire.
  assert.equal(lookup(ctx({ mode: "panel" }), "x"), null);
});

test("the workflow verbs the run view prints are all bound", () => {
  const wf = ctx({ mode: "panel", tab: "workflows" });
  assert.equal(lookup(wf, "e"), "wf.script");
  assert.equal(lookup(wf, "f"), "wf.filter");
  assert.equal(lookup(wf, "o"), "wf.openAgent");
});

test("digits address panel rows, and pgup/pgdn page them", () => {
  for (const d of ["1", "5", "9"]) {
    assert.equal(lookup(ctx({ mode: "panel", tab: "model" }), d), "panel.pick");
  }
  assert.equal(lookup(ctx({ mode: "panel", tab: "model" }), "pageup"), "move.pageUp");
  assert.equal(lookup(ctx({ mode: "panel", tab: "model" }), "pagedown"), "move.pageDown");
});

test("the filter buffer takes the keyboard, and gives every letter back as text", () => {
  // `/` opens it, but only where a list is long enough to need narrowing.
  for (const tab of FILTER_TABS) {
    assert.equal(lookup(ctx({ mode: "panel", tab }), "/"), "panel.filter");
  }
  assert.equal(lookup(ctx({ mode: "panel", tab: "changes" }), "/"), null);
  // While it is open, every bare letter and digit in the panel is unbound — which
  // is the whole reason filtering is modal: `s`, `x` and `p` are live letters, and
  // a typist reaching them would have pinned a model and stopped a run.
  const filtering = ctx({ mode: "panel", tab: "model", panelFiltering: true });
  for (const chord of ["j", "k", "1", "9", "/"]) assert.equal(lookup(filtering, chord), null);
  assert.equal(lookup(ctx({ mode: "panel", tab: "workflows", panelFiltering: true }), "x"), null);
  // Arrows still move and ⏎ still commits: a filter narrows a list, it does not
  // replace the list's own keyboard.
  assert.equal(lookup(filtering, "up"), "move.up");
  assert.equal(lookup(filtering, "enter"), "panel.confirm");
  // Escape unwinds ONE level — the buffer, not the panel.
  assert.equal(lookup(filtering, "esc"), "panel.filterExit");
  assert.equal(lookup(ctx({ mode: "panel", tab: "model" }), "esc"), "panel.close");
  assert.equal(lookup(filtering, "backspace"), "panel.filterBack");
});

test("the rail can stop what it lists", () => {
  assert.equal(lookup(ctx({ mode: "rail" }), "x"), "rail.stop");
  // …and only there: `x` in the composer is a character.
  assert.equal(lookup(ctx({ mode: "chat" }), "x"), null);
});

test("every tab has exactly one chord, and ^t names no tab", () => {
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

test("resolve is lookup straight off an ink keypress", () => {
  assert.equal(resolve(ctx(), "", { escape: true, ...{} }), "cancel");
  assert.equal(resolve(ctx({ mode: "panel" }), "", { downArrow: true }), "move.down");
  assert.equal(resolve(ctx(), "x"), null);
});

// ---------------------------------------------------------------------------
// The overlay
// ---------------------------------------------------------------------------

test("the overlay documents the table and only the table", () => {
  const documented = BINDINGS.filter((b) => b.section && b.desc);
  const sections = helpSections();
  const rows = sections
    .filter((s) => !s.limits && !s.unavailable && !s.commands)
    .flatMap((s) => s.keys);
  assert.equal(rows.length, documented.length);
  // The `/` commands are their own section: the overlay is generated from chords, so
  // a command with no chord (`/compact`) was listed in the `/` popup and NOWHERE else,
  // while `?` claimed to be the place you find out what bough can do.
  const commands = sections.find((s) => s.section === "typed at the prompt");
  assert.ok(commands, "the overlay lists the / commands");
  assert.deepEqual(
    commands.keys.map(([name]) => name),
    ["!cmd", "@path", ...SLASH_COMMANDS.map((c) => `/${c.name}`)],
  );
  assert.ok(commands.keys.every(([, desc]) => desc.length > 0));

  // The tree's marks decide how every row in the switcher reads, and they were documented
  // nowhere — not in the overlay, not in the keymap, not in the spec.
  const marks = sections.find((s) => s.section === "marks in the tree");
  assert.ok(marks, "the overlay explains the tree's glyphs");
  for (const glyph of ["●", "↦", "⑂", "≣"]) {
    assert.ok(marks.keys.some(([g]) => g.includes(glyph)), `${glyph} is unexplained`);
  }
  // Every documented row carries a description; an empty one would render a key
  // with nothing beside it, which is how the old overlay rotted.
  for (const [chord, desc] of rows) {
    assert.ok(chord.length > 0);
    assert.ok(desc.length > 0);
  }
});

/**
 * `esc esc` prints twice — "clear the draft" and "go back to a turn and fork it · empty
 * draft" — and only the second said which state it belonged to, so on the one screen
 * that exists to answer what a key does, the pair read as a contradiction.
 */
test("a guarded row says WHICH state it belongs to, both ways round", () => {
  const rows = helpSections().filter((s) => !s.limits && !s.unavailable && !s.commands)
    .flatMap((s) => s.keys);
  const escEsc = rows.filter(([chord]) => chord === "esc esc").map(([, desc]) => desc);
  assert.equal(escEsc.length, 2, escEsc.join(" | "));
  assert.ok(escEsc.some((d) => d.endsWith("· with a draft")), escEsc.join(" | "));
  assert.ok(escEsc.some((d) => d.endsWith("· empty draft")), escEsc.join(" | "));
});

test("every control chord live in chat is documented somewhere", () => {
  // `^s` was bound to the tree, carried no `desc`, and so appeared in NEITHER the
  // table nor the `not bound` list: a reflex chord that silently moved the keyboard
  // to another surface and that the overlay denied all knowledge of. The rule the
  // file already states in its header — bound implies documented — now has a test.
  const unavailable = new Set(UNAVAILABLE.keys.map(([c]) => c));
  const chatChords = new Set(
    BINDINGS.filter((b) => (b.mode === "chat" || b.mode === "*") && b.chord.startsWith("ctrl+"))
      .map((b) => b.chord),
  );
  for (const chord of chatChords) {
    const pretty = chord.replace("ctrl+", "^");
    // A row may document two chords at once (`label: "^k ^u"`), which is a real
    // pattern here and not a gap — the reader still finds the key.
    const documented = BINDINGS.some((b) =>
      b.section && b.desc && (b.chord === chord || (b.label ?? "").split(" ").includes(pretty))
    );
    assert.ok(
      documented || unavailable.has(pretty),
      `${chord} is bound in chat but documented nowhere`,
    );
  }
});

test("slashCommandFor: a draft that IS a command, and nothing looser", () => {
  // The bug: `/` commands fired only from the completion popup, and the popup only
  // exists if the text rendered a keystroke at a time. A PASTED `/model` therefore
  // went to the frontier model as prose — 19k tokens, billed, and the conversation
  // auto-titled "Model Architecture Discussion".
  assert.equal(slashCommandFor("/model"), "tab.model");
  assert.equal(slashCommandFor("  /help  "), "help.open"); // trimmed like the draft is
  assert.equal(slashCommandFor("/NEW"), "session.new"); // a command is not case
  // Prose that merely starts with one is a message, not a command.
  assert.equal(slashCommandFor("/help me name this variable"), null);
  assert.equal(slashCommandFor("look at /model"), null);
  assert.equal(slashCommandFor("/nosuchcommand"), null);
  assert.equal(slashCommandFor("/"), null);
  assert.equal(slashCommandFor(""), null);
  // A skill reference is TEXT the model reads, so it must still be sent.
  assert.equal(slashCommandFor("/prewalk"), null);
});

test('the "not bound" section is true — none of those chords is bound', () => {
  // ^y used to be listed here; it is the theme tab's chord now, so it is gone from
  // the section. `!` left for a different reason — it is a real sigil now (it starts
  // a shell job), so listing it as unavailable would be the section telling a lie,
  // which is the one thing it may not do.
  const unbound = ["ctrl+g", "ctrl+v", "ctrl+r", "ctrl+z", "meta+d"];
  for (const chord of unbound) {
    assert.equal(
      BINDINGS.some((b) => b.chord === chord),
      false,
      `${chord} is listed as not bound but the table binds it`,
    );
  }
  // `home end` is one prose row rather than a chord: those keys are not bound because
  // OpenTUI's React layer never delivers them, so there is nothing to shadow. It is in the
  // section for the same reason the rest are — a reader will press End and deserves a why.
  assert.equal(UNAVAILABLE.keys.length, unbound.length + 1);
  assert.ok(UNAVAILABLE.keys.some(([chord]) => chord === "home end"));
});

test("every section header survives flattening, and carries its rows", () => {
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

test("the overlay is taller than a terminal, which is why it scrolls", () => {
  // If this ever stops being true the windowing in `Help` is dead code and should
  // go. While it IS true, a renderer that does not window is a broken renderer.
  assert.ok(helpLines().length > 24);
});

test("the prose sections carry no key column of their own", () => {
  const limits = helpSections().find((s) => s.limits);
  assert.ok(limits);
  for (const [chord] of limits.keys) assert.equal(chord, "");
});

// ---------------------------------------------------------------------------
// Line editing
// ---------------------------------------------------------------------------

const line = (text: string, cursor = text.length): LineState => ({ text, cursor });

test("cursor motion clamps at both ends and returns the same object on a no-op", () => {
  const start = line("abc", 0);
  assert.equal(editLine(start, "cursor.left"), start);
  const end = line("abc", 3);
  assert.equal(editLine(end, "cursor.right"), end);
  assert.deepEqual(editLine(line("abc", 1), "cursor.right"), { text: "abc", cursor: 2 });
});

test("home/end are the LOGICAL line's, not the whole draft's", () => {
  const s = line("first\nsecond", 8); // inside "second"
  assert.deepEqual(editLine(s, "cursor.home"), { text: "first\nsecond", cursor: 6 });
  assert.deepEqual(editLine(s, "cursor.end"), { text: "first\nsecond", cursor: 12 });
});

test("↑/↓ hold the column against the line they land on, and stop at the ends", () => {
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

test("word motion and word delete agree on where a word starts", () => {
  const s = line("alpha beta gamma", 16);
  const back = editLine(s, "cursor.wordLeft");
  assert.equal(back.cursor, 11);
  assert.deepEqual(editLine(s, "delete.wordBack"), { text: "alpha beta ", cursor: 11 });
});

test("the kill keys cut to the ends of the current line only", () => {
  const s = line("first\nsecond half", 12); // inside "second half"
  assert.deepEqual(editLine(s, "delete.toEnd"), { text: "first\nsecond", cursor: 12 });
  assert.deepEqual(editLine(s, "delete.toStart"), { text: "first\n half", cursor: 6 });
  assert.deepEqual(editLine(s, "delete.line"), EMPTY_LINE);
});

test("backspace and delete-forward move the cursor the way each should", () => {
  assert.deepEqual(editLine(line("abc", 2), "delete.back"), { text: "ac", cursor: 1 });
  assert.deepEqual(editLine(line("abc", 1), "delete.forward"), { text: "ac", cursor: 1 });
  const atStart = line("abc", 0);
  assert.equal(editLine(atStart, "delete.back"), atStart);
  const atEnd = line("abc", 3);
  assert.equal(editLine(atEnd, "delete.forward"), atEnd);
});

test("newline inserts at the cursor rather than sending", () => {
  assert.deepEqual(editLine(line("ab", 1), "newline"), { text: "a\nb", cursor: 2 });
  assert.deepEqual(insertText(line("ab", 1), "XY"), { text: "aXYb", cursor: 3 });
});

// ---------------------------------------------------------------------------
// Raw input
// ---------------------------------------------------------------------------

test("only a trailing \\r sends a coalesced chunk", () => {
  assert.deepEqual(chunkInput("hello\r"), { body: "hello", send: true });
  // ^j after fast typing arrives in the same read and must NOT send.
  assert.deepEqual(chunkInput("hello\n"), { body: "hello\n", send: false });
  assert.deepEqual(chunkInput("two\r\nlines\r"), { body: "two\nlines", send: true });
});

test("stripCtl removes invisible bytes but keeps newlines and tabs out of harm", () => {
  assert.equal(stripCtl("a\x00b\x07c"), "abc");
  assert.equal(stripCtl("keep\nthe newline"), "keep\nthe newline");
  // This line used to assert "[31mred" — it pinned the bug. Removing the escape
  // byte and keeping the rest of the sequence is what typed a terminal's own key
  // encoding into the user's draft; see the escape-sequence test below.
  assert.equal(stripCtl("\x1b[31mred"), "red");
});

test("isTextInput tells typing from a chord", () => {
  assert.equal(isTextInput("a"), true);
  assert.equal(isTextInput("a", { ctrl: true }), false);
  assert.equal(isTextInput("", { upArrow: true }), false);
  assert.equal(isTextInput("\r", { return: true }), false);
  assert.equal(isTextInput(""), false);
});

test("an escape sequence is dropped whole, never typed into the draft", () => {
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

/**
 * `/compact` is the one command whose trailing text is an argument, not prose. The
 * strict rule (`slashCommandFor`) is still what protects everything else: `/help me
 * name this` is a sentence, and dispatching it would swallow a message.
 */
test("slashInvocation: an argument reaches the commands that declare one", () => {
  assert.deepEqual(slashInvocation("/compact"), { command: "session.compact", arg: "" });
  assert.deepEqual(
    slashInvocation("/compact focus on the parser"),
    { command: "session.compact", arg: "focus on the parser" },
  );
  // Case and surrounding space are the draft's, not the command's.
  assert.deepEqual(
    slashInvocation("  /COMPACT   keep the migration plan  "),
    { command: "session.compact", arg: "keep the migration plan" },
  );
  // Commands that take no argument keep the exact-match rule.
  assert.deepEqual(slashInvocation("/model"), { command: "tab.model", arg: "" });
  assert.equal(slashInvocation("/help me name this variable"), null);
  assert.equal(slashInvocation("/model the domain first"), null);
  assert.equal(slashInvocation("/nosuchcommand anything"), null);
  assert.equal(slashInvocation("look at /compact"), null);
  assert.equal(slashInvocation(""), null);
});

/**
 * The failure this pins was silent and convincing. `/clear`, typed out of Claude Code
 * habit, was sent to the frontier model as prose; haiku answered "Done. State cleared."
 * and offered to revert the workspace's modified files. A made-up confirmation for an
 * operation that never happened, one step from the user's uncommitted work.
 */
test("a bare /word that is not a command is caught, with the nearest name", () => {
  // A command another harness has, mapped to the name bough uses — as a SUGGESTION.
  // Running `/new` on a guess about which product the user came from would be doing
  // something destructive-looking they did not choose.
  assert.deepEqual(unknownCommand("/clear"), { name: "clear", suggestion: "new" });
  assert.deepEqual(unknownCommand("/resume"), { name: "resume", suggestion: "tree" });
  assert.deepEqual(unknownCommand("/diff"), { name: "diff", suggestion: "changes" });
  // `/exit` and `/quit` are real habits with no bough equivalent: named, not guessed at.
  assert.deepEqual(unknownCommand("/quit"), { name: "quit", suggestion: null });

  // A typo finds its neighbour, in commands and in skills alike.
  assert.equal(unknownCommand("/mode")?.suggestion, "model");
  assert.equal(unknownCommand("/prewal", ["prewalk"])?.suggestion, "prewalk");
  assert.deepEqual(unknownCommand("/zzzz"), { name: "zzzz", suggestion: null });

  // Everything that IS legitimate passes through untouched.
  assert.equal(unknownCommand("/model"), null);
  assert.equal(unknownCommand("/compact"), null);
  assert.equal(unknownCommand("/prewalk", ["prewalk"]), null);
  // Not a lone `/word`: a skill reference with a task, and prose that starts with one.
  assert.equal(unknownCommand("/prewalk fix the parser"), null);
  assert.equal(unknownCommand("/clear the cache in redis.ts"), null);
  assert.equal(unknownCommand("look at /clear"), null);
  assert.equal(unknownCommand("/"), null);
  assert.equal(unknownCommand(""), null);
});
