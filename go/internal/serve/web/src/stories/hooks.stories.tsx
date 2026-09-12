import type { Meta, StoryObj } from "@storybook/react-vite";
import { HooksView, type Fire, type Hook, type HooksData, type Watcher } from "../hooks";

// Sample data in the shape GET /api/hooks answers. The words are the
// kind the app shows — real file names, real error text.
const minsAgo = (m: number) => new Date(Date.now() - m * 60_000).toISOString();

const watchers: Watcher[] = [
  { name: "ci.js", path: "/Users/a/.bough/watchers/ci.js", every: "60s", failing: false, error: "",
    lastRun: minsAgo(1), lastWoke: minsAgo(34) },
  { name: "inbox.js", path: "/Users/a/.bough/watchers/inbox.js", every: "5m", failing: false, error: "",
    lastRun: minsAgo(3), lastWoke: null },
];

const failing: Watcher = {
  name: "deploys.js", path: "/w/bough/.bough/watchers/deploys.js", every: "30s", failing: true,
  error: "deploys.js:12 TypeError: cannot read property 'status' of undefined",
  lastRun: minsAgo(2), lastWoke: minsAgo(90),
};

const hooks: Hook[] = [
  { name: "guard.js", event: "pre-code-exec", path: "/Users/a/.bough/hooks/guard.js", scope: "home",
    shadowed: false, lastFired: minsAgo(6), lastDecision: "denied", failing: false, error: "" },
  { name: "rules.js", event: "pre-code-exec", path: "/Users/a/.bough/hooks/rules.js", scope: "home",
    shadowed: true, lastFired: minsAgo(240), lastDecision: "", failing: false, error: "" },
  { name: "rules.js", event: "pre-code-exec", path: "/w/bough/.bough/hooks/rules.js", scope: "project",
    shadowed: false, lastFired: minsAgo(5), lastDecision: "rewrote", failing: false, error: "" },
  { name: "notify.js", event: "post-result", path: "/Users/a/.bough/hooks/notify.js", scope: "home",
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

const body = `export default {\n  event: "pre-code-exec",\n  run(ctx) {\n    if (/rm -rf/.test(ctx.code)) return { deny: "no recursive deletes" };\n  },\n};\n`;

const populated: HooksData = { hooks, watchers, fires };

const meta: Meta<typeof HooksView> = {
  title: "Hooks/HooksView",
  component: HooksView,
  args: {
    data: populated,
    load: () => Promise.resolve(body),
    save: () => Promise.resolve(),
    dryrun: () => Promise.resolve({ result: { deny: "no recursive deletes" }, error: "", ms: 4 }),
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
  args: { data: { hooks: [], watchers: [], fires: [] } },
};

/** The narrow pass: every row wraps, nothing scrolls sideways. */
export const Phone: S = {
  globals: { viewport: { value: "mobile1" } },
};
