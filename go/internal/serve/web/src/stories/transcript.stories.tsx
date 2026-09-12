import type { Meta, StoryObj } from "@storybook/react-vite";
import { CodeBlock, Entry, JobBlock, ResultBlock, TurnView } from "../app";
import { Markdown, groupTurns } from "../render";
import { markdown, turn } from "./fixtures";

const meta: Meta = {
  title: "Transcript",
  parameters: { layout: "padded" },
  decorators: [(Story) => <div className="transcript" style={{ padding: 0, maxWidth: 820 }}><Story /></div>],
};
export default meta;

/** One prompt and everything the agent did before it stopped. */
export const Turn: StoryObj = { render: () => <TurnView turn={groupTurns(turn)[0]} /> };

export const Reply: StoryObj = { render: () => <Entry line={turn[2]} codes={[turn[3].text]} /> };
export const Thinking: StoryObj = { render: () => <Entry line={turn[1]} codes={[]} /> };
export const Code: StoryObj = { render: () => <CodeBlock line={turn[3]} /> };
export const Result: StoryObj = { render: () => <ResultBlock line={turn[4]} /> };
export const Job: StoryObj = { render: () => <JobBlock line={turn[5]} /> };
export const JobFailed: StoryObj = { render: () => <JobBlock line={turn[6]} /> };
export const Error: StoryObj = { render: () => <Entry line={turn[7]} codes={[]} /> };
export const Subagent: StoryObj = { render: () => <Entry line={turn[8]} codes={[]} /> };

/** Assistant replies are markdown, sanitized. Every element the app styles. */
export const MarkdownReply: StoryObj = {
  render: () => <div className="say"><Markdown text={markdown} /></div>,
};
