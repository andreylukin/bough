// Sample data for stories. Shapes mirror src/types.ts; the words are
// the kind the app shows (session titles, tool output), not lorem.
import type { Line, Project, Row } from "../types";

const hoursAgo = (h: number) => new Date(Date.now() - h * 3_600_000).toISOString();

export const projects: Project[] = [
  { id: "p1", name: "Incident 42" },
  { id: "p2", name: "Control room", slug: "bough", orb: { slug: "bough", image: "bough-orb/bough:3f9a2c71d0be", built: true, build: "ok" } },
];

export const rows: Row[] = [
  { id: "s1", title: "Fix the flaky PTY test", cwd: "/w/bough", repo: "andreylukin/bough", branch: "main",
    status: "running", live: true, archived: false, entries: 12, modified: hoursAgo(1), lastAt: hoursAgo(1), project: "p2",
    model: "claude-sonnet-5", effort: "default" },
  { id: "s2", title: "Which migration order is safe?", cwd: "/w/api", repo: "acme/api", branch: "db-split",
    status: "needs-you", live: true, archived: false, entries: 8, modified: hoursAgo(3), lastAt: hoursAgo(3), project: "p1",
    ask: { id: "a1", text: "The migration touches two tables. Run it against staging first?",
           options: ["Yes, staging first", "No, straight to prod"], seq: 8 } },
  { id: "s3", title: "## Wiki compile from history", cwd: "/w/bough", repo: "andreylukin/bough",
    status: "done", live: false, archived: false, entries: 40, modified: hoursAgo(26), lastAt: hoursAgo(26) },
  { id: "s4", title: "Headless SIGPIPE on closed stdout", cwd: "/w/bough", repo: "andreylukin/bough",
    branch: "headless-pipe", status: "error", live: false, archived: false, entries: 5, modified: hoursAgo(30), lastAt: hoursAgo(30),
    project: "p1" },
  { id: "s5", title: "Rename the scratch dir", cwd: "/w/bough", status: "stopped", live: false,
    archived: false, entries: 3, modified: hoursAgo(24 * 5), lastAt: hoursAgo(24 * 5) },
  // Recorded test failures: every surface must rank these as needing you,
  // past 72h, in Background, and after being marked seen (no trouble).
  { id: "s7", title: "Retry budget for the TLS dialer", cwd: "/w/bough", repo: "andreylukin/bough", status: "done",
    testsFailed: true, trouble: "tests failed", live: false, archived: false, entries: 9, modified: hoursAgo(24 * 4), lastAt: hoursAgo(24 * 4) },
  { id: "s8", title: "Nightly bench sweep", cwd: "/w/bench", status: "done", background: true, testsFailed: true,
    live: false, archived: false, entries: 6, modified: hoursAgo(2), lastAt: hoursAgo(2) },
  { id: "s9", title: "Seen but still red: vtreal load", cwd: "/w/bough", repo: "andreylukin/bough", status: "done",
    testsFailed: true, live: false, archived: false, entries: 4, modified: hoursAgo(24 * 6), lastAt: hoursAgo(24 * 6) },
  { id: "s6", title: "Old spike on goja perf", cwd: "/w/bough", status: "idle", live: false,
    archived: true, entries: 2, modified: hoursAgo(24 * 30), lastAt: hoursAgo(24 * 30) },
];

const code = 'tools.bash("go test -race ./plugins/todo/")';

/** One whole turn: prompt, reasoning, reply, code, result, jobs, error, done. */
export const turn: Line[] = [
  { seq: 1, at: hoursAgo(1), kind: "input", text: "Make the todo plugin's test run under a second." },
  { seq: 2, at: hoursAgo(1), kind: "thinking",
    text: "The suite sleeps for the file watcher; a fake clock removes that.\n\n- check whether the watcher is injectable\n- `kernel/testclock` already exists" },
  { seq: 3, at: hoursAgo(1), kind: "assistant",
    text: "The watcher already takes a `clock`. I'll pass the fake one from `kernel/testclock` and drop the sleep.\n\n```js\n" + code + "\n```" },
  { seq: 4, at: hoursAgo(1), kind: "code", text: code },
  { seq: 5, at: hoursAgo(1), kind: "result", text: code + "\nok  \tbough/plugins/todo\t0.41s", data: { code } },
  { seq: 6, at: hoursAgo(1), kind: "job",
    text: "job 49 [exited 0] git push -u origin todo-fast (3s)\nTo github.com:andreylukin/bough.git\n * [new branch]      todo-fast -> todo-fast" },
  { seq: 7, at: hoursAgo(1), kind: "job",
    text: "job 50 [exited 1] go vet ./... (1s)\nplugins/todo/todo.go:41:2: unusedresult: result of fmt.Sprintf call not used" },
  { seq: 8, at: hoursAgo(1), kind: "error", text: "todo: clock must not be nil" },
  { seq: 9, at: hoursAgo(1), kind: "sub:assistant", text: "Checked the other three watchers; none sleep." },
  { seq: 10, at: hoursAgo(1), kind: "usage", text: "usage · 12.4k in, 1.1k out" },
  { seq: 11, at: hoursAgo(1), kind: "done", text: "", data: { files: ["plugins/todo/todo_test.go"] } },
];

/**
 * A turn where the work was farmed out. Two subagents run at once and
 * their entries interleave step by step — worker 1's result lands
 * between worker 2's code and its reply — which is exactly the shape
 * that reads as nonsense when it is rendered flat.
 */
const subCode1 = 'tools.bash("grep -rn \\"deltaWindow\\" internal/serve/")';
const subCode2 = 'tools.bash("go test ./internal/serve/ -run Delta")';

export const subTurn: Line[] = [
  { seq: 1, at: hoursAgo(1), kind: "input", text: "Check the delta stream lands and the tests cover it." },
  { seq: 2, at: hoursAgo(1), kind: "assistant", text: "Two things to check; I'll run them side by side." },
  // On the wire a start's task is Line.text (serve lifts data.text out
  // with history.EntryText); data keeps the worker lane.
  { seq: 3, at: hoursAgo(1), kind: "sub:start", text: "Find every use of deltaWindow and say what sets it.", data: { worker: 1 } },
  { seq: 4, at: hoursAgo(1), kind: "sub:start", text: "Run the serve delta tests and report failures verbatim.", data: { worker: 2 } },
  { seq: 5, at: hoursAgo(1), kind: "sub:assistant", text: "Grepping for the constant first.", data: { worker: 1 } },
  { seq: 6, at: hoursAgo(1), kind: "sub:code", text: subCode1, data: { worker: 1 } },
  { seq: 7, at: hoursAgo(1), kind: "sub:code", text: subCode2, data: { worker: 2 } },
  { seq: 8, at: hoursAgo(1), kind: "sub:result", data: { worker: 1, code: subCode1 },
    text: subCode1 + "\ndeltas.go:27:const deltaWindow = 50 * time.Millisecond\ndeltas.go:64:\ttime.AfterFunc(deltaWindow, ...)" },
  { seq: 9, at: hoursAgo(1), kind: "sub:result", data: { worker: 2, code: subCode2 },
    text: subCode2 + "\n--- FAIL: TestDeltaFlushBeforeRecorded (0.00s)" },
  { seq: 10, at: hoursAgo(1), kind: "sub:assistant", text: "One definition, one use: the flush timer. Nothing else reads it.", data: { worker: 1 } },
  { seq: 11, at: hoursAgo(1), kind: "sub:error", text: "go: exit status 1", data: { worker: 2 } },
  { seq: 12, at: hoursAgo(1), kind: "sub:done", text: "", data: { worker: 1, status: "ok", steps: 2 } },
  { seq: 13, at: hoursAgo(1), kind: "sub:done", text: "", data: { worker: 2, status: "error", steps: 2 } },
  { seq: 14, at: hoursAgo(1), kind: "assistant", text: "The constant is only read by the flush timer, and `TestDeltaFlushBeforeRecorded` is red. I'll look at the ordering next." },
  { seq: 15, at: hoursAgo(1), kind: "done", text: "" },
];

/** A turn caught mid-reply: nothing of it is recorded yet. */
export const openTurn: Line[] = [
  { seq: 1, at: hoursAgo(0), kind: "input", text: "Why does the browser show a duplicate tail after a turn lands?" },
];

export const streamRuns = [
  { kind: "thinking" as const,
    text: "The flush is on a 50ms timer, so a buffered fragment can be delivered *after* the `assistant` line it was building." },
  { kind: "assistant" as const,
    text: "Because the delta is late, not wrong. The recorded entry renders, then the stray fragment append" },
];

export const markdown = `## What I found

The watcher already takes a \`clock\`, so the fix is one line in the test.

- \`plugins/todo/todo_test.go\` sleeps 800ms for the first tick
- \`kernel/testclock\` advances by hand

| package | before | after |
|---|---|---|
| plugins/todo | 1.9s | 0.41s |

> Rendering is asserted in the teatest layer, not only in the data.

See [the plugin guide](https://example.invalid/PLUGINS.md).`;

export const skills = [
  { name: "grill-me", summary: "Interrogate a design until every assumption has been said out loud.", manual: true },
  { name: "review", summary: "Read the diff on this branch the way a strict reviewer would.", manual: true },
  { name: "wiki", summary: "Compile the session wiki from history.", manual: false },
  { name: "loop", summary: "Run a prompt on an interval.", manual: true },
];

export const models = {
  providers: [
    { plugin: "llm-anthropic", models: [
      { id: "claude-sonnet-5", context: 200_000, efforts: ["low", "medium", "high"] },
      { id: "claude-opus-5", context: 200_000 },
    ] },
    { plugin: "llm-echo", models: [{ id: "echo" }] },
  ],
  efforts: ["low", "medium", "high"],
};

/**
 * Controls and SkillPicker call fetch on mount. Storybook has no
 * server, so answer the two endpoints from the fixtures above and let
 * everything else fall through to the real fetch.
 */
export function installFakeApi(): void {
  const real = globalThis.fetch.bind(globalThis);
  const routes: Record<string, unknown> = {
    "/api/models": models,
    "/api/skills": { skills },
    "/api/files?q=del&session=s1": { files: [
      { path: "internal/serve/deltas.go", dir: false },
      { path: "internal/serve/deltas_test.go", dir: false },
      { path: "internal/serve/web/design/", dir: true },
    ] },
  };
  globalThis.fetch = (input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.pathname : input.url;
    const hit = routes[url];
    if (hit !== undefined) {
      return Promise.resolve(new Response(JSON.stringify(hit), { headers: { "Content-Type": "application/json" } }));
    }
    return real(input, init);
  };
}
