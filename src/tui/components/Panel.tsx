// The tabbed side-panel: net (Claw Patrol feed + policy), mcp (server registry),
// skills. Mirrors the web RightRail's tabs; App owns the data and key handling.
import { Box, Text } from "ink";
import type { NetRequest } from "../../schema/parts.ts";
import type { McpStatus, NetConfig, NetStatus, SkillInfo } from "../api.ts";
import { relTime } from "../format.ts";

export type PanelTab = "net" | "mcp" | "skills";
export const PANEL_TABS: PanelTab[] = ["net", "mcp", "skills"];

function Tabs({ tab }: { tab: PanelTab }) {
  return (
    <Text>
      {PANEL_TABS.map((t, i) => (
        <Text key={t} bold={t === tab} dimColor={t !== tab}>
          {i > 0 ? "  " : ""}
          {t}
        </Text>
      ))}
    </Text>
  );
}

const VERDICT_COLOR: Record<NetRequest["verdict"], string> = {
  allowed: "green",
  denied: "red",
  pending: "yellow",
};

function NetTab(
  { status, policy, feed, rows }: {
    status: NetStatus | null;
    policy: NetConfig | null;
    feed: NetRequest[];
    rows: number;
  },
) {
  if (!status) return <Text dimColor>loading…</Text>;
  const yolo = policy?.mode === "yolo";
  const visible = feed.slice(0, Math.max(3, rows - 12));
  return (
    <Box flexDirection="column">
      <Text>
        {status.enabled
          ? <Text color={yolo ? "red" : "green"}>{yolo ? "⚠ YOLO (log-only)" : "● gating"}</Text>
          : <Text dimColor>○ off</Text>}
        {policy
          ? (
            <Text dimColor>
              {"  "}mode {policy.mode}
              {policy.prevMode ? ` (was ${policy.prevMode})` : ""} · miss {policy.hostMiss} ·{" "}
              {policy.allowHosts.length} allow / {policy.denyHosts.length} deny hosts ·{" "}
              {status.listeners} listener{status.listeners === 1 ? "" : "s"}
            </Text>
          )
          : null}
      </Text>
      {status.caTrusted === false
        ? <Text color="yellow" wrap="truncate">CA untrusted — {status.caTrustCommand}</Text>
        : null}
      <Box marginTop={1} flexDirection="column">
        {visible.length === 0
          ? <Text dimColor>no gated requests yet</Text>
          : visible.map((r) => (
            <Text key={r.id} wrap="truncate">
              <Text color={VERDICT_COLOR[r.verdict]}>
                {r.verdict === "allowed" ? "✓" : r.verdict === "denied" ? "✗" : "⏸"}
              </Text>{" "}
              {r.verb ? <Text>{r.verb}{" "}</Text> : null}
              <Text bold>{r.host}</Text>
              <Text dimColor>{"  "}{r.action}{"  "}{relTime(r.ts)} ago</Text>
            </Text>
          ))}
      </Box>
    </Box>
  );
}

function McpTab(
  { mcp, selected, msg }: { mcp: McpStatus | null; selected: number; msg: string | null },
) {
  if (!mcp) return <Text dimColor>loading…</Text>;
  const names = Object.keys(mcp.registry.servers).sort();
  if (names.length === 0) return <Text dimColor>no MCP servers configured — see /mcp</Text>;
  return (
    <Box flexDirection="column">
      {names.map((name, i) => {
        const active = mcp.active.includes(name);
        const conn = mcp.connections.find((c) => c.server === name);
        const auth = mcp.auth[name];
        const entry = mcp.registry.servers[name];
        return (
          <Text key={name} inverse={i === selected} wrap="truncate">
            <Text color={conn?.alive ? "green" : active ? "yellow" : undefined} dimColor={!active}>
              {conn?.alive ? "●" : active ? "◐" : "○"}
            </Text>{" "}
            {name}
            <Text dimColor>
              {"  "}
              {active ? "on" : "off"}
              {conn?.alive ? ` · ${conn.toolCount} tools` : ""}
              {auth ? (auth.authorized ? " · authed" : " · needs auth") : ""}{"  "}
              {entry.url ?? entry.command ?? ""}
            </Text>
          </Text>
        );
      })}
      {msg ? <Text color="yellow" wrap="wrap">{msg}</Text> : null}
    </Box>
  );
}

function SkillsTab({ skills, rows }: { skills: SkillInfo[] | null; rows: number }) {
  if (!skills) return <Text dimColor>loading…</Text>;
  if (skills.length === 0) return <Text dimColor>no skills installed</Text>;
  return (
    <Box flexDirection="column">
      {skills.slice(0, Math.max(3, rows - 8)).map((s) => (
        <Text key={s.name} wrap="truncate">
          <Text bold>{s.name}</Text>
          <Text dimColor>{"  "}{s.description}</Text>
        </Text>
      ))}
      {skills.length > rows - 8 ? <Text dimColor>… {skills.length - (rows - 8)} more</Text> : null}
    </Box>
  );
}

export function Panel(
  { tab, status, policy, feed, mcp, mcpSel, mcpMsg, skills, rows }: {
    tab: PanelTab;
    status: NetStatus | null;
    policy: NetConfig | null;
    feed: NetRequest[];
    mcp: McpStatus | null;
    mcpSel: number;
    mcpMsg: string | null;
    skills: SkillInfo[] | null;
    rows: number;
  },
) {
  return (
    <Box flexDirection="column" borderStyle="round" paddingX={1}>
      <Tabs tab={tab} />
      <Box marginTop={1}>
        {tab === "net"
          ? <NetTab status={status} policy={policy} feed={feed} rows={rows} />
          : tab === "mcp"
          ? <McpTab mcp={mcp} selected={mcpSel} msg={mcpMsg} />
          : <SkillsTab skills={skills} rows={rows} />}
      </Box>
    </Box>
  );
}
