// Keybindings as data: KEYMAP drives the `?` help overlay, HINTS the status bar.
// Kept together so the bindings and their docs can't drift apart.

export const KEYMAP: { section: string; keys: [string, string][] }[] = [
  {
    section: "chat",
    keys: [
      ["enter", "send (steers a running turn; first send creates the session)"],
      ["⌥enter", "queue until the turn finishes"],
      ["esc", "interrupt the running turn"],
      ["↑/↓", "recall previously sent messages"],
      ["click ▸", "expand/collapse that tool group"],
      ["wheel / pgup/pgdn", "scroll the conversation"],
      ["^e", "expand/collapse all tool calls"],
      ["←/→ ⌥b/⌥f ^a/^e", "move the cursor (char · word · line ends)"],
      ["^w / ^k / ^u", "delete word · to end · all"],
      ["^j", "insert a newline (paste keeps its newlines)"],
      ["/ at start", "skill autocomplete · @ completes workspace files"],
      ["?", "this help (when the input is empty)"],
    ],
  },
  {
    section: "sessions",
    keys: [
      ["^p", "session picker (j/k move · / filter · enter open · ^t new · ^x archive)"],
      ["^n", "new session with workspace autocomplete"],
      ["^f", "conversation tree — branch at any turn / open a branch"],
      ["^b", "back to the parent (from a subagent branch)"],
      ["^k", "compact this session onto a summary branch"],
    ],
  },
  {
    section: "work",
    keys: [
      ["^d", "changes review (↑↓ file · j/k scroll · a apply · R revert)"],
      ["^o", "model + worker picker"],
      ["^t", "panels: net / mcp / skills (tab cycles)"],
    ],
  },
  {
    section: "net hold",
    keys: [
      ["a", "allow once"],
      ["A", "allow for the session"],
      ["d", "deny"],
    ],
  },
  {
    section: "global",
    keys: [["^c ^c", "quit (double ctrl+c)"]],
  },
];

export const HINTS = {
  chat: "enter send · esc stop · ^p sessions · ^n new · ^t panels · ? help · ^c^c quit",
  approval: "a allow once · A allow session · d deny · ^p sessions",
  picker: "j/k move · / filter · enter open · ^t new · ^x archive · esc back",
  new: "type dir query · ↑↓ pick · enter create · esc cancel",
  fork: "↑↓ move · enter branch here / open · esc close",
  diff: "↑↓ file · j/k scroll · a apply file · R revert all · esc close",
  model: "↑↓ move · enter set · esc close",
  panel: "tab: cycle · ↑↓ move · enter select · x deprecate · h show hidden · esc close",
  help: "any key closes",
} as const;

export type UiMode = keyof typeof HINTS;
