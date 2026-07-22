import { palette } from "../theme.ts";
import { Box } from "ink";
import { Text } from "./Text.tsx";
import { SelRow } from "./SelRow.tsx";
import type { BoughConfig, KeyProvider } from "../api.ts";
import { windowAround } from "../format.ts";

interface ModelEntry {
  kind: "model" | "effort" | "worker" | "key";
  id: string;
  label: string;
}

const KEY_PROVIDERS: KeyProvider[] = ["anthropic", "openrouter", "openai"];

// "default" leaves the request untouched (the provider decides); the rest map to
// the API's output_config.effort levels (adaptive thinking on, summaries shown).
const EFFORT_LABELS: Record<string, string> = {
  default: "adaptive (provider default)",
  low: "low — quick, minimal thinking",
  medium: "medium — balanced",
  high: "high — thorough",
  xhigh: "xhigh — deep (coding/agentic sweet spot)",
  max: "max — correctness over cost",
};

export function modelEntries(cfg: BoughConfig): ModelEntry[] {
  return [
    ...cfg.models.map((m) => ({ kind: "model" as const, id: m.id, label: m.label })),
    ...["default", ...(cfg.efforts ?? [])].map((e) => ({
      kind: "effort" as const,
      id: e,
      label: EFFORT_LABELS[e] ?? e,
    })),
    ...cfg.workerOptions.map((w) => ({ kind: "worker" as const, id: w.id, label: w.label })),
    ...KEY_PROVIDERS.map((p) => ({
      kind: "key" as const,
      id: p,
      label: `${p} API key`,
    })),
  ];
}

const SECTION: Record<ModelEntry["kind"], string> = {
  model: "model",
  effort: "thinking depth",
  worker: "worker",
  key: "API keys",
};

// Model + worker switcher and API-key setup over GET/PATCH /config + PUT
// /config/keys. One list, three sections; the active entry carries the dot,
// key rows show set/missing. Selecting a key row opens a masked input.
// Content-only — the unified panel container owns the border + tab bar.
export function ModelPicker(
  { cfg, entries, selected, keyInput, sessionModel, sessionEffort, rows }: {
    cfg: BoughConfig;
    entries: ModelEntry[];
    selected: number;
    /** Non-null while typing a key for the selected provider (masked). */
    keyInput: string | null;
    /** The open session's pinned model, if any — the dot marks what THIS session
     * runs on (the global default when unpinned). */
    sessionModel?: string | null;
    /** The open session's pinned thinking depth (same semantics as the model). */
    sessionEffort?: string | null;
    /** Terminal rows — the list windows itself around the selection so it never
     * overflows the panel (Ink clips an overflowing background Box by merging
     * rows into garbage: overlapped labels at 80x26). */
    rows: number;
  },
) {
  const effectiveModel = sessionModel ?? cfg.model;
  const effectiveEffort = sessionEffort ?? cfg.effort ?? "";
  // Flat display list (section headers interleaved with entries), then a window
  // around the selection: panel chrome (border, tab bar, margin, status bar,
  // message line) plus the two ↑/↓ overflow markers ≈ 9 rows.
  const display: ({ header: string } | { e: ModelEntry; i: number })[] = [];
  let lastKind: string | null = null;
  entries.forEach((e, i) => {
    if (e.kind !== lastKind) display.push({ header: SECTION[e.kind] });
    lastKind = e.kind;
    display.push({ e, i });
  });
  const max = Math.max(3, rows - 9);
  const selAt = Math.max(0, display.findIndex((d) => "i" in d && d.i === selected));
  const { start, end } = windowAround(selAt, display.length, max);
  const win = display.slice(start, end);
  return (
    <Box flexDirection="column" marginTop={1}>
      {start > 0 ? <Text dimColor>↑ {start} more</Text> : null}
      {win.map((d) => {
        if ("header" in d) return <Text key={`h-${d.header}`} bold>{d.header}</Text>;
        const { e, i } = d;
        const active = e.kind === "model"
          ? effectiveModel === e.id
          : e.kind === "effort"
          ? (effectiveEffort || "default") === e.id
          : e.kind === "worker"
          ? cfg.worker === e.id
          : !!cfg.keys?.[e.id as KeyProvider];
        const editing = e.kind === "key" && i === selected && keyInput !== null;
        const sel = i === selected && !editing;
        return (
          <Box key={`${e.kind}:${e.id}`} flexDirection="column">
            {/* The active dot drops its color under selection: an inverse
                colored fg reads as a colored bg speck inside the light bar. */}
            <SelRow sel={sel}>
              <Text color={sel ? undefined : palette.accent}>{active ? "●" : " "}</Text> {e.label}
              {editing
                ? (
                  <Text>
                    {"  "}
                    <Text dimColor>paste key:</Text> {"•".repeat(Math.min(keyInput.length, 40))}
                    <Text inverse>{" "}</Text>
                  </Text>
                )
                : e.kind === "key"
                ? (
                  <Text dimColor>
                    {"  "}
                    {active ? "set · enter replaces · d deletes" : "not set · enter adds"}
                  </Text>
                )
                : <Text dimColor>{"  "}{e.id}</Text>}
            </SelRow>
          </Box>
        );
      })}
      {end < display.length
        ? <Text dimColor>↓ {display.length - end} more</Text>
        : null}
    </Box>
  );
}
