/**
 * The model tab: both tiers, one list.
 *
 * THE INVARIANT THIS HOLDS: **the picker chooses the frontier model AND the cheap
 * model.** Spec §12 names two tiers and says both are chosen here — the supervisor,
 * and the cheap model that powers auto titles, composer ghost text and live activity
 * blurbs. A picker that offered only the frontier tier would leave the tier that bills
 * on *every* round unreachable from the product, configurable only by editing state
 * the user cannot see. So `modelEntries` emits two model sections over the same
 * catalog, and `chooseEntry` routes the choice by the row's `tier` and nothing else.
 *
 * SECOND INVARIANT — **switching pins THIS session and moves the default for new
 * sessions, and touches no other existing session.** That sentence is spec §12
 * verbatim, and it is implemented as one pure function (`chooseEntry`) rather than as
 * two API calls a caller might make only one of. It writes `sessionModel` (the pin)
 * and `defaultModel` (what new sessions inherit); every other session keeps whatever
 * it was already pinned to, because nothing here can express a change to them. The
 * cheap tier has no per-session pin — it is one background model for the whole
 * install, so choosing one moves the default only.
 *
 * THIRD — **an id is a provider routing decision, so the catalog is injected.** Model
 * ids route by prefix (`openai:x` → OpenAI, `vendor/model` → OpenRouter, bare →
 * Anthropic — spec §12), and that table lives in `llm/client.ts` where the routing
 * does. This file takes `ModelRow[]` as a prop and imports the type only, so no
 * provider name is written outside `llm/`, no provider SDK is dragged into the TUI's
 * import graph, and a test drives the picker with three fixture rows.
 *
 * NOT PORTED: the API-keys section of `src/tui/components/ModelPicker.tsx`. Keys are
 * environment variables in this tree (`API_KEY_ENV` in `llm/client.ts`) and there is
 * no `/config/keys` route to write to, so a "paste key" row would be a control that
 * does nothing. When a keys route lands it belongs here as a fourth section.
 *
 * KNOWN GAP: there is no `/config` route yet either (`GET/PATCH /config` is not in
 * `server/app.ts` and `api.ts` has no method for it). This component is therefore
 * presentational over a `ModelConfig` the caller owns, and `chooseEntry` returns the
 * next config rather than performing a write. Reported rather than worked around.
 */
import { Box, Text } from "ink";
import type { ModelRow } from "../../llm/client.ts";
import type { Effort } from "../../types.ts";
import { clip, windowAround } from "../format.ts";
import { palette } from "../theme.ts";

// ---------------------------------------------------------------------------
// The config the picker edits
// ---------------------------------------------------------------------------

/** `"default"` leaves the request untouched — the provider decides. */
export type EffortChoice = "default" | Effort;

export const EFFORTS: readonly EffortChoice[] = [
  "default",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
];

const EFFORT_LABELS: Record<EffortChoice, string> = {
  default: "adaptive — the provider decides",
  low: "low — quick, minimal thinking",
  medium: "medium — balanced",
  high: "high — thorough",
  xhigh: "xhigh — deep (the agentic sweet spot)",
  max: "max — correctness over cost",
};

export interface ModelConfig {
  /** What a NEW session starts on. */
  defaultModel: string;
  /** THIS session's pin. `null` = it follows `defaultModel`. */
  sessionModel: string | null;
  /** The cheap tier: titles, ghost text, activity blurbs. One per install. */
  cheapModel: string | null;
  defaultEffort: EffortChoice;
  sessionEffort: EffortChoice | null;
}

/** What the open session actually runs on right now. */
export function effectiveModel(cfg: ModelConfig): string {
  return cfg.sessionModel ?? cfg.defaultModel;
}

export function effectiveEffort(cfg: ModelConfig): EffortChoice {
  return cfg.sessionEffort ?? cfg.defaultEffort;
}

// ---------------------------------------------------------------------------
// Entries (pure)
// ---------------------------------------------------------------------------

export type Tier = "frontier" | "cheap" | "effort";

interface EntryBase {
  label: string;
  /** Right-hand detail: the raw id, and which provider the prefix routes to. */
  detail: string;
}

/**
 * Discriminated on `tier` so an effort row's id is an `EffortChoice` and a model row's
 * is a free-form model id — the two are not interchangeable, and a flat `id: string`
 * let `chooseEntry` write "xhigh" into `defaultModel` without a complaint.
 */
export type ModelEntry =
  | (EntryBase & { tier: "frontier" | "cheap"; id: string })
  | (EntryBase & { tier: "effort"; id: EffortChoice });

export const SECTIONS: Record<Tier, { title: string; hint: string }> = {
  frontier: {
    title: "frontier model — the supervisor",
    hint: "pins this session and becomes the default for new ones; other sessions are left alone",
  },
  cheap: {
    title: "cheap model — titles, ghost text, activity",
    hint: "bills on every round, so it fails silently and never blocks a turn",
  },
  effort: { title: "thinking depth", hint: "not every model accepts one" },
};

/** The flat entry list: frontier catalog, cheap catalog, then the effort levels. */
export function modelEntries(
  catalog: readonly ModelRow[],
  cheapCatalog: readonly ModelRow[] = catalog,
): ModelEntry[] {
  return [
    ...catalog.map((m) => row("frontier", m)),
    ...cheapCatalog.map((m) => row("cheap", m)),
    ...EFFORTS.map((e) => ({
      tier: "effort" as const,
      id: e,
      label: EFFORT_LABELS[e],
      detail: e,
    })),
  ];
}

function row(tier: "frontier" | "cheap", m: ModelRow): ModelEntry {
  return { tier, id: m.id, label: m.label, detail: `${m.id}  ·  ${m.provider}` };
}

/** Whether an entry is the one currently in force for its tier. */
export function isActive(cfg: ModelConfig, e: ModelEntry): boolean {
  switch (e.tier) {
    case "frontier":
      return effectiveModel(cfg) === e.id;
    case "cheap":
      return cfg.cheapModel === e.id;
    case "effort":
      return effectiveEffort(cfg) === e.id;
  }
}

/**
 * Choosing a row. **This is spec §12 in code**: a frontier pick pins the open session
 * and moves the default for new sessions; nothing else moves. Pure — the caller sends
 * the resulting config to the server and re-renders from the response.
 */
export function chooseEntry(cfg: ModelConfig, e: ModelEntry): ModelConfig {
  switch (e.tier) {
    case "frontier":
      return { ...cfg, sessionModel: e.id, defaultModel: e.id };
    case "cheap":
      // No per-session pin: one background model for the whole install.
      return { ...cfg, cheapModel: e.id };
    case "effort":
      return { ...cfg, sessionEffort: e.id, defaultEffort: e.id };
  }
}

// ---------------------------------------------------------------------------
// Presentation
// ---------------------------------------------------------------------------

type DisplayRow = { header: Tier } | { entry: ModelEntry; index: number };

/** Section headers interleaved with entries — what the cursor window is cut from. */
export function displayRows(entries: readonly ModelEntry[]): DisplayRow[] {
  const out: DisplayRow[] = [];
  let last: Tier | null = null;
  entries.forEach((entry, index) => {
    if (entry.tier !== last) out.push({ header: entry.tier });
    last = entry.tier;
    out.push({ entry, index });
  });
  return out;
}

export interface ModelPickerProps {
  cfg: ModelConfig;
  entries: ModelEntry[];
  selected: number;
  rows: number;
  message?: string | null;
}

export function ModelPicker({ cfg, entries, selected, rows, message }: ModelPickerProps) {
  const display = displayRows(entries);
  // The list windows itself around the cursor: ink clips an overflowing background
  // box by merging rows into garbage rather than by scrolling.
  const height = Math.max(3, rows - 6);
  const cursorAt = Math.max(0, display.findIndex((d) => "entry" in d && d.index === selected));
  const { start, end } = windowAround(cursorAt, display.length, height);
  return (
    <Box flexDirection="column">
      {message ? <Text color={palette.warn} wrap="truncate">{message}</Text> : null}
      {start > 0 ? <Text dimColor>↑ {start} more</Text> : null}
      {display.slice(Math.max(0, start), end).map((d) => {
        if ("header" in d) {
          const section = SECTIONS[d.header];
          return (
            <Text key={`h-${d.header}`} wrap="truncate">
              <Text bold color={palette.accent}>{section.title}</Text>
              <Text dimColor>{"  "}{section.hint}</Text>
            </Text>
          );
        }
        const { entry, index } = d;
        const sel = index === selected;
        const active = isActive(cfg, entry);
        return (
          <Text key={`${entry.tier}:${entry.id}`} wrap="truncate">
            <Text color={sel ? palette.accent : undefined}>{sel ? "❯ " : "  "}</Text>
            <Text color={sel || !active ? undefined : palette.accent}>{active ? "●" : " "}</Text>
            <Text bold={sel}>{" "}{clip(entry.label, 38)}</Text>
            <Text dimColor>{"  "}{clip(entry.detail, 34)}</Text>
          </Text>
        );
      })}
      {end < display.length ? <Text dimColor>↓ {display.length - end} more</Text> : null}
      <Text dimColor wrap="truncate">↑↓ move · ⏎ choose</Text>
    </Box>
  );
}
