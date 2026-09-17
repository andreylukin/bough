/**
 * Every keyboard shortcut, in one table: the ? sheet and the Overview's
 * hints both read it, so neither can promise a key the other forgot.
 * "Mod" is ⌘ on a Mac and Ctrl+ elsewhere.
 */
export interface Binding { section: "Global" | "Session" | "Composer"; keys: string[]; label: string; note?: string; overview?: boolean }

// keys are alternatives ("or"); a space inside one ("↑ ↓") is a pair shown side by side.
export const BINDINGS: Binding[] = [
  { section: "Global", keys: ["ModK"], label: "Search or start", overview: true },
  { section: "Global", keys: ["/"], label: "Filter the list", overview: true },
  { section: "Global", keys: ["ModB"], label: "Toggle sidebar", overview: true },
  { section: "Global", keys: ["ModP"], label: "Switch session", note: "↵ reopens the last one" },
  { section: "Global", keys: ["AltN"], label: "New session in a known folder" },
  { section: "Global", keys: ["AltI"], label: "Focus the composer" },
  { section: "Global", keys: ["?"], label: "Show keyboard shortcuts", note: "also in an empty composer" },
  { section: "Global", keys: ["F6", "Shift+F6"], label: "Next or previous region" },
  { section: "Global", keys: ["↑ ↓", "j k"], label: "Move in the sidebar" },
  { section: "Global", keys: ["← →"], label: "Fold or unfold in the sidebar" },
  { section: "Global", keys: ["↵"], label: "Open the focused row" },
  { section: "Session", keys: ["Home", "End"], label: "Top or latest of the transcript" },
  { section: "Session", keys: ["ModEnd"], label: "Jump to latest" },
  { section: "Session", keys: ["Esc"], label: "Close Context or Changes" },
  { section: "Composer", keys: ["↵"], label: "Send, or steer a running turn" },
  { section: "Composer", keys: ["Mod↵"], label: "Queue behind a running turn" },
  { section: "Composer", keys: ["Shift+↵"], label: "Newline" },
  { section: "Composer", keys: ["Esc"], label: "Stop a running turn" },
  { section: "Composer", keys: ["/", "@"], label: "Commands or files" },
];

export const keyText = (k: string, mod: string) => k.replace(/^Mod/, mod).replace(/^Alt/, mod === "\u2318" ? "\u2325" : "Alt+");

/** One chip per key: "ModEnd" is ["⌘", "End"] on a Mac, ["Ctrl", "End"] elsewhere. */
export function keyChips(k: string, mac: boolean): string[] {
  const m = /^(Mod|Alt|Shift\+)(.+)$/.exec(k);
  if (!m) return [k];
  const mods = { Mod: mac ? "\u2318" : "Ctrl", Alt: mac ? "\u2325" : "Alt", "Shift+": mac ? "\u21e7" : "Shift" } as Record<string, string>;
  return [mods[m[1]], m[2]];
}

/** The Overview's empty-state hints. */
export function overviewKeys(mod: string): { key: string; label: string }[] {
  return BINDINGS.filter((b) => b.overview).map((b) => ({ key: keyText(b.keys[0], mod), label: b.label }));
}

/** True when a key event lands where someone is typing: single-key shortcuts leave it alone. */
export function isTypingTarget(t: EventTarget | null): boolean {
  const el = t as HTMLElement | null;
  return Boolean(el && (el.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName ?? "")));
}

type KeyLike = { key: string; code?: string; metaKey: boolean; ctrlKey: boolean; altKey: boolean; shiftKey?: boolean; target: EventTarget | null };

/** ⌘K anywhere; Ctrl+K too, except in a Mac text field, where it is kill-line. */
export function paletteKeyOpens(e: KeyLike, mac: boolean): boolean {
  if (!(e.metaKey || e.ctrlKey) || e.key.toLowerCase() !== "k") return false;
  return !(mac && e.ctrlKey && !e.metaKey && isTypingTarget(e.target));
}

/** ? opens the shortcut sheet, never while typing; an empty composer has nothing to type it into yet. */
export function sheetKey(e: KeyLike): boolean {
  if (e.key !== "?" || e.metaKey || e.ctrlKey || e.altKey) return false;
  const el = e.target as HTMLTextAreaElement | null;
  return !isTypingTarget(el) || (el?.id === "composer" && !el.value);
}

/** ⌘P (Ctrl+P): the recent-session switcher, from anywhere, the browser's print included. */
export function switchKey(e: KeyLike, mac = false): boolean {
  if (!(e.metaKey || e.ctrlKey) || e.altKey || e.shiftKey || e.key.toLowerCase() !== "p") return false;
  return !(mac && e.ctrlKey && !e.metaKey && isTypingTarget(e.target));
}

/** Option+letter by physical key: on a Mac e.key is a symbol (˜, ˆ). */
const altCode = (e: KeyLike, code: string) => e.altKey && !e.metaKey && !e.ctrlKey && !e.shiftKey && e.code === code;

/** ⌥N: new session, the palette offering the folders you work in. */
export const newSessionKey = (e: KeyLike) => altCode(e, "KeyN");

/** ⌥I: into the composer. */
export const focusComposerKey = (e: KeyLike) => altCode(e, "KeyI");

/** j and k are the sidebar's down and up. */
export function treeKey(key: string): string {
  return key === "j" ? "ArrowDown" : key === "k" ? "ArrowUp" : key;
}

export const isMac = () => typeof navigator !== "undefined" && /Mac|iP/.test(navigator.platform);
