import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { ModeChip, ModePicker, type ModeValue } from "../mode";
import type { OrbStatus } from "../types";
import { projects, rows } from "./fixtures";

const meta: Meta<typeof ModePicker> = {
  title: "Projects/Mode",
  component: ModePicker,
  decorators: [(Story) => <div className="app" style={{ padding: 24, display: "block" }}><Story /></div>],
};
export default meta;
type S = StoryObj<typeof ModePicker>;

function Picker({ start }: { start: ModeValue }) {
  const [v, setV] = useState(start);
  return <div className="controls"><ModePicker projects={projects} value={v} onChange={setV} /></div>;
}

export const Local: S = { render: () => <Picker start={{ mode: "local" }} /> };
export const Project: S = { render: () => <Picker start={{ mode: "project", project: "p2" }} /> };
export const NoOrbProjects: S = { render: () => <div className="controls"><ModePicker projects={[{ id: "p1", name: "Incident 42" }]} value={{ mode: "local" }} onChange={() => {}} /></div> };

const statuses: OrbStatus[] = ["building", "starting", "running", "stopped", "failed"];
export const Chips: S = {
  render: () => (
    <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
      {statuses.map((status) => <ModeChip key={status} row={{ ...rows[0], mode: "project", orb: { project: "bough", status } }} />)}
      <ModeChip row={{ ...rows[0], mode: "local" }} />
    </div>
  ),
};
