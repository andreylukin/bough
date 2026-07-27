/**
 * Input handling: keys are DATA, and the help overlay is generated from that data.
 *
 * THE INVARIANT THIS HOLDS: **there is exactly one description of what a key does,
 * and it is the thing that makes the key do it.** `BINDINGS` is a flat table of
 * `(mode, chord, command)` rows carrying their own help text; `lookup()` resolves a
 * keypress against it and `helpSections()` renders it. The old tree kept the two
 * apart — a 109-line `keys.ts` of prose next to a 3,618-line `App.tsx` of
 * `if (key.ctrl && ch === "f")` — so the overlay documented keys that had been
 * renamed and stayed silent about ones that had been added. Here that cannot
 * happen: a row that is not bound cannot be documented, and a chord that is bound
 * twice in the same mode is a test failure rather than a dead binding nobody
 * notices.
 *
 * SECOND INVARIANT — **resolution is pure and needs no terminal.** `chordOf`
 * canonicalizes ink's `(input, key)` pair into a string; `lookup` picks the first
 * row whose guards hold against a plain context object. Both are functions of
 * data, so `keys.test.ts` asserts the whole keymap with no TTY, no renderer and no
 * server (task AC; plan §7).
 *
 * THIRD — **the same chord may mean two things, and the guard says which.** `^f`
 * opens the tree on an empty composer and moves the cursor forward when there is
 * text; `↑` walks history on one line and moves the cursor on several. That is not
 * an accident to be tidied away, it is how a composer and a pager share a
 * keyboard. The guards are explicit fields rather than an ordering trick, and the
 * duplicate test knows the difference: two rows with the same chord AND the same
 * guards is a bug, an unguarded row placed ahead of a guarded one is a bug, and
 * two rows with complementary guards is the design.
 *
 * FOURTH — **`key.super` is only believable under the kitty keyboard protocol.**
 * Without it a terminal sends Cmd+←/→ as `CSI 1;9 C/D` and ink leaks bit 3 of the
 * modifier field into the meta flag, so those sequences are intercepted in
 * `mouse.ts` and delivered as nav-key events instead. `term.ts` decides which path
 * is live; this module binds both, which is why `super+left` and the intercepted
 * `cmdHome` land on the same command.
 *
 * Line editing lives here too, as pure `LineState → LineState` functions. The
 * composer's cursor arithmetic is the part users notice when it is a character off
 * on a wrapped paste, and it has no business being inside a React component.
 */
import { wordLeft, wordRight } from "./format.ts";

// ---------------------------------------------------------------------------
// Modes and commands
// ---------------------------------------------------------------------------

/**
 * Which surface has the keyboard. Not a view stack: a mode is answered by exactly
 * one binding set, so a chord can never be handled twice on its way down.
 */
export type UiMode = "chat" | "rail" | "ask" | "tree" | "workflows" | "help";

export type Command =
  // -- global ---------------------------------------------------------------
  /** First ^c: show the quit hint. A single ^c must never unmount ink under it. */
  | "quit.arm"
  | "quit"
  | "help.open"
  | "help.close"
  | "view.chat"
  | "view.tree"
  | "view.workflows"
  // -- composing ------------------------------------------------------------
  | "send"
  | "send.queue"
  | "newline"
  | "draft.clear"
  | "cancel"
  | "history.prev"
  | "history.next"
  // -- reading --------------------------------------------------------------
  | "fold.all"
  | "scroll.up"
  | "scroll.down"
  // -- editing the line -----------------------------------------------------
  | "cursor.left"
  | "cursor.right"
  | "cursor.home"
  | "cursor.end"
  | "cursor.wordLeft"
  | "cursor.wordRight"
  | "cursor.up"
  | "cursor.down"
  | "delete.back"
  | "delete.forward"
  | "delete.wordBack"
  | "delete.toEnd"
  | "delete.toStart"
  | "delete.line"
  // -- the live subagent rail ----------------------------------------------
  | "rail.enter"
  | "rail.up"
  | "rail.down"
  | "rail.open"
  | "rail.exit"
  // -- a question hold ------------------------------------------------------
  | "ask.pick"
  | "ask.send"
  | "ask.decline"
  // -- list navigation, shared by the tree and the run view -----------------
  | "move.up"
  | "move.down"
  | "move.in"
  | "move.out"
  | "open"
  // -- workflow steering (spec §8) -----------------------------------------
  | "wf.pause"
  | "wf.resume"
  | "wf.stop"
  | "wf.rerun";

// ---------------------------------------------------------------------------
// Chords (pure)
// ---------------------------------------------------------------------------

/** The subset of ink's `Key` this module reads. Structural, so ink's own type fits. */
export interface KeyFlags {
  upArrow?: boolean;
  downArrow?: boolean;
  leftArrow?: boolean;
  rightArrow?: boolean;
  pageUp?: boolean;
  pageDown?: boolean;
  home?: boolean;
  end?: boolean;
  return?: boolean;
  escape?: boolean;
  tab?: boolean;
  backspace?: boolean;
  delete?: boolean;
  ctrl?: boolean;
  shift?: boolean;
  meta?: boolean;
  super?: boolean;
}

/**
 * One keypress as a canonical string — `"ctrl+p"`, `"meta+enter"`, `"esc"`, `"?"`.
 *
 * Returns `""` for anything that is not a chord: a paste, a coalesced chunk of
 * typing, a bare modifier. The caller treats that as text, which is what keeps a
 * multi-character stdin read from being matched against the table by accident.
 */
export function chordOf(input: string, key: KeyFlags = {}): string {
  const mods: string[] = [];
  if (key.ctrl) mods.push("ctrl");
  if (key.meta) mods.push("meta");
  if (key.super) mods.push("super");

  let base: string;
  if (key.upArrow) base = "up";
  else if (key.downArrow) base = "down";
  else if (key.leftArrow) base = "left";
  else if (key.rightArrow) base = "right";
  else if (key.pageUp) base = "pageup";
  else if (key.pageDown) base = "pagedown";
  else if (key.home) base = "home";
  else if (key.end) base = "end";
  else if (key.escape) base = "esc";
  else if (key.tab) base = "tab";
  else if (key.backspace || key.delete) base = "backspace";
  else if (key.return) base = "enter";
  // A raw "\n" with no return flag can only be ^j. Terminals send \r for Return,
  // so this is the newline chord even on terminals that report no ctrl modifier
  // for it — the old tree shipped a bug where ^j submitted half a message.
  else if (input === "\n") return "ctrl+j";
  else if (input === " ") base = "space";
  else if (input.length === 1) base = input;
  else return "";

  if (key.shift && (base === "enter" || base === "tab")) mods.push("shift");
  return mods.length > 0 ? `${mods.join("+")}+${base}` : base;
}

const CHORD_GLYPH: Record<string, string> = {
  ctrl: "^",
  meta: "⌥",
  super: "⌘",
  shift: "⇧",
  up: "↑",
  down: "↓",
  left: "←",
  right: "→",
  enter: "⏎",
  esc: "esc",
  tab: "⇥",
  backspace: "⌫",
  pageup: "pgup",
  pagedown: "pgdn",
  space: "space",
};

/** A chord as the help overlay prints it: `"ctrl+p"` → `"^p"`. */
export function chordLabel(chord: string): string {
  const parts = chord.split("+");
  const base = parts.pop() ?? "";
  const mods = parts.map((m) => CHORD_GLYPH[m] ?? m).join("");
  return mods + (CHORD_GLYPH[base] ?? base);
}

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

/**
 * What a binding can be conditioned on. Every field is a plain boolean the caller
 * already knows, so a guard costs nothing to evaluate and nothing to fake.
 */
export interface KeyContext {
  mode: UiMode;
  /** The composer is empty, so a chord can mean something other than editing. */
  emptyDraft: boolean;
  /** The draft spans more than one line: ↑/↓ move the cursor, not history. */
  multiline: boolean;
  /** A turn is in flight in the open session. */
  busy: boolean;
  /** The previous Escape landed inside the double-tap window. */
  doubleEsc: boolean;
  /** A ^c is already pending — the next one quits (spec: ^c ^c quits). */
  quitArmed: boolean;
  /** At least one subagent is working, so ↓ can drop into the rail. */
  railLive: boolean;
}

export type Guard = Exclude<keyof KeyContext, "mode">;

export interface Binding {
  /** `"*"` binds in every mode — the handful of chords that must always work. */
  mode: UiMode | "*";
  /** Canonical chord, as `chordOf` produces it. */
  chord: string;
  command: Command;
  /** Every named flag must be true. */
  when?: Guard[];
  /** Every named flag must be false. */
  not?: Guard[];
  /** Help section. A binding with no section is an alias and is not documented. */
  section?: string;
  /** Terse: the overlay lays sections out in two columns, ~35 columns each. */
  desc?: string;
  /** Overrides the printed chord, for a run of rows that share one description. */
  label?: string;
}

const digits = (mode: UiMode | "*", command: Command, section: string, desc: string): Binding[] =>
  Array.from({ length: 9 }, (_v, i) => ({
    mode,
    chord: String(i + 1),
    command,
    ...(i === 0 ? { section, desc, label: "1-9" } : {}),
  }));

/**
 * Every binding in the TUI, in resolution order within a mode.
 *
 * Ordering is only ever used to put a GUARDED row ahead of its unguarded fallback
 * (`^f` opens the tree on an empty composer, moves the cursor otherwise). Two rows
 * that could both match the same context is a bug the duplicate test catches.
 */
export const BINDINGS: Binding[] = [
  // -- global ---------------------------------------------------------------
  // Two rows, because a SINGLE ^c must not quit: the first arms the hint, the
  // second acts on it. Bound in every mode — a panel you cannot leave is worse
  // than one you never opened, and ^c is the key everyone reaches for.
  {
    mode: "*",
    chord: "ctrl+c",
    command: "quit",
    when: ["quitArmed"],
    section: "leaving",
    label: "^c ^c",
    desc: "quit · subagents keep running",
  },
  { mode: "*", chord: "ctrl+c", command: "quit.arm" },

  // -- chat -----------------------------------------------------------------
  {
    mode: "chat",
    chord: "?",
    command: "help.open",
    when: ["emptyDraft"],
    section: "leaving",
    desc: "this overlay",
  },

  { mode: "chat", chord: "enter", command: "send", section: "compose", desc: "send" },
  {
    mode: "chat",
    chord: "meta+enter",
    command: "send.queue",
    section: "compose",
    desc: "queue for after this turn",
  },
  { mode: "chat", chord: "ctrl+j", command: "newline", section: "compose", desc: "newline" },
  {
    mode: "chat",
    chord: "esc",
    command: "draft.clear",
    when: ["doubleEsc"],
    section: "compose",
    label: "esc esc",
    desc: "clear the draft",
  },
  { mode: "chat", chord: "esc", command: "cancel" },
  {
    mode: "chat",
    chord: "up",
    command: "cursor.up",
    when: ["multiline"],
    section: "compose",
    label: "↑/↓",
    desc: "history · lines if multiline",
  },
  { mode: "chat", chord: "up", command: "history.prev" },
  { mode: "chat", chord: "down", command: "cursor.down", when: ["multiline"] },
  {
    mode: "chat",
    chord: "down",
    command: "rail.enter",
    when: ["emptyDraft", "railLive"],
    section: "read",
    desc: "into the live subagent rail",
  },
  { mode: "chat", chord: "down", command: "history.next" },

  // -- reading --------------------------------------------------------------
  {
    mode: "chat",
    chord: "ctrl+e",
    command: "fold.all",
    when: ["emptyDraft"],
    section: "read",
    desc: "fold/unfold every tool call",
  },
  { mode: "chat", chord: "pageup", command: "scroll.up", section: "read", desc: "scroll back" },
  {
    mode: "chat",
    chord: "pagedown",
    command: "scroll.down",
    section: "read",
    desc: "scroll forward",
  },

  // -- panels ---------------------------------------------------------------
  {
    mode: "chat",
    chord: "ctrl+f",
    command: "view.tree",
    when: ["emptyDraft"],
    section: "panels — need an empty draft",
    desc: "the conversation tree",
  },
  {
    mode: "chat",
    chord: "ctrl+w",
    command: "view.workflows",
    when: ["emptyDraft"],
    section: "panels — need an empty draft",
    desc: "workflow runs",
  },

  // -- editing the line -----------------------------------------------------
  {
    mode: "chat",
    chord: "ctrl+a",
    command: "cursor.home",
    section: "edit the line",
    label: "^a ^e",
    desc: "line start / end",
  },
  { mode: "chat", chord: "ctrl+e", command: "cursor.end" },
  { mode: "chat", chord: "home", command: "cursor.home" },
  { mode: "chat", chord: "end", command: "cursor.end" },
  { mode: "chat", chord: "left", command: "cursor.left" },
  { mode: "chat", chord: "right", command: "cursor.right" },
  {
    mode: "chat",
    chord: "ctrl+b",
    command: "cursor.left",
    section: "edit the line",
    label: "^b ^f",
    desc: "char back / forward",
  },
  { mode: "chat", chord: "ctrl+f", command: "cursor.right" },
  {
    mode: "chat",
    chord: "meta+b",
    command: "cursor.wordLeft",
    section: "edit the line",
    label: "⌥b ⌥f",
    desc: "word back / forward",
  },
  { mode: "chat", chord: "meta+f", command: "cursor.wordRight" },
  { mode: "chat", chord: "meta+left", command: "cursor.wordLeft" },
  { mode: "chat", chord: "meta+right", command: "cursor.wordRight" },
  {
    mode: "chat",
    chord: "ctrl+d",
    command: "delete.forward",
    section: "edit the line",
    label: "^d · ^w",
    desc: "delete char ahead · word behind",
  },
  { mode: "chat", chord: "ctrl+w", command: "delete.wordBack", not: ["emptyDraft"] },
  { mode: "chat", chord: "meta+backspace", command: "delete.wordBack" },
  {
    mode: "chat",
    chord: "ctrl+k",
    command: "delete.toEnd",
    section: "edit the line",
    label: "^k ^u",
    desc: "kill to end / whole line",
  },
  { mode: "chat", chord: "ctrl+u", command: "delete.line" },
  {
    mode: "chat",
    chord: "super+backspace",
    command: "delete.toStart",
    section: "edit the line",
    label: "⌘⌫ ⌘←→",
    desc: "to line start · jump to ends",
  },
  { mode: "chat", chord: "super+left", command: "cursor.home" },
  { mode: "chat", chord: "super+right", command: "cursor.end" },
  { mode: "chat", chord: "backspace", command: "delete.back" },

  // -- the live subagent rail ----------------------------------------------
  {
    mode: "rail",
    chord: "up",
    command: "rail.up",
    section: "the rail",
    label: "↑/↓",
    desc: "move",
  },
  { mode: "rail", chord: "down", command: "rail.down" },
  {
    mode: "rail",
    chord: "enter",
    command: "rail.open",
    section: "the rail",
    desc: "open a branch",
  },
  {
    mode: "rail",
    chord: "esc",
    command: "rail.exit",
    section: "the rail",
    desc: "back to the composer",
  },

  // -- a question hold ------------------------------------------------------
  ...digits("ask", "ask.pick", "when bough asks", "pick an option"),
  {
    mode: "ask",
    chord: "enter",
    command: "ask.send",
    section: "when bough asks",
    desc: "send what you typed",
  },
  {
    mode: "ask",
    chord: "esc",
    command: "ask.decline",
    section: "when bough asks",
    desc: "decline (the program catches it)",
  },

  // -- the conversation tree ------------------------------------------------
  { mode: "tree", chord: "k", command: "move.up", section: "tree", label: "j/k ↑↓", desc: "move" },
  { mode: "tree", chord: "j", command: "move.down" },
  { mode: "tree", chord: "up", command: "move.up" },
  { mode: "tree", chord: "down", command: "move.down" },
  { mode: "tree", chord: "enter", command: "open", section: "tree", desc: "open the conversation" },
  {
    mode: "tree",
    chord: "right",
    command: "move.in",
    section: "tree",
    label: "→ ←",
    desc: "drill into delegated work",
  },
  { mode: "tree", chord: "left", command: "move.out" },
  { mode: "tree", chord: "esc", command: "view.chat", section: "tree", desc: "back to chat" },
  { mode: "tree", chord: "ctrl+f", command: "view.chat" },

  // -- workflow runs (spec §8: pause, stop, relaunch from the journal) ------
  {
    mode: "workflows",
    chord: "k",
    command: "move.up",
    section: "workflows",
    label: "j/k ↑↓",
    desc: "move",
  },
  { mode: "workflows", chord: "j", command: "move.down" },
  { mode: "workflows", chord: "up", command: "move.up" },
  { mode: "workflows", chord: "down", command: "move.down" },
  {
    mode: "workflows",
    chord: "enter",
    command: "open",
    section: "workflows",
    desc: "what this run replayed",
  },
  {
    mode: "workflows",
    chord: "p",
    command: "wf.pause",
    section: "workflows",
    desc: "pause · in-flight agents finish",
  },
  { mode: "workflows", chord: "P", command: "wf.resume", section: "workflows", desc: "resume" },
  {
    mode: "workflows",
    chord: "x",
    command: "wf.stop",
    section: "workflows",
    desc: "stop · pause first to keep work",
  },
  {
    mode: "workflows",
    chord: "r",
    command: "wf.rerun",
    section: "workflows",
    desc: "relaunch from the journal",
  },
  {
    mode: "workflows",
    chord: "esc",
    command: "view.chat",
    section: "workflows",
    desc: "back to chat",
  },

  // -- the overlay itself ---------------------------------------------------
  { mode: "help", chord: "esc", command: "help.close" },
  { mode: "help", chord: "?", command: "help.close" },
  { mode: "help", chord: "q", command: "help.close" },
  { mode: "help", chord: "up", command: "scroll.up" },
  { mode: "help", chord: "down", command: "scroll.down" },
  { mode: "help", chord: "k", command: "scroll.up" },
  { mode: "help", chord: "j", command: "scroll.down" },
];

function guardsHold(binding: Binding, ctx: KeyContext): boolean {
  for (const g of binding.when ?? []) if (!ctx[g]) return false;
  for (const g of binding.not ?? []) if (ctx[g]) return false;
  return true;
}

const modesOverlap = (a: Binding["mode"], b: Binding["mode"]) => a === b || a === "*" || b === "*";

/** The command a chord means in this context, or null when nothing is bound. */
export function lookup(ctx: KeyContext, chord: string): Command | null {
  if (chord === "") return null;
  for (const b of BINDINGS) {
    if (!modesOverlap(b.mode, ctx.mode) || b.chord !== chord) continue;
    if (guardsHold(b, ctx)) return b.command;
  }
  return null;
}

/** `lookup` straight off an ink keypress. The one entry point a component needs. */
export function resolve(ctx: KeyContext, input: string, key: KeyFlags = {}): Command | null {
  return lookup(ctx, chordOf(input, key));
}

// ---------------------------------------------------------------------------
// The help overlay, generated from the table
// ---------------------------------------------------------------------------

export interface HelpSection {
  section: string;
  keys: [string, string][];
  /** Prose rows with no key column. */
  limits?: boolean;
  /** Chords a terminal veteran will try that bough does not bind. */
  unavailable?: boolean;
}

/**
 * Things bough deliberately WON'T do, so a user stops waiting for them. Prose, no
 * key column — these are not bindings and must not be printed as if they were.
 */
export const LIMITS: HelpSection = {
  section: "won't do",
  limits: true,
  keys: [
    ["", "^c ^c quits; subagents keep running"],
    ["", "programs run as you — no sandbox"],
    ["", "changes land in your checkout as they happen"],
    ["", "a running workflow takes no input — stop, edit, relaunch"],
  ],
};

/**
 * Chords a terminal veteran WILL try that bough does not bind. Rendered muted,
 * never accented: silently eating ^r/^y/^z reads as broken, and printing them
 * like live keys is worse.
 */
export const UNAVAILABLE: HelpSection = {
  section: "not bound",
  unavailable: true,
  keys: [
    ["^r", "no reverse search yet"],
    ["^y", "esc esc clears; ↑ restores"],
    ["^z", "no suspend · ^c ^c quits"],
    ["⌥d", "use ^k"],
  ],
};

/**
 * The overlay's sections, in table order.
 *
 * Derived, never authored: a key appears here because it is bound, with the text
 * the binding carries. That is the whole reason the descriptions live on the rows.
 */
export function helpSections(bindings: Binding[] = BINDINGS): HelpSection[] {
  const out: HelpSection[] = [];
  const bySection = new Map<string, [string, string][]>();
  for (const b of bindings) {
    if (!b.section || !b.desc) continue;
    let rows = bySection.get(b.section);
    if (!rows) {
      rows = [];
      bySection.set(b.section, rows);
      out.push({ section: b.section, keys: rows });
    }
    rows.push([b.label ?? chordLabel(b.chord), b.desc]);
  }
  out.push(LIMITS, UNAVAILABLE);
  return out;
}

/**
 * Bindings that can never fire, as `"mode chord"` strings.
 *
 * Two rows match the same keypress when they share a mode and chord AND the
 * earlier one's guards are implied by the later one's — the simple cases being
 * identical guards, or an unguarded row placed ahead of a guarded one. Exported so
 * the test asserting the keymap has no dead rows reads as one call.
 */
export function deadBindings(bindings: Binding[] = BINDINGS): string[] {
  const dead: string[] = [];
  const sig = (b: Binding) =>
    `${[...(b.when ?? [])].sort().join(",")}/${[...(b.not ?? [])].sort().join(",")}`;
  for (let i = 0; i < bindings.length; i++) {
    for (let j = i + 1; j < bindings.length; j++) {
      const a = bindings[i];
      const b = bindings[j];
      if (!modesOverlap(a.mode, b.mode) || a.chord !== b.chord) continue;
      const aWhen = new Set(a.when ?? []);
      const aNot = new Set(a.not ?? []);
      // `a` shadows `b` when every context `b` accepts is one `a` also accepts —
      // i.e. `a`'s guards are a subset of `b`'s.
      const shadows = [...aWhen].every((g) => (b.when ?? []).includes(g)) &&
        [...aNot].every((g) => (b.not ?? []).includes(g));
      if (shadows) dead.push(`${b.mode} ${b.chord}${sig(b) === "/" ? "" : ` (${sig(b)})`}`);
    }
  }
  return dead;
}

// ---------------------------------------------------------------------------
// Line editing (pure)
// ---------------------------------------------------------------------------

export interface LineState {
  text: string;
  cursor: number;
}

export const EMPTY_LINE: LineState = { text: "", cursor: 0 };

const clamp = (text: string, cursor: number): LineState => ({
  text,
  cursor: Math.max(0, Math.min(cursor, text.length)),
});

/** Start of the logical line the cursor sits on. Multiline-aware, like ⌘←. */
function lineStart(text: string, cursor: number): number {
  const nl = text.lastIndexOf("\n", cursor - 1);
  return nl < 0 ? 0 : nl + 1;
}

function lineEnd(text: string, cursor: number): number {
  const nl = text.indexOf("\n", cursor);
  return nl < 0 ? text.length : nl;
}

/** Move the cursor one visual line, keeping its column where it can. */
function moveLine(s: LineState, dir: -1 | 1): LineState {
  const start = lineStart(s.text, s.cursor);
  const col = s.cursor - start;
  if (dir === -1) {
    if (start === 0) return s;
    const prevStart = lineStart(s.text, start - 1);
    return clamp(s.text, Math.min(prevStart + col, start - 1));
  }
  const end = lineEnd(s.text, s.cursor);
  if (end >= s.text.length) return s;
  const nextEnd = lineEnd(s.text, end + 1);
  return clamp(s.text, Math.min(end + 1 + col, nextEnd));
}

/**
 * Apply an editing command. Returns the SAME object when nothing changed, so a
 * component can skip a render on a no-op (backspace at column 0, ↑ on line one).
 */
export function editLine(s: LineState, command: Command): LineState {
  switch (command) {
    case "cursor.left":
      return s.cursor === 0 ? s : clamp(s.text, s.cursor - 1);
    case "cursor.right":
      return s.cursor >= s.text.length ? s : clamp(s.text, s.cursor + 1);
    case "cursor.home":
      return clamp(s.text, lineStart(s.text, s.cursor));
    case "cursor.end":
      return clamp(s.text, lineEnd(s.text, s.cursor));
    case "cursor.wordLeft":
      return clamp(s.text, wordLeft(s.text, s.cursor));
    case "cursor.wordRight":
      return clamp(s.text, wordRight(s.text, s.cursor));
    case "cursor.up":
      return moveLine(s, -1);
    case "cursor.down":
      return moveLine(s, 1);

    case "delete.back":
      return s.cursor === 0
        ? s
        : { text: s.text.slice(0, s.cursor - 1) + s.text.slice(s.cursor), cursor: s.cursor - 1 };
    case "delete.forward":
      return s.cursor >= s.text.length
        ? s
        : { text: s.text.slice(0, s.cursor) + s.text.slice(s.cursor + 1), cursor: s.cursor };
    case "delete.wordBack": {
      const from = wordLeft(s.text, s.cursor);
      return from === s.cursor
        ? s
        : { text: s.text.slice(0, from) + s.text.slice(s.cursor), cursor: from };
    }
    case "delete.toEnd": {
      const end = lineEnd(s.text, s.cursor);
      return end === s.cursor
        ? s
        : { text: s.text.slice(0, s.cursor) + s.text.slice(end), cursor: s.cursor };
    }
    case "delete.toStart": {
      const start = lineStart(s.text, s.cursor);
      return start === s.cursor
        ? s
        : { text: s.text.slice(0, start) + s.text.slice(s.cursor), cursor: start };
    }
    case "delete.line":
      return s.text === "" ? s : EMPTY_LINE;
    case "newline":
      return insertText(s, "\n");

    default:
      return s;
  }
}

/** Insert text at the cursor. The one mutation a keypress that is not a chord makes. */
export function insertText(s: LineState, text: string): LineState {
  if (text === "") return s;
  return {
    text: s.text.slice(0, s.cursor) + text + s.text.slice(s.cursor),
    cursor: s.cursor + text.length,
  };
}

/** Invisible control bytes must never reach the draft — or the transcript. */
export function stripCtl(s: string): string {
  // deno-lint-ignore no-control-regex -- stripping control bytes is the point
  return s.replace(/[\x00-\x08\x0b-\x1f\x7f]/g, "");
}

/**
 * What a coalesced stdin chunk means for the composer.
 *
 * A fast typist's keystrokes and their Return arrive in ONE read, so a newline can
 * be data rather than a keypress. Only a trailing `\r` means "…then send": a bare
 * `\n` can only have come from ^j and is always a literal newline. The old tree
 * shipped the other rule and sent half-written messages.
 */
export function chunkInput(chunk: string): { body: string; send: boolean } {
  const send = chunk.endsWith("\r");
  const body = stripCtl((send ? chunk.slice(0, -1) : chunk).replace(/\r\n?/g, "\n"));
  return { body, send };
}

/** Is this keypress ordinary text rather than a chord? */
export function isTextInput(input: string, key: KeyFlags = {}): boolean {
  if (input === "") return false;
  if (key.ctrl || key.meta || key.super) return false;
  if (key.return || key.escape || key.tab || key.backspace || key.delete) return false;
  if (key.upArrow || key.downArrow || key.leftArrow || key.rightArrow) return false;
  if (key.pageUp || key.pageDown || key.home || key.end) return false;
  return true;
}
