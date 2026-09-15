import type { Meta, StoryObj } from "@storybook/react-vite";
import { ProjectsView } from "../projects";
import { projects, rows } from "./fixtures";

const noop = () => {};
const meta: Meta<typeof ProjectsView> = {
  title: "Projects/ProjectsView",
  component: ProjectsView,
  args: { onOpen: noop, onAssign: noop, onAssignMany: async () => [], onCreate: async () => ({ id: "p" }), onRename: async () => {}, onDelete: noop },
  decorators: [(Story) => <div className="app" style={{ height: "100vh" }}><Story /></div>],
};
export default meta;
type S = StoryObj<typeof ProjectsView>;

export const Grouped: S = { args: { projects, rows } };
/** No project yet: the explainer lives in the empty state, with the one action. */
export const NoProjects: S = { args: { projects: [], rows } };
/** The narrow pass: the head wraps, rows are two lines, checkboxes always show. */
export const Phone: S = { args: { projects, rows }, globals: { viewport: { value: "mobile1" } } };
export const EmptyProject: S = { args: { projects: [{ id: "p9", name: "Nothing here" }], rows: [] } };
