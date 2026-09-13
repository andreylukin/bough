import { useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { Sidebar, TopBar, type View } from "../app";
import { rows } from "./fixtures";
import type { Row } from "../types";

function Live({ rows: list, selected: initial = "s1", showArchived = false }: { rows: Row[]; selected?: string | null; showArchived?: boolean }) {
  const [selected, setSelected] = useState<string | null>(initial);
  const [query, setQuery] = useState("");
  const [view, setView] = useState<View>("sessions");
  const [archived, setArchived] = useState(showArchived);
  const q = query.trim().toLowerCase();
  const visible = q ? list.filter((r) => [r.title, r.repo, r.branch].some((v) => v?.toLowerCase().includes(q))) : list;
  return (
    <div className="app" data-pane="list" style={{ height: "100vh" }}>
      <TopBar query={query} onQuery={setQuery} view={view} onView={setView} />
      <div className="app-body">
        <Sidebar rows={visible.filter((r) => archived || !r.archived)} selected={selected} onSelect={setSelected}
                 query={query} showArchived={archived} onToggleArchived={() => setArchived((v) => !v)} />
      </div>
    </div>
  );
}

const meta: Meta<typeof Live> = { title: "Navigation/Sidebar", component: Live };
export default meta;
type S = StoryObj<typeof Live>;

export const Grouped: S = { args: { rows } };
export const NothingYet: S = { args: { rows: [], selected: null } };
export const WithArchived: S = { args: { rows, showArchived: true } };
