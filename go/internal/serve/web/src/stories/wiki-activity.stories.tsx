import type { Meta, StoryObj } from "@storybook/react-vite";
import { WikiActivityView } from "../wiki";
import { wikiActivity } from "./wiki-fixtures";

const meta: Meta<typeof WikiActivityView> = {
  title: "Wiki/WikiActivityView",
  component: WikiActivityView,
  args: {
    data: wikiActivity,
    onIngest: () => {},
    onOpenPage: () => {},
    onOpenSession: () => {},
    onIndex: () => {},
  },
  decorators: [(Story) => <div className="app" style={{ height: "100vh" }}><Story /></div>],
};
export default meta;
type S = StoryObj<typeof WikiActivityView>;

/** A run is worth a row only for what it did to the wiki. */
export const Today: S = {};

/** A run in progress: no outcome yet, and no commit. */
export const Ingesting: S = {
  args: {
    data: {
      ...wikiActivity,
      runs: [{ ...wikiActivity.runs[0], running: true, done: null, ms: 0, cost: 0, commit: "", outcomes: [], files: [] },
        ...wikiActivity.runs.slice(1)],
    },
  },
};

export const Empty: S = {
  args: { data: { ...wikiActivity, runs: [], today: { runs: 0, ingested: 0, noMaterial: 0, spent: 0 }, spent: 0 } },
};
