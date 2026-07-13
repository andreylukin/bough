import { Box, Text } from "ink";
import type { BoughConfig, KeyProvider } from "../api.ts";

export interface ModelEntry {
  kind: "model" | "worker" | "key";
  id: string;
  label: string;
}

export const KEY_PROVIDERS: KeyProvider[] = ["anthropic", "openrouter", "openai"];

export function modelEntries(cfg: BoughConfig): ModelEntry[] {
  return [
    ...cfg.models.map((m) => ({ kind: "model" as const, id: m.id, label: m.label })),
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
  worker: "worker",
  key: "API keys",
};

// Model + worker switcher and API-key setup over GET/PATCH /config + PUT
// /config/keys. One list, three sections; the active entry carries the dot,
// key rows show set/missing. Selecting a key row opens a masked input.
export function ModelPicker(
  { cfg, entries, selected, keyInput }: {
    cfg: BoughConfig;
    entries: ModelEntry[];
    selected: number;
    /** Non-null while typing a key for the selected provider (masked). */
    keyInput: string | null;
  },
) {
  let lastKind: string | null = null;
  return (
    <Box flexDirection="column" borderStyle="round" borderColor="gray" paddingX={1}>
      {entries.map((e, i) => {
        const header = e.kind !== lastKind;
        lastKind = e.kind;
        const active = e.kind === "model"
          ? cfg.model === e.id
          : e.kind === "worker"
          ? cfg.worker === e.id
          : !!cfg.keys?.[e.id as KeyProvider];
        const editing = e.kind === "key" && i === selected && keyInput !== null;
        return (
          <Box key={`${e.kind}:${e.id}`} flexDirection="column">
            {header && <Text bold>{SECTION[e.kind]}</Text>}
            <Text inverse={i === selected && !editing} wrap="truncate">
              <Text color="green">{active ? "●" : " "}</Text> {e.label}
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
                    {active ? "set · enter replaces" : "not set · enter adds"}
                  </Text>
                )
                : <Text dimColor>{"  "}{e.id}</Text>}
            </Text>
          </Box>
        );
      })}
    </Box>
  );
}
