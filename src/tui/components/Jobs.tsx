// The jobs tab: every background shell of the open session and its subagents,
// with the outcome stated and the full output one keypress away. Before this tab
// the only way to read a background job's output was to spend an LLM turn asking
// the agent to call bashOutput — and the only way to stop one was to ask it to
// call bashKill. App owns the data and key handling.
import { palette } from "../theme.ts";
import { Box } from "ink";
import { Text } from "./Text.tsx";
import type { JobRow } from "../api.ts";
import { SelRow } from "./SelRow.tsx";

function fmtElapsed(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${s % 60}s`;
}

/** Status label + its color, kept in one place so the list and the output header
 * never disagree about whether a job succeeded. */
export function jobStatus(job: JobRow): { label: string; color: string } {
  if (job.status === "running") return { label: "⋯ running", color: palette.warn };
  if (job.status === "killed") return { label: "✗ killed", color: palette.error };
  if (job.signal) return { label: `✗ ${job.signal}`, color: palette.error };
  return job.exitCode === 0
    ? { label: "✓ done", color: palette.accent }
    : { label: `✗ exit ${job.exitCode}`, color: palette.error };
}

/** The drill-in view: one job's whole retained buffer, scrolled by the parent. */
function JobOutput(
  { job, output, scroll, rows }: {
    job: JobRow;
    output: string | null;
    scroll: number;
    rows: number;
  },
) {
  const st = jobStatus(job);
  const height = Math.max(3, rows - 8);
  const lines = output === null ? [] : output.split("\n");
  const view = lines.slice(scroll, scroll + height);
  return (
    <Box flexDirection="column">
      <Text wrap="truncate">
        <Text bold>{job.id}</Text> <Text color={st.color}>{st.label}</Text>
        <Text dimColor>
          {"  "}
          {job.command}
        </Text>
      </Text>
      <Text dimColor wrap="truncate">
        {lines.length} lines · {fmtElapsed((job.endedAt ?? Date.now()) - job.startedAt)} · j/k
        scroll · esc back{job.status === "running" ? " · x stops it" : ""}
      </Text>
      <Box flexDirection="column" marginTop={1}>
        {output === null
          ? <Text dimColor>loading…</Text>
          : lines.length === 0
          ? <Text dimColor>(no output)</Text>
          : view.map((l, i) => (
            <Text key={scroll + i} wrap="truncate">
              {l || " "}
            </Text>
          ))}
      </Box>
      {lines.length > scroll + height
        ? <Text dimColor>↓ {lines.length - scroll - height} more</Text>
        : null}
    </Box>
  );
}

// Note: the panel container renders panelMsg beneath every tab but sessions/mcp,
// so this component must not draw it too.
export function Jobs(
  { jobs, sel, open, output, scroll, rows }: {
    jobs: JobRow[];
    sel: number;
    /** The drilled-into job id, or null for the list. */
    open: string | null;
    output: string | null;
    scroll: number;
    rows: number;
  },
) {
  const opened = open ? jobs.find((j) => j.id === open) ?? null : null;
  if (opened) {
    return <JobOutput job={opened} output={output} scroll={scroll} rows={rows} />;
  }
  if (jobs.length === 0) {
    return (
      <Box flexDirection="column" marginTop={1}>
        <Text dimColor>no background shells</Text>
        <Text dimColor>
          long-running commands move here automatically; finished ones stay for 30 minutes
        </Text>
      </Box>
    );
  }
  return (
    <Box flexDirection="column">
      <Text dimColor>enter opens the output · x stops a running job</Text>
      {jobs.map((job, i) => {
        const st = jobStatus(job);
        const s = i === sel;
        return (
          // The status glyph drops its color under selection, matching the mcp tab:
          // an inverse colored fg reads as a bg speck inside the light bar.
          <SelRow key={job.id} sel={s}>
            <Text color={s ? undefined : st.color}>{st.label.padEnd(11)}</Text>
            <Text bold>{job.id.padEnd(7)}</Text>
            <Text dimColor>
              {fmtElapsed((job.endedAt ?? Date.now()) - job.startedAt).padStart(7)}
              {"  "}
              {String(job.outputLines).padStart(4)}L{"  "}
            </Text>
            {job.command}
          </SelRow>
        );
      })}
    </Box>
  );
}
