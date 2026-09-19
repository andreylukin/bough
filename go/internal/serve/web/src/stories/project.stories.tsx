import type { Meta, StoryObj } from "@storybook/react-vite";
import { ProjectPage } from "../project";
import type { OrbFile, ProjectDetail, Row } from "../types";
import { projects, rows } from "./fixtures";

// One project's own page. The conversation is passed in by the App, so a
// story stands one in: what is being designed here is the two columns
// around it, not the transcript.

const noop = () => {};
const hoursAgo = (h: number) => new Date(Date.now() - h * 3_600_000).toISOString();

const threads: Row[] = rows
  .filter((r) => !r.archived)
  .map((r) => ({ ...r, mode: "project" as const, project: "bough" }));

const detail: ProjectDetail = {
  slug: "bough", name: "Control room", orb: projects[1].orb,
  main: "main-1",
  mainOrb: { session: "main-1", project: "bough", status: "running", up: true, container: "bough-orb-main-1", updatedAt: hoursAgo(2) },
  orbs: [
    { session: "main-1", project: "bough", status: "running", up: true, container: "bough-orb-main-1", updatedAt: hoursAgo(2) },
    { session: "s1", project: "bough", status: "running", up: true, container: "bough-orb-s1", title: "Fix the flaky PTY test", updatedAt: hoursAgo(1) },
  ],
  threads,
};

const files: Partial<Record<OrbFile, string>> = {
  "MEMORY.md": "# Control room\n\nThe web UI builds with bun into dist/app.js, which is committed and embedded.\nRun `bun run build` after touching src/.\n",
  "project.yml": "name: Control room\nrepos:\n  - andreylukin/bough\n",
};

const main: Row = { ...rows[0], id: "main-1", title: "Main thread", status: "idle", live: true };

const meta: Meta<typeof ProjectPage> = {
  title: "Projects/ProjectPage",
  component: ProjectPage,
  args: {
    detail, files, mainRow: main, open: "", titles: Object.fromEntries(rows.map((r) => [r.id, r.title])),
    onOpen: noop, onNewThread: noop, onBack: noop, onStopOrb: noop, onRetry: noop,
    onSave: async () => {}, onMessage: async () => {},
    conversation: <div className="thread" style={{ padding: 16, color: "var(--text-3)" }}>the main thread’s conversation</div>,
  },
  decorators: [(Story) => <div className="app" style={{ height: "100vh" }}><Story /></div>],
};
export default meta;
type S = StoryObj<typeof ProjectPage>;

export const Page: S = {};
/** Nothing said yet: the composer is the whole page. */
export const NoMainThread: S = { args: { detail: { ...detail, main: undefined, mainOrb: undefined, orbs: [], threads: [] }, mainRow: undefined, conversation: undefined } };
/** A main thread with nothing beside it: one line, and no group headers at all. */
export const NoThreads: S = { args: { detail: { ...detail, threads: [] } } };
/** Long enough to be worth saying so: the counter turns amber past 200 lines. */
export const LongMemory: S = { args: { files: { ...files, "MEMORY.md": Array.from({ length: 240 }, (_, i) => `line ${i + 1}`).join("\n") } } };
/** The definition does not parse: the page still opens, and says where the fix is. */
export const BadDefinition: S = { args: { detail: { ...detail, error: "yaml: line 3: mapping values are not allowed in this context" } } };
/** The orb is down: what starts it again is the next message, not a button. */
export const OrbStopped: S = { args: { detail: { ...detail, mainOrb: { ...detail.mainOrb!, status: "stopped", up: false } } } };
/** A thread is open: the conversation swaps and the way back is one chip. */
export const ThreadOpen: S = { args: { open: "s2", conversation: <div className="thread" style={{ padding: 16, color: "var(--text-3)" }}>the thread’s conversation</div> } };
