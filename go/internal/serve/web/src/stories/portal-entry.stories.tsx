import type { Meta, StoryObj } from "@storybook/react-vite";
import { PortalButton } from "../app";
import { OrbUp } from "../mode";

const meta: Meta<typeof PortalButton> = {
  title: "Projects/Portal entry",
  component: PortalButton,
  decorators: [(Story) => <div className="app" style={{ padding: 24, display: "block" }}><Story /></div>],
};
export default meta;
type S = StoryObj<typeof PortalButton>;

/* The three things the button can say: nothing is listening, one port is, several are. */
export const Button: S = {
  render: () => (
    <div className="controls" style={{ display: "flex", gap: 8 }}>
      <PortalButton ports={[]} onClick={() => {}} />
      <PortalButton ports={[3000]} onClick={() => {}} />
      <PortalButton ports={[3000, 5173, 8080]} onClick={() => {}} />
    </div>
  ),
};

/* The roll-up: accent on the Projects page, quiet in the sidebar, nothing at zero. */
export const RollUp: S = {
  render: () => (
    <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
      <span><OrbUp n={1} /></span>
      <span><OrbUp n={2} /></span>
      <span><OrbUp n={2} quiet /></span>
      <span>Zero renders nothing: [<OrbUp n={0} />]</span>
    </div>
  ),
};
