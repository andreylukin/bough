import type { Meta, StoryObj } from "@storybook/react-vite";
import { WikiReviewView } from "../wiki";
import { wikiReview } from "./wiki-fixtures";

const meta: Meta<typeof WikiReviewView> = {
  title: "Wiki/WikiReviewView",
  component: WikiReviewView,
  args: {
    data: wikiReview,
    onOpenPage: () => {},
    onAct: () => Promise.resolve(),
    onSearch: () => {},
    onIngest: () => Promise.resolve(),
    onIndex: () => {},
  },
  decorators: [(Story) => <div className="app" style={{ height: "100vh" }}><Story /></div>],
};
export default meta;
type S = StoryObj<typeof WikiReviewView>;

/** The unit of review is a claim: the smallest thing that can be kept, labeled or dropped. */
export const Flagged: S = {};

export const Clean: S = {
  args: { data: { flags: [], pending: [] } },
};
