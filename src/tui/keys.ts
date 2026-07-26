// Keybindings as data: KEYMAP drives the `?` help overlay — the single place
// keys are documented (the status bar shows only session info + "? help").
export const KEYMAP: { section: string; keys: [string, string][] }[] = [
  {
    section: "chat",
    keys: [
      ["enter", "send — steers a running turn"],
      ["⌥enter", "queue for after this turn"],
      ["esc", "interrupt a RUNNING turn (also during a question hold) · ^x stops the rest"],
      ["esc esc", "clear the draft · rewind when empty"],
      ["↑/↓", "recall sent messages"],
      ["↓ (empty)", "into the subagent rail · enter opens · esc back"],
      ["^s", "search the conversation"],
      ["! cmd", "run in a local shell · ↑/↓ picks history"],
      ["/", "skill autocomplete · @ workspace files"],
      ["tab", "accept the gray autocomplete preview"],
      ["^e", "expand/collapse all tool calls"],
      ["^j", "insert a newline"],
      ["^w ^k ^u", "delete word · to line end · all"],
      ["⌘⌫", "delete to line start"],
      ["⌘←/→", "jump to start/end of the line"],
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
    section: "panel — ^t toggle · tab cycle · these keys mean the same in EVERY tab",
    keys: [
      ["^p ^f ^d ^b ^o", "sessions · conversation · changes · jobs · model"],
      ["enter", "open / act on the selected row"],
      ["esc", "back out one level · then out of the panel"],
      ["x", "the destructive action here (all recoverable)"],
      ["/", "filter"],
      ["j/k or ↑↓", "move"],
    ],
  },
  {
    section: "sessions tab — x archives",
    keys: [
      ["n", "new session"],
      ["g/G", "first/last row"],
      ["x", "archive — x again confirms"],
      ["D", "deprecate (hide) a branch"],
      ["h / u", "show hidden incl. archived · restore archived"],
    ],
  },
  {
    section: "conversation tab — x deprecates a branch (or deletes a range)",
    keys: [
      ["enter", "rewind to a turn · open a branch"],
      ["v", "select a range → c compact · e extract · m copy to · x delete"],
      ["s", "label sections by topic"],
      ["C", "compact the whole session"],
      ["h", "show hidden branches"],
      ["glyphs", "● root ⑂ fork ◆ subagent ≣ compacted ◇ tool"],
    ],
  },
  {
    section: "changes tab — x reverts everything",
    keys: [
      ["enter", "apply the selected file"],
      ["A", "apply every listed file"],
      ["→ / ←", "focus the hunks pane (j/k scrolls it) · back to the list"],
    ],
  },
  {
    section: "jobs tab — ^b · background shells · x stops a job",
    keys: [
      ["enter", "open the job's full output"],
      ["j/k", "scroll that output"],
    ],
  },
  {
    section: "workflows tab — /workflows opens it · x stops the run (or the agent)",
    keys: [
      ["enter / →", "open a run · then an agent"],
      ["p", "pause/resume the run"],
      [
        "r",
        "rerun — edit its script file first to change it; unchanged agents replay from the journal",
      ],
      ["o", "open the selected agent's conversation"],
    ],
  },
  {
    section: "model tab — x removes the selected API key",
    keys: [["enter", "set model/effort · edit API keys"]],
  },
  {
    section: "mcp · skills · theme tabs",
    keys: [
      ["c e r a", "mcp: connect · enable · restart · auth"],
      ["↑↓", "theme: applies live"],
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
    keys: [
      ["^x ^x", "stop everything here — turn, subagents, background shells"],
      ["^c ^c", "quit — leaves subagents running on the server"],
    ],
  },
];

/** Question-hold card hints (the card replaces the composer, so it carries its own keys). */
export const ASK_OPTIONS_HINT = "1-9 choose · t type an answer · esc decline";
export const ASK_TYPING_HINT = "enter send · esc decline";
export const ASK_TYPING_BACK_HINT = "enter send · esc back to options";

export type UiMode = "chat" | "approval" | "new" | "help" | "panel";
