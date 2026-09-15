import type { Meta, StoryObj } from "@storybook/react-vite";
import { CodeBlock, Entry, JobBlock, ResultBlock, TodoRun, TurnView } from "../app";
import { Markdown, groupTurns } from "../render";
import { jobLegacyFailed, jobTyped0, jobTyped2, jobTypedNoExit, markdown, turn } from "./fixtures";
import { ExecNote } from "../work-ui";
import type { Line } from "../types";

const meta: Meta = {
  title: "Transcript",
  parameters: { layout: "padded" },
  decorators: [(Story) => <div className="transcript" style={{ padding: 0, maxWidth: 820 }}><Story /></div>],
};
export default meta;

/** One prompt and everything the agent did before it stopped. */
export const Turn: StoryObj = { render: () => <TurnView turn={groupTurns(turn)[0]} /> };
/** A turn from an earlier day shows its date beside the time. */
export const OlderTurn: StoryObj = {
  render: () => <TurnView turn={groupTurns(turn.map((l) => ({ ...l, at: new Date(Date.now() - 3 * 86400e3).toISOString() })))[0]} />,
};

const at = new Date(Date.now() - 4 * 60_000).toISOString();
const wakeLines: Line[] = [
  { seq: 901, at, kind: "input", text: "[background job] A command you started in the background has finished while you were idle. Deal with it if it needs anything, then reply to the user with what happened.\n\njob 8 [failed] # bash \"$BOUGH_SCRATCH/setup.sh\" > \"$BOUGH_SCRATCH/setup.log\" 2>&1 … (1m37s): exit status 137" },
  { seq: 902, at, kind: "assistant", text: "The setup job was killed (exit 137, out of memory). I'll rerun it with fewer parallel builds." },
  { seq: 903, at, kind: "done", text: "" },
];
/** A turn a finished background job started: a notice with the job's row, not a prompt you typed. */
export const JobWakeUp: StoryObj = { render: () => <TurnView turn={groupTurns(wakeLines)[0]} /> };
/** A failing block ends the reply; the blocks after it were skipped, and the notice says why. */
export const SkippedBlocksNotice: StoryObj = { render: () => <ExecNote note={{ notRun: 2, reason: "this block failed." }} /> };

export const Reply: StoryObj = { render: () => <Entry line={turn[2]} codes={[turn[3].text]} /> };
export const Thinking: StoryObj = { render: () => <Entry line={turn[1]} codes={[]} /> };
export const Code: StoryObj = { render: () => <CodeBlock line={turn[3]} /> };
export const Result: StoryObj = { render: () => <ResultBlock line={turn[4]} /> };
export const Job: StoryObj = { render: () => <JobBlock line={turn[5]} /> };
export const JobFailed: StoryObj = { render: () => <JobBlock line={turn[6]} /> };
/** A typed record only: exit 0 is Finished, and there is no output to show. */
export const JobTypedExit0: StoryObj = { render: () => <JobBlock line={jobTyped0} /> };
/** Exit 2 is Failed · exit 2. */
export const JobTypedExitNonZero: StoryObj = { render: () => <JobBlock line={jobTyped2} /> };
/** Finished with no exit recorded: Outcome unknown, "Exit not recorded.", no timer, no Stop. */
export const JobTypedExitAbsent: StoryObj = { render: () => <JobBlock line={jobTypedNoExit} /> };
/** The loop's [failed] notice carries no exit code, so none is shown. */
export const JobLegacyFailed: StoryObj = { render: () => <JobBlock line={jobLegacyFailed} /> };
export const Error: StoryObj = { render: () => <Entry line={turn[7]} codes={[]} /> };
const todoAt = new Date(Date.now() - 6 * 60_000).toISOString();
const todo = (seq: number, kind: string, id?: number, text?: string): Line => ({ seq, at: todoAt, kind, text: text ?? "", data: id === undefined ? {} : { id, ...(text ? { text } : {}) } });
const planAdds: Line[] = [
  todo(1, "todo/add", 1, "Read the watcher and find where it sleeps"),
  todo(2, "todo/add", 2, "Inject the fake clock from kernel/testclock"),
  todo(3, "todo/add", 3, "Drop the 800ms sleep from todo_test.go"),
  todo(4, "todo/add", 4, "Run go test -race ./plugins/todo/"),
  todo(5, "todo/add", 5, "Fix the go vet unusedresult warning"),
];
/** The plan as it stands: 2 of 5 done, the first open item spinning while the turn runs. */
export const TodoPlanInProgress: StoryObj = {
  render: () => <TodoRun live lines={[...planAdds, todo(6, "todo/done", 1), todo(7, "todo/done", 2)]} />,
  play: async ({ canvasElement }) => { canvasElement.querySelector<HTMLDetailsElement>(".todo-plan")!.open = true; },
};
/** Every item done: struck through, the bar full, no spinner. */
export const TodoPlanAllDone: StoryObj = {
  render: () => <TodoRun lines={[...planAdds, ...[1, 2, 3, 4, 5].map((id) => todo(5 + id, "todo/done", id))]} />,
  play: async ({ canvasElement }) => { canvasElement.querySelector<HTMLDetailsElement>(".todo-plan")!.open = true; },
};
/** A cleared list says so; the events stay under History. */
export const TodoCleared: StoryObj = {
  render: () => <TodoRun lines={[...planAdds.slice(0, 2), todo(3, "todo/done", 1), todo(4, "todo/clear")]} />,
  play: async ({ canvasElement }) => { canvasElement.querySelector<HTMLDetailsElement>(".todo-plan")!.open = true; },
};
export const Subagent: StoryObj ={ render: () => <Entry line={turn[8]} codes={[]} /> };

/** Assistant replies are markdown, sanitized. Every element the app styles. */
export const MarkdownReply: StoryObj = {
  render: () => <div className="say"><Markdown text={markdown} /></div>,
};
