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
      ["right-click", "copy that section's text (message, reasoning, tool calls)"],
      ["wheel / pgup/pgdn", "scroll the conversation"],
      ["^e", "expand/collapse all tool calls (when the input is empty)"],
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
      ["^k", "compact this session onto a summary branch (when the input is empty)"],
      ["/handoff <goal>", "draft a fresh conversation from this thread, focused on the goal"],
      ["glyphs", "● root · ⑂ fork · ◆ subagent · ≣ compacted · ◇ tool step"],
    ],
  },
  {
    section: "work",
    keys: [
      ["^d", "changes review (↑↓ file · a apply · A apply all · R revert)"],
      ["^o", "model + worker picker"],
      ["^t", "panels: net / mcp / skills / theme (tab cycles; theme ↑↓ applies live)"],
    ],
  },
  {
    section: "net hold",
    keys: [
      ["a", "allow once"],
      ["A", "allow for the session"],
      ["d", "deny"],
      ["v", "toggle request details (headers · body, credentials redacted)"],
    ],
  },
  {
    section: "global",
    keys: [["^c ^c", "quit (double ctrl+c)"]],
  },
];

export const HINTS = {
  chat: "enter send · esc stop · ^p sessions · ^n new · ^t panels · ? help · ^c^c quit",
  approval: "a allow once · A allow session · d deny · v details · ^p sessions",
  picker: "j/k move · / filter · enter open · ^t new · ^x archive · esc back",
  new: "type dir query · ↑↓ pick · enter create · esc cancel",
  fork: "↑↓ move · enter branch here / open · esc close",
  diff: "↑↓ file · j/k scroll · a apply file · A apply all · R revert all · esc close",
  model: "↑↓ move · enter set · esc close",
  panel: "tab: cycle · ↑↓ move · enter select · x deprecate · h show hidden · esc close",
  help: "any key closes",
} as const;

export type UiMode = keyof typeof HINTS;
