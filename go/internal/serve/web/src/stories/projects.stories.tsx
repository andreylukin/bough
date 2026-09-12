import type { Meta, StoryObj } from "@storybook/react-vite";
import { ProjectsView } from "../projects";
import { projects, rows } from "./fixtures";

const noop = () => {};
const meta: Meta<typeof ProjectsView> = {
  title: "Projects/ProjectsView",
  component: ProjectsView,
  args: { onOpen: noop, onAssign: noop, onCreate: noop, onRename: noop, onDelete: noop },
  decorators: [(Story) => <div className="app" style={{ height: "100vh" }}><Story /></div>],
};
export default meta;
type S = StoryObj<typeof ProjectsView>;

export const Grouped: S = { args: { projects, rows } };
export const NoProjects: S = { args: { projects: [], rows } };
export const EmptyProject: S = { args: { projects: [{ id: "p9", name: "Nothing here" }], rows: [] } };
