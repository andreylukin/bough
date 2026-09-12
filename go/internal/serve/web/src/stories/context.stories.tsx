import type { Meta, StoryObj } from "@storybook/react-vite";
import { ContextView, type ContextData } from "../context";
import type { Rule } from "../hooks";

// Sample data in the shape GET /api/sessions/{id}/context answers: what
// is shaping one conversation, resolved against that session's cwd.

const rules: Rule[] = [
  { id: "/Users/a/.claude/rules/tone.md", name: "tone.md", path: "/Users/a/.claude/rules/tone.md",
    scope: "home", kind: "prose", globs: [], off: false },
  { id: "/Users/a/.claude/rules/no-force-push.md", name: "no-force-push.md",
    path: "/Users/a/.claude/rules/no-force-push.md", scope: "home", kind: "gate", globs: [], off: false },
  { id: "/w/bough/.claude/rules/selected-tests.md", name: "selected-tests.md",
    path: "/w/bough/.claude/rules/selected-tests.md", scope: "repo", kind: "scoped",
    globs: ["go/**/*_test.go"], off: true },
];

const populated: ContextData = {
  cwd: "/w/bough",
  rules,
  contextFiles: [
    { path: "/w/bough/AGENTS.md", found: true, dropped: 0, same: "" },
    { path: "/w/bough/CLAUDE.md", found: true, dropped: 3, same: "/w/bough/AGENTS.md" },
    { path: "/w/bough/go/AGENTS.md", found: true, dropped: 0, same: "" },
    { path: "/Users/a/CLAUDE.md", found: false, dropped: 0, same: "" },
  ],
  skills: [
    { id: "circleci", name: "circleci", summary: "Read CircleCI job output for a branch or PR.",
      source: "plugin", off: false },
    { id: "shell-use", name: "shell-use", summary: "Drive a real terminal from the command line.",
      source: "pool", off: false },
    { id: "gbass", name: "gbass", summary: "Send a GarageBand bounce to Gemini for review.",
      source: "pool", off: true },
  ],
};

const meta: Meta<typeof ContextView> = {
  title: "Context/ContextView",
  component: ContextView,
  args: {
    data: populated,
    load: () => Promise.resolve("Run only the packages you touched.\n"),
    save: () => Promise.resolve(),
    setOff: () => Promise.resolve(),
  },
  decorators: [(Story) => <div className="app" style={{ height: "100vh" }}><Story /></div>],
};
export default meta;
type S = StoryObj<typeof ContextView>;

export const Populated: S = {};

/** A fresh directory: three empty states that name what to write, and where. */
export const Empty: S = {
  args: { data: { cwd: "/Users/a/scratch", rules: [], contextFiles: [], skills: [] } },
};

/** The narrow pass: every row wraps, nothing scrolls sideways. */
export const Phone: S = {
  globals: { viewport: { value: "mobile1" } },
};
