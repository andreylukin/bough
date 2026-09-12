import type { Meta, StoryObj } from "@storybook/react-vite";
import { Controls, Thread } from "../app";
import { projects, rows, turn } from "./fixtures";

const noop = () => {};
const handlers = {
  onSend: noop, onAnswer: noop, onInterrupt: noop, onArchive: noop, onRename: noop,
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

export const Running: S = { args: { row: rows[0] } };
/** A live ask renders as its own card above the composer, and the composer answers it. */
export const WaitingForYou: S = { args: { row: rows[1] } };
export const Done: S = { args: { row: rows[2] } };
export const Archived: S = { args: { row: { ...rows[2], archived: true } } };

export const Empty: S = {
  render: () => (
    <div className="thread empty">
      <div>
        <h1>No session open</h1>
        <p>Pick a session on the left to watch it, steer it, or answer what it is waiting on.</p>
      </div>
    </div>
  ),
};

export const ModelAndThinking: StoryObj<typeof Controls> = {
  render: () => <div style={{ padding: 24 }}><Controls row={rows[0]} projects={projects} onModel={noop} onEffort={noop} onAssign={noop} /></div>,
};
