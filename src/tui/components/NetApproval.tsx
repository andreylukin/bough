import { palette } from "../theme.ts";
import { Box, Text } from "ink";
import type { NetRequest } from "../../schema/parts.ts";
import { APPROVAL_HINT } from "../keys.ts";
import { clip } from "../format.ts";

// The hold-and-ask card: a network request is parked on the wire waiting for a
// verdict. Replaces the composer while a hold is pending (holds wedge their turn,
// so releasing them takes priority over typing).
export function NetApproval(
  { req, count, detail }: { req: NetRequest; count: number; detail: boolean },
) {
  const headers = detail && req.headers ? Object.entries(req.headers) : [];
  return (
    <Box
      flexDirection="column"
      borderStyle="round"
      backgroundColor={palette.panel}
      borderColor={palette.warn}
      paddingX={1}
    >
      <Text color={palette.warn} bold>
        ⏸ network hold{count > 1 ? ` (1 of ${count})` : ""}
      </Text>
      <Text>
        {req.verb ? `${req.verb} ` : ""}
        <Text bold>{req.host}</Text>
        {req.path && req.path !== "/" ? clip(req.path, 60) : ""}
        <Text dimColor>{"  "}{req.action}</Text>
      </Text>
      {req.reason ? <Text dimColor wrap="wrap">{req.reason}</Text> : null}
      {req.annotation ? <Text dimColor wrap="wrap">{req.annotation}</Text> : null}
      {detail
        ? (
          <Box flexDirection="column">
            {headers.length === 0 && !req.bodyPreview
              ? <Text dimColor>no header/body detail captured for this request</Text>
              : null}
            {headers.map(([k, v]) => (
              <Text key={k} wrap="truncate">
                <Text dimColor>{"  "}{k}:</Text> {clip(v, 70)}
              </Text>
            ))}
            {req.bodyPreview
              ? (
                <Text wrap="wrap">
                  <Text dimColor>{"  "}body:</Text> {clip(req.bodyPreview, 300)}
                </Text>
              )
              : null}
          </Box>
        )
        : null}
      <Text dimColor>{APPROVAL_HINT}</Text>
    </Box>
  );
}
