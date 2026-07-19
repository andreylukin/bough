// Keybindings as data: KEYMAP drives the `?` help overlay; HINTS and PANEL_HINTS
// drive the status bar. Kept together so the bindings and their docs can't drift.
import type { PanelTab } from "./components/Panel.tsx";

export const KEYMAP: { section: string; keys: [string, string][] }[] = [
  {
    section: "chat",
    keys: [
      ["enter", "send (steers a running turn; first send creates the session)"],
      ["⌥enter", "queue until the turn finishes"],
      ["esc", "interrupt the running turn (also during a net hold)"],
      ["esc esc", "clear the draft (↑ recalls it) · rewind via the conversation tab when empty"],
      ["↑/↓", "recall previously sent messages"],
      ["^s", "search the conversation (enter/↓ next · ↑ prev · esc close)"],
      ["! cmd", "run a local shell command in the workspace (esc dismisses output)"],
      ["/ at start", "skill autocomplete · @ completes workspace files"],
      ["^e", "expand/collapse all tool calls (when the input is empty)"],
      ["click ▸", "expand/collapse that tool group or thinking fold"],
      ["right-click", "copy that section's text (message, reasoning, tool calls)"],
      ["wheel / pgup/pgdn", "scroll the conversation"],
      ["←/→ ⌥b/⌥f ^a/^e", "move the cursor (char · word · line ends)"],
      ["^w / ^k / ^u", "delete word · to end · all"],
      ["^j", "insert a newline (paste keeps its newlines)"],
      ["?", "this help (when the input is empty)"],
    ],
  },
  {
    section: "panel — one surface, tab cycles its tabs, esc backs out",
    keys: [
      ["^t", "open/close the panel on its last tab"],
      ["^p ^f ^d ^o", "jump to a tab: sessions · conversation · changes · model"],
      ["↑↓ or j/k", "move · enter acts · esc backs out (chords work inside too)"],
    ],
  },
  {
    section: "sessions tab",
    keys: [
      ["enter", "open the selected session"],
      ["n", "new session (workspace autocomplete)"],
      ["/", "filter · g/G jump to first/last"],
      ["^x / x / h", "archive · deprecate · show hidden"],
    ],
  },
  {
    section: "conversation tab",
    keys: [
      ["enter", "rewind to a turn (its message back in the composer) / open a branch"],
      ["v", "select a range, then: c compact · e extract · m move · d delete"],
      ["s", "label sections by topic (enter on a header selects the section)"],
      ["C", "compact the whole session onto a summary branch"],
      ["glyphs", "● root · ⑂ fork · ◆ subagent · ≣ compacted · ◇ tool step"],
    ],
  },
  {
    section: "changes tab",
    keys: [
      ["↑/↓ · j/k", "select file · scroll the diff"],
      ["a / A / R", "apply file · apply all · revert all"],
    ],
  },
  {
    section: "net hold",
    keys: [
      ["a / A / d", "allow once · allow for the session · deny"],
      ["v", "toggle request details (headers · body, credentials redacted)"],
    ],
  },
  {
    section: "global",
    keys: [["^c ^c", "quit (double ctrl+c)"]],
  },
];

export const HINTS = {
  chat: "enter send · esc stop · ^s find · ^t panel · ^p sessions · ? help · ^c^c quit",
  approval: "a allow once · A allow session · d deny · v details",
  new: "type dir query · ↑↓ pick · enter create · esc back",
  help: "any key closes",
} as const;

/** Per-tab status-bar hints while the panel is open. */
export const PANEL_HINTS: Record<PanelTab, string> = {
  sessions: "↑↓ move · / filter · enter open · n new · ^x archive · esc back",
  conversation: "↑↓ move · enter rewind/open · v range then c/e/m/d · s sections · C compact · esc",
  changes: "↑↓ file · j/k scroll · a apply · A apply all · R revert all · esc back",
  model: "↑↓ move · enter set (key rows: enter edits) · esc back",
  net: "g scope · y yolo · tab next tab · esc back",
  mcp: "↑↓ move · c connect · e enable · r restart · a auth · esc back",
  skills: "tab next tab · esc back",
  theme: "↑↓ applies live · esc back",
};

export type UiMode = keyof typeof HINTS | "panel";
