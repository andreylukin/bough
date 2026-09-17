import { Fragment, useEffect, useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { Sidebar, Thread, TurnView } from "../app";
import { groupTurns } from "../render";
import type { Line, Row } from "../types";
import { WorkButton, WorkDialog, useWork, type ChildState } from "../work-ui";
import { workCounts } from "../work";
import {
  jobGroupLines, jobGroupRunning, legacyJob, projects, subStates, twentyChildren, twentyJobs, twentyLines,
  typedJob, workAllLines, workChildren, workRow,
} from "./fixtures";
import { AutoStop, WorkFrame } from "./work-frame";

const noop = () => {};
const handlers = {
  onSend: noop, onAnswer: noop, onInterrupt: noop, onArchive: noop, onRename: async () => {},
  onModel: noop, onEffort: noop, onAssign: noop,
};

/** `a` now, `b` after `ms`: a record landing while the story is on screen. */
function useLater<T>(a: T, b: T, ms: number): T {
  const [v, setV] = useState(a);
  useEffect(() => { const t = setTimeout(() => setV(b), ms); return () => clearTimeout(t); }, []); // eslint-disable-line react-hooks/exhaustive-deps
  return v;
}

/** The transcript of a frame's lines, every turn. */
function Turns({ lines }: { lines: Line[] }) {
  return <>{groupTurns(lines).map((t) => <TurnView key={t.seq} turn={t} />)}</>;
}

/** The Work dialog over an empty thread, reading the frame's index. */
function Dialog({ childState = "ok", paused, sheet = false }: { childState?: ChildState; paused?: boolean; sheet?: boolean }) {
  const ctx = useWork();
  return (
    <WorkDialog workers={ctx?.workers ?? []} sheet={sheet} childState={childState} onRetryChildren={noop} paused={paused}
                parent={ctx?.session ?? ""} onClose={noop} onView={noop} onOpenAgent={noop} />
  );
}

const pane = (node: React.ReactNode) => (
  <div className="app" style={{ height: "100vh" }}><div className="thread">{node}</div></div>
);
const transcript = (node: React.ReactNode) => (
  <div className="transcript" style={{ padding: 16, maxWidth: 820 }}>{node}</div>
);

const viewports = {
  phone320: { name: "Phone 320", styles: { width: "320px", height: "640px" }, type: "mobile" },
  phone390: { name: "Phone 390", styles: { width: "390px", height: "844px" }, type: "mobile" },
  desk1280: { name: "Desktop 1280", styles: { width: "1280px", height: "800px" }, type: "desktop" },
};

const meta: Meta = { title: "Work", parameters: { viewport: { options: viewports } } };
export default meta;

/* ---------------- jobs ---------------- */

/** "Jobs · 2 running · 2 failed": consecutive job rows under one head, Stop on the running ones. */
export const JobGroupRunningAndFailed: StoryObj = {
  render: () => {
    const row = workRow({ id: "w-jobs", jobs: jobGroupRunning });
    return <WorkFrame row={row} lines={jobGroupLines}>{transcript(<Turns lines={jobGroupLines} />)}</WorkFrame>;
  },
};

/* ---------------- Work button ---------------- */

/** Every form the button takes, wide and two-line narrow. */
export const WorkButtonForms: StoryObj = {
  render: () => {
    const c = (o: Partial<ReturnType<typeof workCounts>>) => ({ ...workCounts([]), ...o });
    const forms: [string, React.ComponentProps<typeof WorkButton>][] = [
      ["live", { counts: c({ running: 2, queued: 1, failed: 1, total: 4 }), expanded: false, onClick: noop }],
      ["all finished", { counts: c({ finished: 7, total: 7 }), expanded: false, onClick: noop }],
      ["ended with a failure", { counts: c({ finished: 7, failed: 1, total: 8 }), expanded: false, onClick: noop }],
      ["unknown and new result", { counts: c({ running: 1, unknown: 1, newResults: 1, finished: 1, total: 3 }), expanded: false, onClick: noop }],
      ["loading, nothing known", { counts: c({}), loading: true, expanded: false, onClick: noop }],
      ["unavailable, nothing known", { counts: c({}), unavailable: true, expanded: false, onClick: noop }],
      ["paused", { counts: c({ running: 2, total: 2 }), paused: true, expanded: false, onClick: noop }],
      ["narrow", { counts: c({ running: 2, queued: 1, failed: 1, total: 4 }), narrow: true, expanded: false, onClick: noop }],
    ];
    return (
      <div style={{ padding: 24, display: "grid", gridTemplateColumns: "max-content 1fr", gap: "12px 16px", alignItems: "center" }}>
        {forms.map(([label, p]) => <Fragment key={label}><span className="meta-line">{label}</span><div><WorkButton {...p} /></div></Fragment>)}
      </div>
    );
  },
};

/** A session that started agents, still looking them up: the button shows before anything is known. */
export const HeaderLoadingNoWorkers: StoryObj = {
  render: () => pane(
    <Thread row={workRow({ id: "w-loading", status: "idle", agents: { running: 0, queued: 0, total: 1 } })} lines={[]} projects={projects} busy={false} {...handlers} />,
  ),
};

/** The lookup failed: "Work · Unavailable", never hidden. */
export const HeaderUnavailableNoWorkers: StoryObj = {
  render: () => pane(
    <Thread row={workRow({ id: "w-down", status: "idle", agents: { running: 0, queued: 0, total: 1 } })} lines={[]} projects={projects} busy={false} {...handlers} />,
  ),
};

/** The feed stopped: last-known counts, "Updates paused" in the strip. */
export const HeaderReconnecting: StoryObj = {
  render: () => pane(
    <Thread row={workRow({ jobs: [{ id: 21, cmd: "bun run build --watch", started: new Date().toISOString() }] })} rows={workChildren}
            lines={workAllLines} paused={Date.now() - 90_000} onRetry={noop} projects={projects} busy={false} {...handlers} />,
  ),
};

/* ---------------- Work dialog ---------------- */

const allRow = workRow({ jobs: [{ id: 21, cmd: "bun run build --watch", started: new Date().toISOString() }] });

/** Needs review, Running, Queued and History, with every kind filter. */
export const PopoverAllGroups: StoryObj = {
  render: () => <WorkFrame row={allRow} lines={workAllLines} kids={workChildren}>{pane(<Dialog />)}</WorkFrame>,
};

export const PopoverLoading: StoryObj = {
  render: () => <WorkFrame row={allRow} lines={workAllLines}>{pane(<Dialog childState="loading" />)}</WorkFrame>,
};

export const PopoverUnavailable: StoryObj = {
  render: () => <WorkFrame row={allRow} lines={workAllLines}>{pane(<Dialog childState="error" />)}</WorkFrame>,
};

/** The lookup succeeded and found no agents, and nothing else ran. */
export const PopoverEmptyAfterLookup: StoryObj = {
  render: () => <WorkFrame row={workRow({ id: "w-none", status: "idle" })} lines={[]} kids={[]}>{pane(<Dialog childState="ok" />)}</WorkFrame>,
};

export const PopoverReconnecting: StoryObj = {
  render: () => <WorkFrame row={allRow} lines={workAllLines} kids={workChildren}>{pane(<Dialog paused />)}</WorkFrame>,
};

/** The bottom sheet a phone gets. */
export const SheetAt390: StoryObj = {
  globals: { viewport: { value: "phone390", isRotated: false } },
  render: () => (
    <WorkFrame row={allRow} lines={workAllLines} kids={workChildren}>
      <div className="app" style={{ height: "100vh", maxWidth: 390 }}><div className="thread"><Dialog sheet /></div></div>
    </WorkFrame>
  ),
};

/* ---------------- Stop ---------------- */

/** Stop agent → Stopping… → the child's own row says Stopped. */
export const StopAcceptedThenConfirmed: StoryObj = {
  render: function Story() {
    const running = workChildren.slice(0, 1);
    const kids = useLater(running, [{ ...running[0], status: "stopped" as const, live: false }], 2600);
    return (
      <WorkFrame row={workRow({ id: "w-stopok" })} lines={[]} kids={kids}>
        <AutoStop pick={(w) => w.kind === "agent"} fromWork />
        {pane(<Dialog />)}
      </WorkFrame>
    );
  },
};

/** The kill request is refused: "Try stop again", named for the job. */
export const StopRequestFailed: StoryObj = {
  render: () => {
    const lines = [typedJob(7, "started", { cmd: "npm test -- --watch=false" })];
    const row = workRow({ id: "w-stopfail", jobs: [{ id: 7, cmd: "npm test -- --watch=false", started: new Date().toISOString() }] });
    return (
      <WorkFrame row={row} lines={lines}>
        <AutoStop pick={(w) => w.id === "7"} />
        {transcript(<Turns lines={lines} />)}
      </WorkFrame>
    );
  },
};

const naturalStart = [typedJob(8, "started", { cmd: "go test -race ./internal/serve/..." })];
const naturalEnd = [...naturalStart, legacyJob("job 8 [exited 1] go test -race ./internal/serve/... (41s)\n--- FAIL: TestChildrenQueued (0.02s)")];

/** Stop pressed, then the job fails on its own before the kill lands: the recorded failure wins. */
export const StopNaturalFailureDuringStop: StoryObj = {
  render: function Story() {
    const lines = useLater(naturalStart, naturalEnd, 2200);
    const row = workRow({ id: "w-natural", jobs: lines === naturalStart ? [{ id: 8, cmd: "go test -race ./internal/serve/...", started: new Date().toISOString() }] : [] });
    return (
      <WorkFrame row={row} lines={lines}>
        <AutoStop pick={(w) => w.id === "8"} />
        {transcript(<Turns lines={lines} />)}
      </WorkFrame>
    );
  },
};

/** Parent Run: Idle and not live; its background agent still runs, so it keeps Stop. */
export const IdleParentWithLiveChild: StoryObj = {
  render: () => {
    const row = workRow({ id: "w-idle", status: "idle", live: false });
    const kids = [workRow({ id: "k-live0001", title: "Port the hooks view to the new router", status: "running", spawnedBy: "w-idle" })];
    return <WorkFrame row={row} lines={[]} kids={kids}>{pane(<Dialog />)}</WorkFrame>;
  },
};

/** "Local · read-only" means no file writes; Stop Job still works on a live session. */
export const LocalReadOnlyStillShowsStop: StoryObj = {
  render: () => {
    const lines: Line[] = [
      { seq: 1, at: new Date().toISOString(), kind: "input", text: "Tail the dev server log while I click around." },
      typedJob(12, "started", { cmd: "tail -f ~/.bough/serve.log" }),
    ];
    const row = workRow({ id: "w-local", title: "Watch the serve log", mode: "local", jobs: [{ id: 12, cmd: "tail -f ~/.bough/serve.log", started: new Date().toISOString() }] });
    return pane(<Thread row={row} lines={lines} projects={projects} busy={false} {...handlers} />);
  },
};

/* ---------------- scale and geometry ---------------- */

const twentyRow = workRow({ id: "w-twenty", jobs: twentyJobs });

/** Eight jobs, six subagents, six background agents, all running. */
export const TwentyConcurrentWorkers: StoryObj = {
  render: () => <WorkFrame row={twentyRow} lines={twentyLines} kids={twentyChildren}>{pane(<Dialog />)}</WorkFrame>,
};

export const TwentyConcurrentInTranscript: StoryObj = {
  render: () => <WorkFrame row={twentyRow} lines={twentyLines} kids={twentyChildren}>{transcript(<Turns lines={twentyLines} />)}</WorkFrame>,
};

const narrowLines = [...jobGroupLines, ...subStates.failed.slice(1, -1), ...subStates.waiting.slice(1)];
const narrowRow: Row = workRow({ id: "w-all", jobs: jobGroupRunning });

/** The whole thread on the narrowest phone: rows stack, Stop stays, the Work button is two lines. */
export const Viewport320: StoryObj = {
  globals: { viewport: { value: "phone320", isRotated: false } },
  render: () => (
    <div className="app" style={{ height: "100vh", maxWidth: 320 }}>
      <Thread row={narrowRow} rows={workChildren} lines={narrowLines} projects={projects} busy={false} {...handlers} />
    </div>
  ),
};

/** A 600px main pane on a 1280px screen: the pane's container queries stack rows, not the window's width. */
export const MainPane600In1280: StoryObj = {
  globals: { viewport: { value: "desk1280", isRotated: false } },
  render: () => (
    <div className="app" style={{ height: "100vh", width: 1280 }}>
      <Sidebar rows={[narrowRow, ...workChildren]} selected="w-all" onSelect={noop} query="" onQuery={noop} showArchived={false} onToggleArchived={noop} />
      <div style={{ width: 600, flex: "none", display: "flex", minWidth: 0, borderRight: "1px solid var(--line)" }}>
        <Thread row={narrowRow} rows={workChildren} lines={narrowLines} projects={projects} busy={false} {...handlers} />
      </div>
      <div style={{ flex: 1 }} aria-hidden="true" />
    </div>
  ),
};
