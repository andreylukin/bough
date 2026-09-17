/**
 * Every keyboard shortcut, in one table: the ? sheet and the Overview's
 * hints both read it, so neither can promise a key the other forgot.
 * "Mod" is ⌘ on a Mac and Ctrl+ elsewhere.
 */
export interface Binding { section: "Global" | "Session" | "Composer"; keys: string[]; label: string; overview?: boolean }

export const BINDINGS: Binding[] = [
  { section: "Global", keys: ["ModK"], label: "search or start", overview: true },
  { section: "Global", keys: ["/"], label: "filter the list", overview: true },
  { section: "Global", keys: ["ModB"], label: "sidebar", overview: true },
  { section: "Global", keys: ["?"], label: "keyboard shortcuts" },
  { section: "Global", keys: ["F6", "Shift+F6"], label: "next or previous region" },
  { section: "Global", keys: ["↑", "↓", "j", "k"], label: "move in the sidebar" },
  { section: "Global", keys: ["←", "→"], label: "fold or unfold in the sidebar" },
  { section: "Global", keys: ["↵"], label: "open the focused row" },
  { section: "Session", keys: ["Home", "End"], label: "top or latest of the transcript" },
  { section: "Session", keys: ["ModEnd"], label: "jump to latest" },
  { section: "Session", keys: ["Esc"], label: "close Context or Changes" },
  { section: "Composer", keys: ["↵"], label: "send, or steer a running turn" },
  { section: "Composer", keys: ["Mod↵"], label: "queue behind a running turn" },
  { section: "Composer", keys: ["Shift+↵"], label: "newline" },
  { section: "Composer", keys: ["Esc"], label: "stop a running turn" },
  { section: "Composer", keys: ["/", "@"], label: "commands, files" },
];

export const keyText = (k: string, mod: string) => k.replace(/^Mod/, mod);

/** The Overview's empty-state hints. */
export function overviewKeys(mod: string): { key: string; label: string }[] {
  return BINDINGS.filter((b) => b.overview).map((b) => ({ key: keyText(b.keys[0], mod), label: b.label }));
}

/** True when a key event lands where someone is typing: single-key shortcuts leave it alone. */
export function isTypingTarget(t: EventTarget | null): boolean {
  const el = t as HTMLElement | null;
  return Boolean(el && (el.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName ?? "")));
}

type KeyLike = { key: string; metaKey: boolean; ctrlKey: boolean; altKey: boolean; shiftKey?: boolean; target: EventTarget | null };

/** ⌘K anywhere; Ctrl+K too, except in a Mac text field, where it is kill-line. */
export function paletteKeyOpens(e: KeyLike, mac: boolean): boolean {
  if (!(e.metaKey || e.ctrlKey) || e.key.toLowerCase() !== "k") return false;
  return !(mac && e.ctrlKey && !e.metaKey && isTypingTarget(e.target));
}

/** ? opens the shortcut sheet, never while typing. */
export function sheetKey(e: KeyLike): boolean {
  return e.key === "?" && !e.metaKey && !e.ctrlKey && !e.altKey && !isTypingTarget(e.target);
}

/** j and k are the sidebar's down and up. */
export function treeKey(key: string): string {
  return key === "j" ? "ArrowDown" : key === "k" ? "ArrowUp" : key;
}

export const isMac = () => typeof navigator !== "undefined" && /Mac|iP/.test(navigator.platform);
