// Keybindings as data: KEYMAP drives the `?` help overlay — the single place
// keys are documented (the status bar shows only session info + "? help").
//
// Two conventions the overlay renders differently:
//   `limits`      — things bough deliberately WON'T do, so a user stops waiting
//                   for them (a turn survives quitting, `!` output never reaches
//                   the model). Stated as facts, not bindings.
//   `unavailable` — chords a terminal veteran WILL try that bough doesn't bind.
//                   Silently eating ^r/^y/^z read as "broken"; naming them, with
//                   the thing to use instead, turns a dead end into a signpost.
export const KEYMAP: {
  section: string;
  keys: [string, string][];
  /** Rendered muted: these are not live bindings. */
  unavailable?: boolean;
  /** Rendered as prose rows without a key column. */
  limits?: boolean;
}[] = [
  {
    section: "the four you need",
    keys: [
      ["enter", "send your message"],
      ["esc", "interrupt whatever is running"],
      ["?", "this help · any key closes it"],
      ["^c ^c", "quit bough"],
    ],
  },
  {
    section: "writing a message",
    keys: [
      ["⌥enter", "queue it for after the current turn instead of steering"],
      ["^j", "newline (shift+enter too, where the terminal reports it)"],
      ["esc esc", "clear the draft — ↑ brings it back · rewind when already empty"],
      ["↑/↓", "recall what you've sent · move between lines in a multi-line draft"],
      ["/", "skills and commands · @ workspace files · tab accepts the gray preview"],
      ["! cmd", "run a shell command locally · ↑/↓ or type to search past ones"],
    ],
  },
  {
    section: "editing the draft — readline, as you'd expect",
    keys: [
      ["^a ^e", "start / end of line"],
      ["^b ^f", "back / forward a character"],
      ["⌥b ⌥f", "back / forward a word (⌥←/→ too)"],
      ["^d", "delete the character ahead"],
      ["^w ⌥⌫", "delete the word behind"],
      ["^k ^u", "delete to end of line · delete the whole line"],
      ["⌘⌫ ⌘←/→", "delete to line start · jump to line start/end"],
    ],
  },
  {
    section: "reading the transcript",
    keys: [
      ["^s", "search this conversation — enter/↓ next match, ↑ previous"],
      ["^e", "expand/collapse every tool call (empty draft only)"],
      ["click ▸", "expand/collapse one fold · right-click copies that section"],
      ["wheel", "scroll · pgup/pgdn too"],
      ["↓ (empty)", "drop into the live subagent rail · enter opens one"],
    ],
  },
  {
    section: "panels — one view, nine tabs · tab cycles them",
    keys: [
      ["^p ^o ^t", "sessions · model · toggle the panel — work any time, even mid-draft"],
      ["^f ^d ^b", "conversation · changes · jobs — empty draft only, they edit text otherwise"],
      ["j/k or ↑↓", "move · / filters · enter opens or acts on the row"],
      ["esc", "back out one level, then out of the panel"],
      ["x", "the destructive action for that tab — irreversible ones ask twice"],
    ],
  },
  {
    section: "sessions tab",
    keys: [
      ["n", "start a new session · g/G jump to first/last"],
      ["x", "archive (asks twice) · u restores · h shows hidden"],
      ["D", "deprecate a branch — hides it without archiving"],
    ],
  },
  {
    section: "conversation tab — the tree of turns and branches",
    keys: [
      ["enter", "rewind to a turn, or open a branch"],
      ["v", "select a range → c compact · e extract · m copy to · x delete"],
      ["s", "label sections by topic · C compacts the whole session"],
      ["glyphs", "● root ⑂ fork ◆ subagent ≣ compacted ◇ tool"],
    ],
  },
  {
    section: "changes tab — what bough wrote, before it reaches your repo",
    keys: [
      ["enter", "apply the selected file to your checkout · A applies all of them"],
      ["→ / ←", "focus the hunks pane (j/k scrolls it) · back to the list"],
      ["x", "revert everything listed — asks twice, and there's no undo after"],
    ],
  },
  {
    section: "jobs · workflows · model · mcp · theme tabs",
    keys: [
      ["jobs", "enter opens a job's output (j/k scrolls) · x stops it"],
      ["workflows", "enter/→ opens a run then an agent · p pauses · r reruns · o opens its chat"],
      ["model", "enter sets model/effort or edits API keys · x removes a key (asks twice)"],
      ["mcp", "c connect · e enable · r restart · a auth"],
      ["theme", "↑↓ previews live · enter keeps it"],
    ],
  },
  {
    section: "when bough asks you a question",
    keys: [
      ["1-9", "pick an option · t types your own answer"],
      ["enter / esc", "send the answer · decline the question"],
    ],
  },
  {
    section: "stopping things",
    keys: [
      ["esc", "interrupt the running turn only"],
      ["^x ^x", "stop everything here — the turn, its subagents, its background shells"],
      ["^c ^c", "quit the TUI · the server and any subagents keep running"],
    ],
  },
  {
    section: "what bough won't do",
    limits: true,
    keys: [
      ["", "Quitting doesn't stop work. Subagents and background shells live in the"],
      ["", "server, not this window — ^x ^x first if you meant to stop them."],
      ["", "`!` commands are yours, not the agent's: it never sees them, they run in"],
      ["", "your real workspace with no sandbox, and they're killed after 30 seconds."],
      ["", "A subagent's edits stay on its own branch until a turn calls adopt()."],
      ["", "Nothing reaches your repo until you apply it in the changes tab."],
    ],
  },
  {
    section: "keys bough doesn't bind — use these instead",
    unavailable: true,
    keys: [
      ["^r", "no reverse-i-search · ^s searches the conversation, ! then type searches shells"],
      ["^y", "no yank/paste ring · ↑ brings back a cleared draft"],
      ["^z", "no suspend — the TUI owns the screen · ^c ^c quits, work keeps running"],
      ["⌥d", "no forward kill-word · ^k clears to end of line"],
      ["^t", "not transpose here — it toggles the panel"],
    ],
  },
];

/** Question-hold card hints (the card replaces the composer, so it carries its own keys). */
export const ASK_OPTIONS_HINT = "1-9 choose · t type an answer · esc decline";
export const ASK_TYPING_HINT = "enter send · esc decline";
export const ASK_TYPING_BACK_HINT = "enter send · esc back to options";

export type UiMode = "chat" | "approval" | "new" | "help" | "panel";
