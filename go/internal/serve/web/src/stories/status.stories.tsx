import type { Meta, StoryObj } from "@storybook/react-vite";
import { STATUS, StatusMark } from "../status";
import type { Status } from "../types";

const meta: Meta<typeof StatusMark> = {
  title: "Status/StatusMark",
  component: StatusMark,
  parameters: { layout: "padded" },
};
export default meta;
type S = StoryObj<typeof StatusMark>;

export const One: S = { args: { status: "needs-you", size: 13 } };

/** Every state at once. Only amber and red carry a hue; resting states are neutral. */
export const All: S = {
  render: () => (
    <div style={{ display: "grid", gridTemplateColumns: "repeat(2, minmax(0, 1fr))", gap: "14px 32px", maxWidth: 520 }}>
      {(Object.keys(STATUS) as Status[]).map((s) => (
        <div key={s} style={{ display: "flex", alignItems: "center", gap: 12 }}>
          <StatusMark status={s} />
          <span className="mono" style={{ marginLeft: "auto", fontSize: 12, color: "var(--text-3)" }}>{s}</span>
        </div>
      ))}
    </div>
  ),
};
