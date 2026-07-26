// Keybindings as data: KEYMAP drives the `?` help overlay — the single place
// keys are documented (the status bar shows only session info + "? help").
//
// Descriptions are deliberately TERSE: the overlay lays sections out in two
// columns on a normal terminal, so a row has ~35 columns of description before
// it wraps and the whole thing stops fitting on one screen. Say the trigger and
// the effect; drop the sentence. The four keys everyone needs live in the
// overlay's pinned header, not in a section.
//
// Two section flags render differently:
//   `limits`      — things bough deliberately WON'T do, so a user stops waiting
//                   for them. Prose rows, no key column.
//   `unavailable` — chords a terminal veteran WILL try that bough doesn't bind.
//                   Rendered muted, never accented: silently eating ^r/^y/^z
//                   reads as broken, and printing them like live keys is worse.
export const KEYMAP: {
  section: string;
  keys: [string, string][];
  unavailable?: boolean;
  limits?: boolean;
}[] = [
  {
    section: "compose",
    keys: [
      ["⌥enter", "queue for after this turn"],
      ["^j", "newline"],
      ["esc esc", "clear draft (↑ restores)"],
      ["↑/↓", "history · lines if multiline"],
      ["/ @ tab", "skills · files · accept preview"],
      ["! cmd", "local shell · ↑/↓ its history"],
    ],
  },
  {
    section: "edit the line",
    keys: [
      ["^a ^e", "line start / end"],
      ["^b ^f", "char back / forward"],
      ["⌥b ⌥f", "word back / forward"],
      ["^d · ^w ⌥⌫", "delete char ahead · word behind"],
      ["^k ^u", "kill to end / whole line"],
      ["⌘⌫ ⌘←→", "to line start · jump to ends"],
    ],
  },
  {
    section: "read",
    keys: [
      ["^s", "search this conversation"],
      ["^e", "fold/unfold all tool calls"],
      ["wheel", "scroll (pgup/pgdn)"],
      ["click", "▸ folds · right-click copies"],
      ["↓", "into the live subagent rail"],
    ],
  },
  {
    section: "panels — ^t/tab · ^f ^d ^b need an empty draft",
    keys: [
      ["^p ^o", "sessions · model — any time"],
      ["^f ^d ^b", "conv · changes · jobs"],
      ["j/k · /", "move · filter · enter acts"],
      ["x · esc", "destructive · back out"],
    ],
  },
  {
    section: "tabs",
    keys: [
      ["sessions", "n new · x archive · u restore"],
      ["conv", "rewind · v range · C compact"],
      ["changes", "enter apply · A all · x revert"],
      ["jobs", "enter output · x stop"],
      ["workflows", "p pause · r rerun · o chat"],
      ["model", "enter set · x drop key"],
      ["mcp", "c connect · e enable · r restart"],
    ],
  },
  {
    section: "when bough asks",
    keys: [
      ["1-9 · t", "pick · type your own"],
      ["enter esc", "send · decline"],
    ],
  },
  {
    section: "won't do",
    limits: true,
    keys: [
      ["", "^c ^c quits; subagents keep running"],
      ["", "! = your cwd, unsandboxed, 30s cap"],
      ["", "subagent edits wait on its branch"],
      ["", "changes land only when you apply"],
    ],
  },
  {
    section: "not bound",
    unavailable: true,
    keys: [
      ["^r", "use ^s · or ! then type"],
      ["^y", "↑ restores a cleared draft"],
      ["^z", "no suspend · ^c ^c quits"],
      ["⌥d", "use ^k"],
    ],
  },
];

/** Question-hold card hints (the card replaces the composer, so it carries its own keys). */
export const ASK_OPTIONS_HINT = "1-9 choose · t type an answer · esc decline";
export const ASK_TYPING_HINT = "enter send · esc decline";
export const ASK_TYPING_BACK_HINT = "enter send · esc back to options";

export type UiMode = "chat" | "approval" | "new" | "help" | "panel";
