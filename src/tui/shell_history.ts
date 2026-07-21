/**
 * The `!` composer prefix's backwards-fzf corpus: commands previously run via
 * `!` in bough (persisted in tui.json), seeded with the tail of the user's own
 * shell history files so the search is useful from the first session. Read-only
 * on the shell files; best-effort everywhere — no history is just an empty list.
 */
import { loadShellHistory } from "./state.ts";

/** zsh EXTENDED_HISTORY lines look like `: 1700000000:0;git status`. */
const ZSH_EXTENDED = /^: \d+:\d+;/;

/** Parse a shell history file's text into commands, oldest first. */
export function parseShellHistory(text: string): string[] {
  const out: string[] = [];
  for (const raw of text.split("\n")) {
    const line = raw.replace(ZSH_EXTENDED, "").replace(/\\$/, "").trim();
    if (line) out.push(line);
  }
  return out;
}

const SEED_TAIL = 500; // commands kept per source file

/**
 * The merged corpus, most recent LAST, deduped keeping the latest occurrence.
 * bough's own `!` history outranks the seeded shell files (proven use here beats
 * general terminal habits), so it comes after them in the list.
 */
export function shellHistoryCorpus(): string[] {
  const home = Deno.env.get("HOME");
  const seeded: string[] = [];
  for (const file of [".bash_history", ".zsh_history"]) {
    try {
      const text = Deno.readTextFileSync(`${home}/${file}`);
      seeded.push(...parseShellHistory(text).slice(-SEED_TAIL));
    } catch {
      // absent/unreadable — skip
    }
  }
  const merged = [...seeded, ...loadShellHistory()];
  // Dedupe keeping the LAST occurrence: walk backwards, then restore order.
  const seen = new Set<string>();
  const out: string[] = [];
  for (let i = merged.length - 1; i >= 0; i--) {
    if (seen.has(merged[i])) continue;
    seen.add(merged[i]);
    out.push(merged[i]);
  }
  return out.reverse();
}
