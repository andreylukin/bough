import { Box, Text } from "ink";
import type { Message } from "../../schema/parts.ts";
import { clip } from "../format.ts";

// Preview line for a fork target: first prose, else the tool names.
function preview(m: Message): string {
  const text = m.parts.find((p) => p.type === "text" || p.type === "reasoning");
  if (text && "text" in text) return clip(text.text.split("\n")[0], 64);
  const tools = m.parts.filter((p) => p.type === "tool_call").map((p) => p.name);
  return tools.length ? `⚙ ${clip(tools.join(" · "), 60)}` : "(empty)";
}

// Pick the message to branch at (newest first). The server keeps history up to and
// including the picked message; inherited turns 400 with a message → notice.
export function ForkPicker(
  { messages, selected, rows }: { messages: Message[]; selected: number; rows: number },
) {
  const max = Math.max(3, rows - 7);
  const start = Math.max(0, Math.min(selected - Math.floor(max / 2), messages.length - max));
  const win = messages.slice(start, start + max);
  return (
    <Box flexDirection="column" borderStyle="round" borderColor="gray" paddingX={1}>
      <Text bold>fork at…</Text>
      {win.map((m, i) => (
        <Text key={m.id} inverse={start + i === selected} wrap="truncate">
          <Text color={m.role === "user" ? "cyan" : "green"}>{m.role.padEnd(10)}</Text>
          {preview(m)}
        </Text>
      ))}
      {messages.length === 0 && <Text dimColor>no messages to fork at</Text>}
    </Box>
  );
}
