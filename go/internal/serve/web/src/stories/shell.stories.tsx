import { useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { Sidebar, Thread, type View } from "../app";
import { ProjectsView } from "../projects";
import { projects, rows, turn } from "./fixtures";

const noop = () => {};

/** The whole control room, sidebar and thread, with fixture data. The
 * design bench: what a change to index.html looks like in context. */
function Shell({ selected: initial = "s2", pane: initialPane = "thread" }: { selected?: string | null; pane?: "list" | "thread" }) {
  const [selected, setSelected] = useState<string | null>(initial);
  const [pane, setPane] = useState<"list" | "thread">(initialPane);
  const [query, setQuery] = useState("");
  const [view, setView] = useState<View>("sessions");
  const [archived, setArchived] = useState(false);
  const row = rows.find((r) => r.id === selected) ?? null;
  return (
    <div className="app" data-pane={pane}>
      <Sidebar rows={rows.filter((r) => archived || !r.archived)} selected={selected}
               onSelect={(id) => { setSelected(id); setView("sessions"); setPane("thread"); }}
               query={query} onQuery={setQuery} view={view} onView={(v) => { setView(v); setPane("thread"); }}
               showArchived={archived} onToggleArchived={() => setArchived((v) => !v)} />
      {view === "projects" ? (
        <ProjectsView projects={projects} rows={rows} onOpen={(id) => { setSelected(id); setView("sessions"); }} onBack={() => setPane("list")}
                      onAssign={noop} onCreate={noop} onRename={noop} onDelete={noop} />
      ) : row ? (
        <Thread row={row} lines={turn} projects={projects} busy={false} onBack={() => setPane("list")}
                onSend={noop} onAnswer={noop} onInterrupt={noop} onArchive={noop} onRename={noop}
                onModel={noop} onEffort={noop} onAssign={noop} />
      ) : (
        <div className="thread empty"><div>
          <h1>No session open</h1>
          <p>Pick a session on the left to watch it, steer it, or answer what it is waiting on.</p>
        </div></div>
      )}
    </div>
  );
}

const meta: Meta<typeof Shell> = { title: "Shell", component: Shell };
export default meta;
type S = StoryObj<typeof Shell>;

export const WaitingForYou: S = { args: { selected: "s2" } };
export const Running: S = { args: { selected: "s1" } };
export const NothingOpen: S = { args: { selected: null } };
export const PhoneList: S = { args: { selected: "s2", pane: "list" }, globals: { viewport: { value: "mobile1" } } };
export const PhoneThread: S = { args: { selected: "s2", pane: "thread" }, globals: { viewport: { value: "mobile1" } } };
