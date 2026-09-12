import type { Meta, StoryObj } from "@storybook/react-vite";
import { HooksView, type Fire, type Hook, type HooksData, type Plugin, type Rule, type Watcher } from "../hooks";

// Sample data in the shape GET /api/hooks answers. The words are the
// kind the app shows — real file names, real error text.
const minsAgo = (m: number) => new Date(Date.now() - m * 60_000).toISOString();

const watchers: Watcher[] = [
  { id: "ci.js", off: false, name: "ci.js", path: "/Users/a/.bough/watchers/ci.js", every: "60s", failing: false, error: "",
    lastRun: minsAgo(1), lastWoke: minsAgo(34) },
  { id: "inbox.js", off: false, name: "inbox.js", path: "/Users/a/.bough/watchers/inbox.js", every: "5m", failing: false, error: "",
    lastRun: minsAgo(3), lastWoke: null },
];

const failing: Watcher = {
  id: "deploys.js", off: false, name: "deploys.js", path: "/w/bough/.bough/watchers/deploys.js", every: "30s", failing: true,
  error: "deploys.js:12 TypeError: cannot read property 'status' of undefined",
  lastRun: minsAgo(2), lastWoke: minsAgo(90),
};

const hooks: Hook[] = [
  { id: "pre-code-exec/guard.js", off: false, name: "guard.js", event: "pre-code-exec", path: "/Users/a/.bough/hooks/guard.js", scope: "home",
    shadowed: false, lastFired: minsAgo(6), lastDecision: "denied", failing: false, error: "" },
  { id: "pre-code-exec/rules.js@home", off: false, name: "rules.js", event: "pre-code-exec", path: "/Users/a/.bough/hooks/rules.js", scope: "home",
    shadowed: true, lastFired: minsAgo(240), lastDecision: "", failing: false, error: "" },
  { id: "pre-code-exec/rules.js@project", off: false, name: "rules.js", event: "pre-code-exec", path: "/w/bough/.bough/hooks/rules.js", scope: "project",
    shadowed: false, lastFired: minsAgo(5), lastDecision: "rewrote", failing: false, error: "" },
  { id: "post-result/notify.js", off: true, name: "notify.js", event: "post-result", path: "/Users/a/.bough/hooks/notify.js", scope: "home",
    shadowed: false, lastFired: null, lastDecision: "", failing: true,
    error: "notify.js: require('node:child_process') is not available to hooks" },
];

const fires: Fire[] = [
  { at: minsAgo(5), session: "s1", event: "pre-code-exec", name: "rules", ms: 3, decision: "rewrote", error: "" },
  { at: minsAgo(6), session: "s1", event: "pre-code-exec", name: "guard", ms: 1, decision: "denied", error: "" },
  { at: minsAgo(12), session: "s2", event: "post-result", name: "notify", ms: 41, decision: "", error: "" },
  { at: minsAgo(18), session: "s2", event: "pre-code-exec", name: "guard", ms: 2, decision: "", error: "" },
  { at: minsAgo(20), session: "s2", event: "post-result", name: "notify", ms: 9, decision: "",
    error: "hook threw after 9ms" },
  { at: minsAgo(33), session: "s3", event: "pre-code-exec", name: "guard", ms: 2, decision: "blocked", error: "" },
];

// A repo rule sitting under the central one of the same name: both are
// in force, which is what the two groups in the view have to make plain.
const rules: Rule[] = [
  { id: "/Users/a/.claude/rules/python-standards.md", name: "python-standards.md",
    path: "/Users/a/.claude/rules/python-standards.md", scope: "home", kind: "scoped",
    globs: ["**/*.py", "**/pyproject.toml"], off: false },
  { id: "/Users/a/.claude/rules/no-force-push.md", name: "no-force-push.md",
    path: "/Users/a/.claude/rules/no-force-push.md", scope: "home", kind: "gate", globs: [], off: false },
  { id: "/Users/a/.claude/rules/tone.md", name: "tone.md", path: "/Users/a/.claude/rules/tone.md",
    scope: "home", kind: "prose", globs: [], off: false },
  { id: "/w/bough/.claude/rules/python-standards.md", name: "python-standards.md",
    path: "/w/bough/.claude/rules/python-standards.md", scope: "repo", kind: "scoped",
    globs: ["go/**/*.py"], off: false },
  { id: "/w/bough/.claude/rules/selected-tests.md", name: "selected-tests.md",
    path: "/w/bough/.claude/rules/selected-tests.md", scope: "repo", kind: "prose", globs: [], off: false },
];

const plugins: Plugin[] = [
  { id: "uni-common@uni-claude-marketplace", name: "uni-common", marketplace: "uni-claude-marketplace",
    version: "0.6.0", scope: "user", projectPath: "", installPath: "/Users/a/.claude/plugins/uni-common",
    present: true, skills: ["circleci", "grill-me"], commands: ["new-component"], off: false },
  { id: "worktrunk@andrey-tools", name: "worktrunk", marketplace: "andrey-tools", version: "1.2.1",
    scope: "project", projectPath: "/w/bough", installPath: "/w/bough/.claude/plugins/worktrunk",
    present: true, skills: ["worktrunk"], commands: [], off: false },
  { id: "user-testing-agent@uni-claude-marketplace", name: "user-testing-agent",
    marketplace: "uni-claude-marketplace", version: "0.3.0", scope: "user", projectPath: "",
    installPath: "/Users/a/.claude/plugins/user-testing-agent", present: false,
    skills: ["user-test"], commands: [], off: false },
];

const body = `export default {\n  event: "pre-code-exec",\n  run(ctx) {\n    if (/rm -rf/.test(ctx.code)) return { deny: "no recursive deletes" };\n  },\n};\n`;

const populated: HooksData = { hooks, watchers, fires, rules, plugins };

const meta: Meta<typeof HooksView> = {
  title: "Hooks/HooksView",
  component: HooksView,
  args: {
    data: populated,
    load: () => Promise.resolve(body),
    save: () => Promise.resolve(),
    dryrun: () => Promise.resolve({ result: { deny: "no recursive deletes" }, error: "", ms: 4 }),
    setOff: () => Promise.resolve(),
  },
  decorators: [(Story) => <div className="app" style={{ height: "100vh" }}><Story /></div>],
};
export default meta;
type S = StoryObj<typeof HooksView>;

export const Populated: S = {};

export const FailingWatcher: S = {
  args: { data: { ...populated, watchers: [failing, ...watchers] } },
};

export const Empty: S = {
  args: { data: { hooks: [], watchers: [], fires: [], rules: [], plugins: [] } },
};

/** A repo rule stacking on the central rule of the same name. */
export const RulesStacking: S = {
  args: { data: { hooks: [], watchers: [], fires: [], rules, plugins: [] } },
};

/** A plugin whose install path holds nothing: listed, and said so. */
export const PluginNotPresent: S = {
  args: { data: { hooks: [], watchers: [], fires: [], rules: [], plugins } },
};

/** Off is a state, not a disappearance: the row stays, and says the word. */
export const ToggledOff: S = {
  args: {
    data: {
      ...populated,
      watchers: [{ ...watchers[0], off: true }, watchers[1]],
      rules: rules.map((r, i) => (i === 3 ? { ...r, off: true } : r)),
      plugins: plugins.map((p, i) => (i === 1 ? { ...p, off: true } : p)),
    },
  },
};

/** The narrow pass: every row wraps, nothing scrolls sideways. */
export const Phone: S = {
  globals: { viewport: { value: "mobile1" } },
};
