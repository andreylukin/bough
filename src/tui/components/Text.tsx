// Themed <Text>: ink has no foreground inheritance from Box, so the default
// text color (palette.text) is applied here instead of leaking the terminal's
// own default fg — on a light-profile terminal the app was near-invisible
// (visual audit P0). `dimColor` maps to palette.muted truecolor rather than
// SGR faint, which is emulator-dependent and fails contrast (P0b). An explicit
// `color` always wins; every component imports Text from here, not from ink.
import { Text as InkText } from "ink";
import type { ComponentProps } from "react";
import { palette } from "../theme.ts";

export function Text({ color, dimColor, ...props }: ComponentProps<typeof InkText>) {
  return <InkText color={color ?? (dimColor ? palette.muted : palette.text)} {...props} />;
}
