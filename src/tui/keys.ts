// Keybindings as data: KEYMAP drives the `?` help overlay — the single place
// keys are documented (the status bar shows only session info + "? help").
export const KEYMAP: { section: string; keys: [string, string][] }[] = [
  {
    section: "chat",
    keys: [
      ["enter", "send — steers a running turn"],
      ["⌥enter", "queue for after this turn"],
      ["esc", "interrupt the turn (also during a net hold)"],
      ["esc esc", "clear the draft · rewind when empty"],
      ["↑/↓", "recall sent messages"],
      ["^s", "search the conversation"],
      ["! cmd", "run in a local shell · ↑/↓ picks history"],
      ["/", "skill autocomplete · @ workspace files"],
      ["tab", "accept the gray autocomplete preview"],
      ["^e", "expand/collapse all tool calls"],
      ["^j", "insert a newline"],
      ["^w ^k ^u", "delete word · to line end · all"],
      ["?", "this help"],
    ],
  },
  {
    section: "mouse",
    keys: [
      ["click ▸", "expand/collapse that fold"],
      ["right-click", "copy that section"],
      ["wheel", "scroll (also pgup/pgdn)"],
    ],
  },
  {
    section: "panel — ^t toggle · tab cycle · esc back",
    keys: [
      ["^p ^f ^d ^o", "sessions · conversation · changes · model"],
      ["↑↓ or j/k", "move · enter acts"],
    ],
  },
  {
    section: "sessions tab",
    keys: [
      ["n", "new session"],
      ["/", "filter · g/G first/last"],
      ["^x", "archive — ^x again confirms · recoverable"],
      ["x", "deprecate (hide) a branch"],
      ["h / u", "show hidden incl. archived · restore archived"],
    ],
  },
  {
    section: "conversation tab",
    keys: [
      ["enter", "rewind to a turn · open a branch"],
      ["v", "select a range → compact/extract/move/delete"],
      ["s", "label sections by topic"],
      ["C", "compact the whole session"],
      ["glyphs", "● root ⑂ fork ◆ subagent ≣ compacted ◇ tool"],
    ],
  },
  {
    section: "changes tab",
    keys: [
      ["a / A / R", "apply file · apply all · revert all"],
      ["j/k", "scroll the diff"],
    ],
  },
  {
    section: "model tab",
    keys: [["enter", "set model/effort · edit API keys"]],
  },
  {
    section: "net · mcp · skills · theme tabs",
    keys: [
      ["g / y", "net: feed scope · yolo mode"],
      ["c e r a", "mcp: connect · enable · restart · auth"],
      ["↑↓", "theme: applies live"],
    ],
  },
  {
    section: "net hold",
    keys: [
      ["a / A / d", "allow once · allow for the session · deny"],
      ["v", "show request details"],
    ],
  },
  {
    section: "question hold",
    keys: [
      ["1-9", "choose an option"],
      ["t", "type a custom answer"],
      ["enter / esc", "send the typed answer · decline"],
    ],
  },
  {
    section: "global",
    keys: [["^c ^c", "quit"]],
  },
];

/** Shown inside the net-hold card (it replaces the composer, so it carries its own keys). */
export const APPROVAL_HINT = "a allow once · A allow session · d deny · v details";

/** Question-hold card hints (same convention: the card carries its own keys). */
export const ASK_OPTIONS_HINT = "1-9 choose · t type an answer · esc decline";
export const ASK_TYPING_HINT = "enter send · esc decline";
export const ASK_TYPING_BACK_HINT = "enter send · esc back to options";

export type UiMode = "chat" | "approval" | "new" | "help" | "panel";
