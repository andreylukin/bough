import type { StorybookConfig } from "@storybook/react-vite";

// Storybook renders the control room's components in isolation, off
// the same src/ the bundle is built from. No addons on purpose: the
// value is the components under the real stylesheet, not the tooling.
const config: StorybookConfig = {
  stories: ["../src/**/*.stories.tsx"],
  framework: { name: "@storybook/react-vite", options: {} },
  core: { disableTelemetry: true },
};
export default config;
