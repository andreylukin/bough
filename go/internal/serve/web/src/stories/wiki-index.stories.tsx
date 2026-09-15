import type { Meta, StoryObj } from "@storybook/react-vite";
import { WikiIndexView } from "../wiki";
import { wikiIndex } from "./wiki-fixtures";

const meta: Meta<typeof WikiIndexView> = {
  title: "Wiki/WikiIndexView",
  component: WikiIndexView,
  args: {
    data: wikiIndex,
    onOpen: () => {},
    onReview: () => {},
    onActivity: () => {},
    check: () => Promise.resolve([]),
  },
  decorators: [(Story) => <div className="app" style={{ height: "100vh" }}><Story /></div>],
};
export default meta;
type S = StoryObj<typeof WikiIndexView>;

export const Flagged: S = {};

/** Every claim cited: the health line says so in one word. */
export const Healthy: S = {
  args: {
    data: {
      ...wikiIndex,
      health: { ...wikiIndex.health, unsupported: 0, superseded: 0, uncited: 0, pending: 0 },
      topics: [wikiIndex.topics[0]],
    },
  },
};

/** Scheduler on, no pages yet: waiting count, next tick, and Ingest now. */
export const Indexing: S = {
  args: { data: { ...wikiIndex, exists: false, topics: [] }, onIngest: () => {} },
};

/** Before `bough wiki install`: what the view is and how to start it. */
export const NoWiki: S = {
  args: { data: { ...wikiIndex, exists: false, topics: [], health: { ...wikiIndex.health, installed: false, ingesting: false } } },
};
