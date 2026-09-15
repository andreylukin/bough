import type { Meta, StoryObj } from "@storybook/react-vite";
import { Controls, Thread } from "../app";
import { projects, rows, turn } from "./fixtures";

const noop = () => {};
const handlers = {
  onSend: noop, onAnswer: noop, onInterrupt: noop, onArchive: noop, onRename: async () => {},
  onModel: noop, onEffort: noop, onAssign: noop,
};

const meta: Meta<typeof Thread> = {
  title: "Thread/Thread",
  component: Thread,
  args: { projects, lines: turn, busy: false, ...handlers },
  decorators: [(Story) => <div className="app" style={{ height: "100vh" }}><Story /></div>],
};
export default meta;
type S = StoryObj<typeof Thread>;

/** A live turn has no done entry: it ends on "Working · 12s", never on a Done footer. */
export const Running: S = { args: { row: rows[0], lines: turn.filter((l) => l.kind !== "done" && l.kind !== "usage") } };
/** A live ask renders as its own card above the composer, and the composer answers it. */
export const WaitingForYou: S = { args: { row: rows[1] } };
export const Done: S = { args: { row: rows[2] } };
export const Archived: S = { args: { row: { ...rows[2], archived: true } } };
/** A project session whose setup failed: "Why?" beside the orb chip opens the error and the end of resume.log, with Refresh and Rebuild image. */
export const OrbSetupFailed: S = {
  args: { row: { ...rows[2], id: "orb-failed", mode: "project", project: "p2", orb: { project: "bough", status: "failed" }, status: "done" } },
};
/** A project session waiting on its image: "Log" opens the live build output with a running timer, following the end. */
export const OrbBuilding: S = {
  args: { row: { ...rows[0], id: "orb-building", mode: "project", project: "p2", orb: { project: "bough", status: "building" } } },
};

export const Empty: S = {
  render: () => (
    <div className="thread empty">
      <div>
        <h1>Choose a session</h1>
        <p>Pick one on the left to watch it, steer it, or answer what it is waiting on.</p>
      </div>
    </div>
  ),
};

export const ModelAndThinking: StoryObj<typeof Controls> = {
  render: () => <div style={{ padding: 24 }}><Controls row={rows[0]} projects={projects} onModel={noop} onEffort={noop} onAssign={noop} /></div>,
};
