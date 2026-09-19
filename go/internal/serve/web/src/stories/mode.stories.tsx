import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";
import { ModeChip, ModePicker, OrbStartup, type ModeValue } from "../mode";
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
export const Project: S = { render: () => <Picker start={{ mode: "project", project: "bough" }} /> };
export const NoProjects: S = { render: () => <div className="controls"><ModePicker projects={[]} value={{ mode: "local" }} onChange={() => {}} /></div> };

const statuses: OrbStatus[] = ["building", "starting", "running", "stopped", "failed"];
export const Chips: S = {
  render: () => (
    <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
      {statuses.map((status) => <ModeChip key={status} row={{ ...rows[0], mode: "project", orb: { project: "bough", status } }} />)}
      <ModeChip row={{ ...rows[0], mode: "local" }} />
    </div>
  ),
};

const since = (s: number) => new Date(Date.now() - s * 1000).toISOString();
const steps: [string, number][] = [["sync", 4], ["build", 286], ["worktree", 1], ["container", 3], ["resume.sh", 21]];

/**
 * A busy chip names the step it is on and how long it has been there, so a
 * 286s build does not look the same at second 4 as at minute 4. `bare` is the
 * sidebar; the full chip leads with the project.
 */
export const BusySteps: S = {
  render: () => (
    <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
      {steps.map(([phase, s]) => (
        <ModeChip key={phase} bare row={{ ...rows[0], mode: "project", orb: { project: "bough", status: "building", phase, phaseAt: since(s) } }} />
      ))}
      <ModeChip row={{ ...rows[0], mode: "project", orb: { project: "bough", status: "building", phase: "build", phaseAt: since(286) } }} />
      {/* Phase without a phaseAt (an older server): the plain word, no timer. */}
      <ModeChip row={{ ...rows[0], mode: "project", orb: { project: "bough", status: "starting", phase: "container" } }} />
      {/* An unknown step name falls back to the status word. */}
      <ModeChip bare row={{ ...rows[0], mode: "project", orb: { project: "bough", status: "starting", phase: "mystery", phaseAt: since(9) } }} />
    </div>
  ),
};

/** The start's total, as the popover head carries it: resting grey, amber while it climbs. */
export const Startup: S = {
  render: () => (
    <div style={{ display: "flex", flexDirection: "column", gap: 8, maxWidth: 240 }}>
      {[2030, 12_400, 286_000].map((ms) => (
        <p key={ms} className="orb-pop-head"><span>Orb start</span><OrbStartup ms={ms} /></p>
      ))}
      <p className="orb-pop-head"><span>Orb start</span><OrbStartup ms={41_200} live /></p>
    </div>
  ),
};
