import { Box, Text } from "ink";
import type { BoughConfig } from "../api.ts";

export interface ModelEntry {
  kind: "model" | "worker";
  id: string;
  label: string;
}

export function modelEntries(cfg: BoughConfig): ModelEntry[] {
  return [
    ...cfg.models.map((m) => ({ kind: "model" as const, id: m.id, label: m.label })),
    ...cfg.workerOptions.map((w) => ({ kind: "worker" as const, id: w.id, label: w.label })),
  ];
}

// Model + worker switcher over GET/PATCH /config. One list, two sections; the
// active entry in each carries the dot.
export function ModelPicker(
  { cfg, entries, selected }: { cfg: BoughConfig; entries: ModelEntry[]; selected: number },
) {
  let lastKind: string | null = null;
  return (
    <Box flexDirection="column" borderStyle="round" paddingX={1}>
      {entries.map((e, i) => {
        const header = e.kind !== lastKind;
        lastKind = e.kind;
        const active = e.kind === "model" ? cfg.model === e.id : cfg.worker === e.id;
        return (
          <Box key={`${e.kind}:${e.id}`} flexDirection="column">
            {header && <Text bold>{e.kind === "model" ? "model" : "worker"}</Text>}
            <Text inverse={i === selected} wrap="truncate">
              <Text color="green">{active ? "●" : " "}</Text> {e.label}
              <Text dimColor>{"  "}{e.id}</Text>
            </Text>
          </Box>
        );
      })}
    </Box>
  );
}
