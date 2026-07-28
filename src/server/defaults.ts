/**
 * What a NEW conversation runs on, stored in `~/.bough/model.json`.
 *
 * THE BUG THIS EXISTS FOR: the model picker wrote a pin to the open session and
 * nothing else. `ctx.model` is read once from `BOUGH_MODEL` at server start and is
 * immutable for the life of the process, so the next conversation — and every
 * conversation after it — went back to the built-in default. Picking a model
 * appeared to work, survived exactly as long as the session you picked it in, and
 * then silently reverted. There was no route to write a default at all:
 * `/model-settings` was GET-only.
 *
 * A SIBLING OF `theme.ts`, deliberately and structurally. Both are "exactly one per
 * install, not per session", both are a preference rather than data, and both would
 * otherwise want a row in a schema whose own header says the table set is closed and
 * that a task needing a column should stop and ask. A JSON file beside the theme is
 * the precedent this codebase already set for this exact shape of state.
 *
 * FORGIVING ON READ, like the theme: a missing file is the ordinary state and not a
 * failure, and a file that cannot be parsed — or that a hand-edit filled with the
 * wrong types — degrades to "nothing pinned" rather than taking the server down on
 * the path that answers what model to use.
 */
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import { modelSettingsPath } from "../paths.ts";
import type { Effort } from "../types.ts";

/** `Effort` is a plain union, so the read guard carries its own value set. */
const EFFORTS: readonly string[] = ["low", "medium", "high", "xhigh", "max"];

export interface ModelDefaults {
  /** The frontier model a new conversation is created with. `null` = not pinned. */
  model: string | null;
  /** The thinking depth it starts at. `null` = let the provider decide. */
  effort: Effort | null;
}

export const NO_DEFAULTS: ModelDefaults = { model: null, effort: null };

/**
 * The stored defaults, or `NO_DEFAULTS` when nothing is pinned.
 *
 * `path` is injected by tests so nothing here touches a real `~/.bough` — the same
 * arrangement `loadTheme` uses, and for the same reason: a test that wrote the
 * developer's own home directory would be a test that changed their editor's model.
 */
export function loadDefaults(path: string = modelSettingsPath()): ModelDefaults {
  try {
    if (!existsSync(path)) return NO_DEFAULTS;
    const raw: unknown = JSON.parse(readFileSync(path, "utf8"));
    if (typeof raw !== "object" || raw === null) return NO_DEFAULTS;
    const { model, effort } = raw as { model?: unknown; effort?: unknown };
    return {
      model: typeof model === "string" && model.trim() ? model.trim() : null,
      effort: typeof effort === "string" && EFFORTS.includes(effort) ? effort as Effort : null,
    };
  } catch {
    return NO_DEFAULTS;
  }
}

/**
 * Persist the defaults. Creates `~/.bough` if this is the first write.
 *
 * Rebuilt rather than passed through, so the file holds exactly the two validated
 * keys and nothing a looser caller let ride along.
 */
export function saveDefaults(next: ModelDefaults, path: string = modelSettingsPath()): ModelDefaults {
  const clean: ModelDefaults = {
    model: next.model?.trim() ? next.model.trim() : null,
    effort: next.effort ?? null,
  };
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, JSON.stringify(clean, null, 2) + "\n");
  return clean;
}
