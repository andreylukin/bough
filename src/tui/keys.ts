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
      ["! cmd", "run a local shell command"],
      ["/", "skill autocomplete · @ workspace files"],
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
    section: "panel — ^t toggles · tab cycles · esc backs out",
    keys: [
      ["^p ^f ^d ^o", "jump: sessions · conversation · changes · model"],
      ["↑↓ or j/k", "move · enter acts"],
    ],
  },
  {
    section: "sessions tab",
    keys: [
      ["n", "new session"],
      ["/", "filter · g/G first/last"],
      ["^x / x / h", "archive · deprecate · show hidden"],
    ],
  },
  {
    section: "conversation tab",
    keys: [
      ["enter", "rewind to a turn · open a branch"],
      ["v", "select a range → c compact · e extract · m move · d delete"],
      ["s", "label sections by topic"],
      ["C", "compact the whole session"],
      ["glyphs", "● root · ⑂ fork · ◆ subagent · ≣ compacted · ◇ tool step"],
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
    section: "net · mcp · theme tabs",
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
    section: "global",
    keys: [["^c ^c", "quit"]],
  },
];

/** Shown inside the net-hold card (it replaces the composer, so it carries its own keys). */
export const APPROVAL_HINT = "a allow once · A allow session · d deny · v details";

export type UiMode = "chat" | "approval" | "new" | "help" | "panel";
