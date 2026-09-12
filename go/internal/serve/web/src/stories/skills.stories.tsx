import type { Meta, StoryObj } from "@storybook/react-vite";
import { SkillPicker } from "../skills";

// The picker opens upward from the composer's button row, so the
// story leaves room above it.
const meta: Meta<typeof SkillPicker> = {
  title: "Composer/SkillPicker",
  component: SkillPicker,
  decorators: [(Story) => <div style={{ padding: "360px 32px 32px", display: "flex", justifyContent: "flex-end" }}><Story /></div>],
  args: { onPick: () => {} },
};
export default meta;
type S = StoryObj<typeof SkillPicker>;

/** Click Skills to open; type to filter; arrows and Return pick. */
export const Closed: S = {};
