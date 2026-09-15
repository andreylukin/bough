import { useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { Sidebar, type View } from "../app";
import { projects, rows } from "./fixtures";
import type { Project, Row } from "../types";

function Live({ rows: list, selected: initial = "s1", showArchived = false, projects: labels = projects }: { rows: Row[]; selected?: string | null; showArchived?: boolean; projects?: Project[] }) {
  const [selected, setSelected] = useState<string | null>(initial);
  const [query, setQuery] = useState("");
  const [view, setView] = useState<View>("sessions");
  const [archived, setArchived] = useState(showArchived);
  const q = query.trim().toLowerCase();
  const visible = q ? list.filter((r) => [r.title, r.repo, r.branch].some((v) => v?.toLowerCase().includes(q))) : list;
  return (
    <div className="app" style={{ height: "100vh" }}>
      <Sidebar rows={visible.filter((r) => archived || !r.archived)} projects={labels} selected={selected} onSelect={setSelected}
               query={query} onQuery={setQuery} view={view} onView={setView}
               showArchived={archived} onToggleArchived={() => setArchived((v) => !v)} />
    </div>
  );
}

const meta: Meta<typeof Live> = { title: "Navigation/Sidebar", component: Live };
export default meta;
type S = StoryObj<typeof Live>;

/** Sessions in a project group under the project's name ("Incident 42", "Control room"); the rest group by checkout. */
export const Grouped: S = { args: { rows } };
/**
 * A parent with background agents: the agents get no rows of their own; the
 * parent says "Background: 2 running · 1 failed", and its Work panel lists them.
 */
export const BackgroundAgentCounts: S = {
  args: {
    rows: [
      rows[0],
      { ...rows[0], id: "k1", title: "Map every route", spawnedBy: "s1", status: "running" },
      { ...rows[0], id: "k2", title: "Write the docs", spawnedBy: "s1", status: "running" },
      { ...rows[0], id: "k3", title: "Port the hooks view", spawnedBy: "s1", status: "error", live: false },
      rows[2],
    ],
  },
};
/** Waiting and failed sessions pin to Needs you on top and leave their group, whose head says "N need you ↑". Every session id renders in exactly one row. */
export const NeedsYou: S = {
  args: {
    rows: [
      { ...rows[0], id: "n1", title: "Pick a migration order", status: "needs-you" },
      { ...rows[0], id: "n2", title: "Fix the flaky login test", status: "error", live: false },
      ...rows,
    ],
  },
};
export const NothingYet: S = { args: { rows: [], selected: null } };
export const WithArchived: S = { args: { rows, showArchived: true } };
