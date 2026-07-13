import { Box, Text } from "ink";
import type { NetRequest } from "../../schema/parts.ts";
import { HINTS } from "../keys.ts";

// The hold-and-ask card: a network request is parked on the wire waiting for a
// verdict. Replaces the composer while a hold is pending (holds wedge their turn,
// so releasing them takes priority over typing).
export function NetApproval({ req, count }: { req: NetRequest; count: number }) {
  return (
    <Box flexDirection="column" borderStyle="round" borderColor="yellow" paddingX={1}>
      <Text color="yellow" bold>
        ⏸ network hold{count > 1 ? ` (1 of ${count})` : ""}
      </Text>
      <Text>
        {req.verb ? `${req.verb} ` : ""}
        <Text bold>{req.host}</Text>
        <Text dimColor>{"  "}{req.action}</Text>
      </Text>
      {req.reason ? <Text dimColor wrap="wrap">{req.reason}</Text> : null}
      {req.annotation ? <Text dimColor wrap="wrap">{req.annotation}</Text> : null}
      <Text dimColor>{HINTS.approval}</Text>
    </Box>
  );
}
