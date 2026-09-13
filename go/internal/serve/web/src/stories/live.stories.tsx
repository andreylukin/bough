import type { Meta, StoryObj } from "@storybook/react-vite";
import { StreamView, SubRun, Thread, TurnView } from "../app";
import { Mentions } from "../mention";
import { StatusMark, Working } from "../status";
import { groupSubs, groupTurns } from "../render";
import { openTurn, projects, rows, streamRuns, subTurn } from "./fixtures";

const noop = () => {};
const handlers = {
  onSend: noop, onAnswer: noop, onInterrupt: noop, onArchive: noop, onRename: async () => {},
  onModel: noop, onEffort: noop, onAssign: noop,
};
const shell = (Story: () => React.ReactNode) => (
  <div className="app" style={{ height: "100vh" }}>{Story()}</div>
);

const meta: Meta<typeof Thread> = {
  title: "Thread/Live",
  component: Thread,
  args: { projects, busy: false, ...handlers },
  decorators: [(Story) => shell(Story)],
};
export default meta;
type S = StoryObj<typeof Thread>;

/**
 * A reply arriving. Nothing on screen below the prompt is recorded
 * yet: the text is fragments the server coalesced on a 50ms timer,
 * and the moment the real entry lands through ?since= it is replaced
 * in place. The caret is the only thing that says so.
 */
export const StreamingReply: S = {
  args: { row: rows[0], lines: openTurn, stream: streamRuns },
};

/** The same turn a beat later: thinking is done, the reply is still coming. */
export const StreamingAfterThinking: S = {
  args: { row: rows[0], lines: openTurn, stream: [streamRuns[1]] },
};

/**
 * Two subagents in one turn, their steps interleaved in the
 * transcript. Each gets its own card behind the run's rail; the one
 * that failed opens itself.
 */
export const SubagentRun: S = {
  args: { row: rows[2], lines: subTurn },
};

/** The run on its own, out of the thread chrome. */
export const SubagentCards: StoryObj = {
  decorators: [],
  render: () => {
    const body = groupTurns(subTurn)[0].body;
    const run = groupSubs(body).find((i) => i.kind === "sub");
    return (
      <div style={{ padding: 24, maxWidth: 760 }}>
        {run?.kind === "sub" && <SubRun agents={run.agents} live />}
      </div>
    );
  },
};

/**
 * A running session, everywhere it says so: the sidebar row's mark,
 * the thread header's mark, and the working line at the foot of the
 * open turn. All three are driven by the derived status — set the row
 * to anything else and all three stop together.
 */
export const RunningSpinner: S = {
  args: { row: rows[0], lines: openTurn, stream: [] },
};

/** The marks alone, including what reduced motion leaves behind. */
export const SpinnerMarks: StoryObj = {
  decorators: [],
  render: () => (
    <div style={{ padding: 24, display: "flex", flexDirection: "column", gap: 18 }}>
      <StatusMark status="running" size={16} />
      <StatusMark status="needs-you" size={16} />
      <StatusMark status="done" size={16} />
      <Working />
      <Working label="Thinking" />
    </div>
  ),
};

/** Fragments with no thread around them. */
export const StreamFragments: StoryObj = {
  decorators: [],
  render: () => (
    <div style={{ padding: 24, maxWidth: 760, display: "flex", flexDirection: "column", gap: 14 }}>
      <StreamView runs={streamRuns} />
    </div>
  ),
};

/**
 * The "/" picker. It opens at any word start now, not only when the
 * slash leads the message — half-remembering a skill's name happens
 * mid-sentence like any other name.
 */
export const SkillPickerOpen: StoryObj = {
  decorators: [],
  render: () => (
    <div style={{ padding: "180px 24px 24px", maxWidth: 760 }}>
      <div className="composer">
        <Mentions session="s1" onPick={noop} onClose={noop}
          trigger={{ kind: "/", token: "gr", from: 21, to: 23 }} />
        <textarea id="composer" aria-label="Message" rows={2}
          defaultValue="take another pass /gr" />
        <div style={{ display: "flex", gap: 14 }}>
          <span className="hint">Return to send</span>
        </div>
      </div>
    </div>
  ),
};

/** The "@" picker: files, ranked by the server that saw the whole tree. */
export const FilePickerOpen: StoryObj = {
  decorators: [],
  render: () => (
    <div style={{ padding: "180px 24px 24px", maxWidth: 760 }}>
      <div className="composer">
        <Mentions session="s1" onPick={noop} onClose={noop}
          trigger={{ kind: "@", token: "del", from: 9, to: 13 }} />
        <textarea id="composer" aria-label="Message" rows={2}
          defaultValue="read the @del" />
        <div style={{ display: "flex", gap: 14 }}>
          <span className="hint">Return to send</span>
        </div>
      </div>
    </div>
  ),
};

/** One turn, subagents and all, at phone width. */
export const SubagentsNarrow: StoryObj = {
  decorators: [],
  render: () => (
    <div style={{ width: 390, padding: 16, border: "1px solid var(--line)" }}>
      <div className="transcript" style={{ padding: 0 }}>
        {groupTurns(subTurn).map((t) => <TurnView key={t.seq} turn={t} />)}
      </div>
    </div>
  ),
};
