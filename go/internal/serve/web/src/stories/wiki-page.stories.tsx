import type { Meta, StoryObj } from "@storybook/react-vite";
import { WikiPageView } from "../wiki";
import { wikiPage, wikiSource } from "./wiki-fixtures";

const meta: Meta<typeof WikiPageView> = {
  title: "Wiki/WikiPageView",
  component: WikiPageView,
  args: {
    page: wikiPage,
    onCite: () => {},
    onCloseSource: () => {},
    onOpenPage: () => {},
    onIndex: () => {},
    onOpenSession: () => {},
    onSave: () => Promise.resolve(),
    loadHistory: () => Promise.resolve([]),
  },
  decorators: [(Story) => <div className="app" style={{ height: "100vh" }}><Story /></div>],
};
export default meta;
type S = StoryObj<typeof WikiPageView>;

/** Evidence in the margin: every state a claim can be in, on one page. */
export const Claims: S = {};

/** A marker clicked: the cited entry in place, with what surrounds it. */
export const WithSource: S = {
  args: { cite: { session: wikiSource.session.id, seq: 214 }, source: wikiSource },
};
