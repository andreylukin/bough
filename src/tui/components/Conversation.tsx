// Message rendering — the TUI translation of web/src/components/Conversation.tsx:
// prose/reasoning blocks with consecutive tool parts folded into one-line groups.
import { Box, Text } from "ink";
import type { Message, Part, Role } from "../../schema/parts.ts";
import { clip, md, outputText, segmentParts, toolSummary } from "../format.ts";

const ROLES: Record<Role, { label: string; color?: string; dim?: boolean }> = {
  user: { label: "you", color: "cyan" },
  supervisor: { label: "bough", color: "green" },
  worker: { label: "worker", dim: true },
  system: { label: "system", color: "yellow" },
};

export function Divider({ label }: { label: string }) {
  return (
    <Box marginTop={1}>
      <Text dimColor>── {label} ──</Text>
    </Box>
  );
}

function ToolGroup({ parts, expanded }: { parts: Part[]; expanded: boolean }) {
  const { calls, results, running, verdict, hasError } = toolSummary(parts);
  if (calls.length === 0) return null;
  return (
    <Box flexDirection="column">
      <Text>
        <Text dimColor>
          {expanded ? "▾" : "▸"} {calls.length} tool {calls.length === 1 ? "call" : "calls"}{"  "}
          {calls.map((c) => c.name).join(" · ")}
        </Text>
        {verdict ? <Text color={verdict.ok ? "green" : "yellow"}>{"  "}{verdict.text}</Text> : null}
        {hasError && !verdict ? <Text color="red">{"  "}✗ error</Text> : null}
        {running ? <Text color="yellow">{"  "}⚙ {running.name}…</Text> : null}
      </Text>
      {expanded &&
        calls.map((call) => {
          const res = results.get(call.id);
          // A code-mode call carries the program in `code` — show it as the input.
          const raw = call.input as Record<string, unknown> | null | undefined;
          const code = raw && typeof raw.code === "string" ? raw.code : null;
          const input = code ?? JSON.stringify(call.input);
          return (
            <Box key={call.id} flexDirection="column" marginLeft={2}>
              <Text>
                <Text color="green">◇</Text> {call.name} {res
                  ? (
                    <Text color={res.isError ? "red" : "green"}>
                      {res.isError ? "✗ error" : "✓ done"}
                    </Text>
                  )
                  : <Text color="yellow">⚙ running</Text>}
              </Text>
              {input ? <Text dimColor wrap="wrap">{clip(input, 300)}</Text> : null}
              {res && outputText(res) !== ""
                ? (
                  <Text color={res.isError ? "red" : undefined} dimColor={!res.isError} wrap="wrap">
                    {clip(outputText(res), 500)}
                  </Text>
                )
                : null}
            </Box>
          );
        })}
    </Box>
  );
}

export function MessageView(
  { msg, streaming, expandTools = false }: {
    msg: Message;
    streaming?: string;
    /** Expand tool groups (live/pending messages only — Static content is immutable). */
    expandTools?: boolean;
  },
) {
  const role = ROLES[msg.role];
  const segs = segmentParts(msg.parts);
  return (
    <Box flexDirection="column" marginTop={1}>
      <Text color={role.color} dimColor={role.dim} bold>
        {role.label}
      </Text>
      {segs.map((s, i) =>
        s.kind === "text"
          // Markdown styling only on finalized prose; reasoning stays uniformly dim.
          ? <Text key={i} wrap="wrap">{md(s.text)}</Text>
          : s.kind === "reasoning"
          ? <Text key={i} dimColor wrap="wrap">{s.text}</Text>
          : <ToolGroup key={i} parts={s.parts} expanded={expandTools} />
      )}
      {streaming ? <Text wrap="wrap">{md(streaming)}▌</Text> : null}
    </Box>
  );
}
