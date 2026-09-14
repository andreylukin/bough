import { useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { Sidebar, Thread } from "../app";
import { DialogHost, askChoice } from "../dialog";
import type { Row } from "../types";
import { agentRows, backgroundRows, projects, turn, workChildren, workRow } from "./fixtures";

const noop = () => {};
const handlers = {
  onSend: noop, onAnswer: noop, onInterrupt: noop, onArchive: noop, onRename: async () => {},
  onModel: noop, onEffort: noop, onAssign: noop,
};

function Tree({ rows = agentRows, selected: first = "k-routes01" }: { rows?: Row[]; selected?: string }) {
  const [selected, setSelected] = useState<string | null>(first);
  const [query, setQuery] = useState("");
  return (
    <div className="app" style={{ height: "100vh" }}>
      <Sidebar rows={rows} selected={selected} onSelect={setSelected} query={query} onQuery={setQuery}
               showArchived={false} onToggleArchived={noop} />
    </div>
  );
}

const child = (row: Row, rows: Row[]) => (
  <div className="app" style={{ height: "100vh" }}>
    <Thread row={row} rows={rows} onOpenSession={noop} lines={turn} projects={projects} busy={false} {...handlers} />
  </div>
);

const meta: Meta = { title: "Agents/Background agents" };
export default meta;

/** Children nest under the session that started them; the queued one waits for a slot. */
export const ParentAndChildren: StoryObj = { render: () => <Tree /> };

/** A queued child named by its task, and one with no task yet: "Queued agent". */
export const QueuedChildWithAndWithoutTask: StoryObj = {
  render: () => <Tree rows={[workRow({ status: "running" }), ...workChildren]} selected="k-que0002" />,
};

/** The Background heading's second line: "1 running · 1 failed". */
export const BackgroundHeadingSummary: StoryObj = {
  render: () => <Tree rows={backgroundRows} selected="s1" />,
};

export const ParentHeadCount: StoryObj = {
  render: () => (
    <div className="app" style={{ height: "100vh" }}>
      <Thread row={agentRows[0]} rows={agentRows} onOpenSession={noop} lines={turn} projects={projects} busy={false} {...handlers} />
    </div>
  ),
};

/** "← Parent: Split the serve API by resource" above the title. */
export const ChildHeadBacklink: StoryObj = { render: () => child(agentRows[2], agentRows) };

/** The parent has no title yet: "← Parent session". */
export const ChildHeadUntitledParent: StoryObj = {
  render: () => child(agentRows[2], [{ ...agentRows[0], title: "" }, ...agentRows.slice(1)]),
};

/** The parent cannot be found: plain "Parent unavailable", not a link. */
export const ChildHeadParentUnavailable: StoryObj = {
  render: () => child({ ...agentRows[2], spawnedBy: "gone-parent" }, agentRows.slice(1)),
};

export const ArchiveConfirm: StoryObj = {
  render: () => {
    const [picked, setPicked] = useState<string | null>(null);
    return (
      <div style={{ padding: 24 }}>
        <button className="btn" onClick={async () => setPicked(await askChoice("Archive Split the serve API by resource?",
          "Stop its 3 running agents too?", ["Stop and archive", "Archive only"]))}>Archive</button>
        <p className="meta-line">Chose: {picked ?? "nothing"}</p>
        <DialogHost />
      </div>
    );
  },
};
