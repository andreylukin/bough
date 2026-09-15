import type { Meta, StoryObj } from "@storybook/react-vite";
import { STATUS, StatusMark, TESTS_FAILED_GLYPH } from "../status";
import type { Status } from "../types";

const meta: Meta<typeof StatusMark> = {
  title: "Status/StatusMark",
  component: StatusMark,
  parameters: { layout: "padded" },
};
export default meta;
type S = StoryObj<typeof StatusMark>;

export const One: S = { args: { status: "needs-you", size: 13 } };

/** Failed (x-circle) beside tests failed (triangle): the glyph alone separates them. */
export const FailedVsTestsFailed: S = {
  render: () => (
    <div style={{ display: "flex", gap: 24 }}>
      <StatusMark status="error" />
      <span className="status is-failed" style={{ display: "inline-flex", alignItems: "center", gap: 7 }}>
        <svg className="state-mark" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5"
             strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">{TESTS_FAILED_GLYPH}</svg>
        <span style={{ fontSize: 12 }}>Tests failed</span>
      </span>
    </div>
  ),
};

/** Every state at once. Amber waits, red failed, accent runs; resting states are neutral. */
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
