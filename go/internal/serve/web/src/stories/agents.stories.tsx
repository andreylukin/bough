import { useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { AgentsChip, Sidebar, Thread } from "../app";
import { DialogHost, askChoice } from "../dialog";
import { agentRows, projects, turn } from "./fixtures";

const noop = () => {};
const handlers = {
  onSend: noop, onAnswer: noop, onInterrupt: noop, onArchive: noop, onRename: async () => {},
  onModel: noop, onEffort: noop, onAssign: noop,
};

function Tree() {
  const [selected, setSelected] = useState<string | null>("k-routes01");
  const [query, setQuery] = useState("");
  return (
    <div className="app" style={{ height: "100vh" }}>
      <Sidebar rows={agentRows} selected={selected} onSelect={setSelected} query={query} onQuery={setQuery}
               showArchived={false} onToggleArchived={noop} />
    </div>
  );
}

const meta: Meta = { title: "Agents/Background agents" };
export default meta;

/** Children nest under the session that started them; the queued one waits for a slot. */
export const ParentAndChildren: StoryObj = { render: () => <Tree /> };

export const ParentHeadCount: StoryObj = {
  render: () => (
    <div className="app" style={{ height: "100vh" }}>
      <Thread row={agentRows[0]} rows={agentRows} onOpenSession={noop} lines={turn} projects={projects} busy={false} {...handlers} />
    </div>
  ),
};

export const ChildHeadBacklink: StoryObj = {
  render: () => (
    <div className="app" style={{ height: "100vh" }}>
      <Thread row={agentRows[2]} rows={agentRows} onOpenSession={noop} lines={turn} projects={projects} busy={false} {...handlers} />
    </div>
  ),
};

export const Chip: StoryObj = {
  render: () => <div style={{ padding: 24 }}><AgentsChip row={agentRows[0]} rows={agentRows} onOpen={noop} /></div>,
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
