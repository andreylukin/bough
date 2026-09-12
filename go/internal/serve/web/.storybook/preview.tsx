import type { Preview } from "@storybook/react-vite";
// The one stylesheet the app has, copied out of dist/index.html by
// `bun run design:sync`. Stories and the Claude Design cards share it,
// so both drift together and design:check catches both.
import "../design/bough.css";
import { installFakeApi } from "../src/stories/fixtures";

// Controls and SkillPicker fetch /api/models and /api/skills on mount.
// Answer them from fixtures rather than letting them fail quietly.
installFakeApi();

const preview: Preview = {
  parameters: {
    layout: "fullscreen",
    backgrounds: { disable: true },
  },
  decorators: [
    (Story) => (
      // The app is dark by commitment: every colour is painted. The
      // shell root fills the iframe so layout stories get their height.
      <div style={{ background: "var(--bg)", color: "var(--text-1)", minHeight: "100vh" }}>
        <Story />
      </div>
    ),
  ],
};
export default preview;
