// Sample data for stories. Shapes mirror src/types.ts; the words are
// the kind the app shows (session titles, tool output), not lorem.
import type { Line, Project, Row } from "../types";

const hoursAgo = (h: number) => new Date(Date.now() - h * 3_600_000).toISOString();

export const projects: Project[] = [
  { slug: "incident-42", name: "Incident 42", orb: { slug: "incident-42", image: "bough-orb/incident-42:0c1d2e3f4a5b", built: false } },
  { slug: "bough", name: "Control room", orb: { slug: "bough", image: "bough-orb/bough:3f9a2c71d0be", built: true, build: "ok" } },
];

export const rows: Row[] = [
  { id: "s1", title: "Fix the flaky PTY test", cwd: "/w/bough", repo: "andreylukin/bough", branch: "main",
    status: "running", live: true, archived: false, entries: 12, modified: hoursAgo(1), lastAt: hoursAgo(1), project: "bough",
    model: "claude-sonnet-5", effort: "default" },
  { id: "s2", title: "Which migration order is safe?", cwd: "/w/api", repo: "acme/api", branch: "db-split",
    status: "needs-you", live: true, archived: false, entries: 8, modified: hoursAgo(3), lastAt: hoursAgo(3), project: "incident-42",
    ask: { id: "a1", text: "The migration touches two tables. Run it against staging first?",
           options: ["Yes, staging first", "No, straight to prod"], seq: 8 } },
  { id: "s3", title: "## Wiki compile from history", cwd: "/w/bough", repo: "andreylukin/bough",
    status: "done", live: false, archived: false, entries: 40, modified: hoursAgo(26), lastAt: hoursAgo(26) },
  { id: "s4", title: "Headless SIGPIPE on closed stdout", cwd: "/w/bough", repo: "andreylukin/bough",
    branch: "headless-pipe", status: "error", live: false, archived: false, entries: 5, modified: hoursAgo(30), lastAt: hoursAgo(30),
    project: "incident-42" },
  { id: "s5", title: "Rename the scratch dir", cwd: "/w/bough", status: "stopped", live: false,
    archived: false, entries: 3, modified: hoursAgo(24 * 5), lastAt: hoursAgo(24 * 5) },
  // Recorded test failures: every surface must rank these as needing you,
  // past 72h (never folded under "older"), in Background, and after being
  // marked seen (no trouble).
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
  // 14s after the thinking block: it reads "Thought for 14s".
  { seq: 3, at: new Date(Date.parse(hoursAgo(1)) + 14_000).toISOString(), kind: "assistant",
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
  { seq: 11, at: hoursAgo(1), kind: "done", text: "",
    data: { files: ["plugins/todo/todo_test.go", "plugins/todo/todo.go", "kernel/testclock/clock.go"], usage: { in: 12400, out: 1100, cost: 0.05, last_in: 12400 } } },
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
let buildTick = 0, buildOffset = 0;
const buildStarted = new Date(Date.now() - 83_000).toISOString();

export function installFakeApi(): void {
  const real = globalThis.fetch.bind(globalThis);
  const routes: Record<string, unknown> = {
    "/api/models": models,
    "/api/skills?session=s1": { skills },
    "/api/files?q=del&session=s1": { files: [
      { path: "internal/serve/deltas.go", dir: false },
      { path: "internal/serve/deltas_test.go", dir: false },
      { path: "internal/serve/web/design/", dir: true },
    ] },
  };
  const json = (v: unknown, status = 200) => new Response(JSON.stringify(v), { status, headers: { "Content-Type": "application/json" } });
  const later = (ms: number, r: () => Response) => new Promise<Response>((ok) => setTimeout(() => ok(r()), ms));
  // The Work stories' endpoints: children found, still loading, or down;
  // stops accepted, refused, or slow enough for the job to end first.
  const patterns: [RegExp, () => Response | Promise<Response>][] = [
    // Session edits, so a turn's files chip can show per-file counts.
    [/^\/api\/sessions\/s1\/edits$/, () => json({ repo: true, files: [
      { path: "plugins/todo/todo_test.go", add: 18, del: 9, patch: true },
      { path: "plugins/todo/todo.go", add: 4, del: 1, patch: true },
      { path: "kernel/testclock/clock.go", add: 12, del: 0, patch: true, new: true },
    ] })],
    [/^\/api\/sessions\/w-loading\/children$/, () => new Promise<Response>(() => {})],
    [/^\/api\/sessions\/w-down\/children$/, () => json({ error: "supervisor unavailable" }, 503)],
    [/^\/api\/sessions\/w-none\/children$/, () => json({ children: [] })],
    [/^\/api\/sessions\/w-local\/children$/, () => json({ children: [] })],
    [/^\/api\/sessions\/w-all\/children$/, () => json({ children: workChildren })],
    [/^\/api\/sessions\/w-twenty\/children$/, () => json({ children: twentyChildren })],
    [/^\/api\/sessions\/p-lead\/children$/,() => json({ children: agentRows.filter((r) => r.spawnedBy === "p-lead") })],
    [/^\/api\/sessions\/[^/]+\/agent\?/, () => later(600, () => json({ status: "done", title: "", reply: "Every handler writes through `writeJSON`; two call sites set the status first.", project: "", spawnedBy: "w-all" }))],
    [/^\/api\/sessions\/w-stopfail\/jobs\/\d+\/kill$/, () => later(400, () => json({ error: "job 7 is not running" }, 500))],
    [/^\/api\/sessions\/w-natural\/jobs\/\d+\/kill$/, () => later(4000, () => json({ ok: true }))],
    [/^\/api\/sessions\/[^/]+\/jobs\/\d+\/kill$/, () => later(300, () => json({ ok: true }))],
    [/^\/api\/sessions\/[^/]+\/stop$/, () => later(300, () => json({ ok: true, was: "running" }))],
    [/^\/api\/sessions\/gone-parent$/, () => json({ error: "no such session" }, 404)],
    // The orb stories: a failed setup's log, and a build that keeps printing.
    [/^\/api\/sessions\/orb-failed\/orb\/log$/, () => json({ status: "failed", error: "resume.sh: exit status 1", image: "bough-orb/bough:3f9a2c71d0be",
      text: "== resume.sh start 2026-09-14T19:03:45Z\nresume.sh: line 6: DEVPI_URL: package index URL required\n== resume.sh end 2026-09-14T19:03:45Z duration 142ms: exit status 1\n" })],
    [/^\/api\/sessions\/orb-building\/orb\/build\/log\?offset=\d+$/, () => {
      buildTick++;
      const text = buildTick === 1 ? "#6 [linux/arm64 2/9] RUN bash /bough-setup/steps/01-apt.sh\n" : `#6 ${(buildTick * 1.7).toFixed(1)} Setting up package ${buildTick}\n`;
      buildOffset += text.length;
      return json({ text, offset: buildOffset, state: "building", status: "building", startedAt: buildStarted });
    }],
    [/^\/api\/projects\/[^/]+\/orb\/build$/, () => later(300, () => json({ build: { state: "building" } }, 202))],
  ];
  globalThis.fetch = (input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.pathname : input.url;
    const hit = routes[url];
    if (hit !== undefined) return Promise.resolve(json(hit));
    const path = url.replace(/^https?:\/\/[^/]+/, "");
    const p = patterns.find(([re]) => re.test(path));
    if (p) return Promise.resolve(p[1]());
    return real(input, init);
  };
}

/** A session with background agents: one running here, one handed off to a project orb, one queued. */
export const agentRows: Row[] = [
  { id: "p-lead", title: "Split the serve API by resource", cwd: "/w/bough", repo: "andreylukin/bough", branch: "main",
    status: "running", live: true, archived: false, entries: 20, modified: hoursAgo(0.2), lastAt: hoursAgo(0.2), mode: "local",
    agents: { running: 2, queued: 1, total: 4 } },
  { id: "k-routes01", title: "Map every route in api.go", cwd: "/w/bough", repo: "andreylukin/bough", status: "running",
    live: true, archived: false, entries: 6, modified: hoursAgo(0.1), lastAt: hoursAgo(0.1), mode: "local", spawnedBy: "p-lead" },
  { id: "k-tests002", title: "Move handler tests to per-file suites", cwd: "/w/bough", repo: "andreylukin/bough", status: "running",
    live: true, archived: false, entries: 9, modified: hoursAgo(0.1), lastAt: hoursAgo(0.1), mode: "project",
    orb: { project: "bough", status: "running" }, spawnedBy: "p-lead" },
  { id: "k-docs0003", title: "", cwd: "/w/bough", repo: "andreylukin/bough", status: "queued", queued: true,
    live: false, archived: false, entries: 0, modified: hoursAgo(0.1), lastAt: hoursAgo(0.1), mode: "local", spawnedBy: "p-lead" },
  { id: "k-grep0004", title: "Find callers of writeJSON", cwd: "/w/bough", repo: "andreylukin/bough", status: "done",
    live: false, archived: false, entries: 4, modified: hoursAgo(0.5), lastAt: hoursAgo(0.5), mode: "local", spawnedBy: "p-lead" },
];

/* ---------------- background work ---------------- */

const minsAgo = (m: number) => new Date(Date.now() - m * 60_000).toISOString();
let workSeq = 100;
const L = (kind: string, text: string, data?: Record<string, unknown>, at = minsAgo(5)): Line => ({ seq: ++workSeq, at, kind, text, data });

/** A session row for the Work stories; live and local unless told otherwise. */
export const workRow = (over: Partial<Row>): Row => ({
  id: "w-all", title: "Split the serve API by resource", cwd: "/w/bough", repo: "andreylukin/bough", branch: "main",
  status: "running", live: true, archived: false, entries: 30, modified: minsAgo(1), lastAt: minsAgo(1), mode: "local", ...over,
});

/** Typed job records, as plugins/tools/jobs.go writes them: no output, only the facts. */
export const typedJob = (id: number, event: "started" | "finished", extra: Record<string, unknown> = {}, at?: string): Line =>
  L("job", "", { id, event, cmd: "bun test --filter serve", ...extra }, at);

/** The loop's text notice for a job that ended. */
export const legacyJob = (text: string, at?: string): Line => L("job", text, undefined, at);

export const jobTyped0 = typedJob(31, "finished", { exit: 0 });
export const jobTyped2 = typedJob(32, "finished", { exit: 2, cmd: "go vet ./internal/serve/..." });
export const jobTypedNoExit = typedJob(33, "finished", { cmd: "make check" });
export const jobLegacyFailed = legacyJob("job 52 [failed] make check (4s): exit status 2\nmake: *** [check] Error 2\ninternal/serve/api.go:88:3: undefined: writeJSON");

/** Two jobs still running and two that failed, one after another in a turn. */
export const jobGroupLines: Line[] = [
  L("input", "Run the checks in the background while you read the handlers."),
  typedJob(7, "started", { cmd: "npm test -- --watch=false" }, minsAgo(2)),
  typedJob(8, "started", { cmd: "go test -race ./internal/serve/..." }, minsAgo(2)),
  legacyJob("job 6 [exited 1] bun test ./test/work.test.ts (1s)\nerror: Cannot find module './fixtures/config'"),
  legacyJob("job 5 [failed] make lint (3s): exit status 2\nlint: 4 issues"),
  // A script that opens with a comment, failing with tree output and a scratch path.
  typedJob(9, "started", { cmd: "# regenerate the fixtures\ncd web && bun run gen --out \"$BOUGH_SCRATCH/gen\"\nbun test ./test/gen.test.ts" }, minsAgo(1)),
  typedJob(9, "finished", { cmd: "# regenerate the fixtures\ncd web && bun run gen --out \"$BOUGH_SCRATCH/gen\"\nbun test ./test/gen.test.ts", exit: 1 }, minsAgo(1)),
  legacyJob("job 9 [exited 1] # regenerate the fixtures … (2s)\nwrote /home/dev/.bough/scratch/3f9c2a7e-51d4/gen/out.json\n└──"),
];
export const jobGroupRunning = [{ id: 7, cmd: "npm test -- --watch=false", started: minsAgo(2) }, { id: 8, cmd: "go test -race ./internal/serve/...", started: minsAgo(2) }];

const turnOf = (prompt: string, body: Line[], done: boolean): Line[] =>
  [L("input", prompt), ...body, ...(done ? [L("done", "")] : [])];

const subCode = 'tools.bash("grep -rn writeJSON internal/serve/")';

/** Subagent turns, one per state a card can be in. */
export const subStates = {
  finishedStepError: turnOf("Map the handlers.", [
    L("sub:start", "Read previous bough session transcripts and list every handler that writes JSON.", { worker: "1" }, minsAgo(9)),
    L("sub:code", subCode, { worker: "1" }, minsAgo(8)),
    L("sub:result", subCode + "\napi.go:88: writeJSON(w, 200, rows)", { worker: "1", code: subCode }, minsAgo(8)),
    L("sub:error", "grep: internal/serve/old/: No such file or directory", { worker: "1" }, minsAgo(8)),
    L("sub:assistant", "Two call sites.\n\n[the 3 code block(s) after this one were not run: the reply ran one block per step]", { worker: "1" }, minsAgo(7)),
    L("sub:done", "", { worker: "1", status: "ok", steps: 9, text: "`writeJSON` is called from `api.go:88` and `children.go:41`; both set the status first." }, minsAgo(5)),
  ], true),
  runningNoSteps: turnOf("Check the migrations.", [
    L("sub:start", "Read every migration under db/ and say which ones are not reversible.", { worker: "1" }, minsAgo(0.2)),
  ], false),
  missingResult: turnOf("Rename the helper.", [
    L("sub:start", "Rename writeJSON to respondJSON across internal/serve.", { worker: "1" }, minsAgo(6)),
    L("sub:code", subCode, { worker: "1" }, minsAgo(5)),
    L("sub:done", "", { worker: "1", status: "ok", steps: 1 }, minsAgo(4)),
  ], true),
  failed: turnOf("Run the delta tests.", [
    L("sub:start", "Run the serve delta tests and report failures verbatim.", { worker: "2" }, minsAgo(4)),
    L("sub:code", 'tools.bash("go test ./internal/serve/ -run Delta")', { worker: "2" }, minsAgo(3.5)),
    L("sub:error", "--- FAIL: TestDeltaFlushBeforeRecorded (0.00s)\n    deltas_test.go:61: flushed 2 fragments after the recorded entry", { worker: "2" }, minsAgo(3)),
    L("sub:done", "", { worker: "2", status: "error", steps: 2 }, minsAgo(3)),
  ], true),
  stopped: turnOf("Sweep the benches.", [
    L("sub:start", "Run the nightly bench sweep and summarise regressions.", { worker: "1" }, minsAgo(12)),
    L("sub:code", 'tools.bash("bun run bench --all")', { worker: "1" }, minsAgo(11)),
    L("sub:result", 'tools.bash("bun run bench --all")\nbench 14/40 · delta-flush 1.9ms', { worker: "1", code: 'tools.bash("bun run bench --all")' }, minsAgo(10)),
    L("sub:done", "", { worker: "1", status: "stopped", steps: 2 }, minsAgo(10)),
  ], true),
  unknown: turnOf("Find the flake.", [
    L("sub:start", "Reproduce the vtreal load flake 20 times and record each exit.", { worker: "1" }, minsAgo(40)),
    L("sub:code", 'tools.bash("go test -count 20 ./internal/vtreal/")', { worker: "1" }, minsAgo(39)),
  ], true),
  waiting: turnOf("Split the API by resource.", [
    L("sub:start", "Move the session handlers into sessions.go.", { worker: "1" }, minsAgo(3)),
    L("sub:start", "Move the project handlers into projects.go.", { worker: "2" }, minsAgo(3)),
    L("sub:start", "List every route that has no test.", { worker: "3" }, minsAgo(3)),
    L("sub:code", 'tools.read("internal/serve/api.go")', { worker: "1" }, minsAgo(1)),
    L("sub:code", subCode, { worker: "3" }, minsAgo(2)),
    L("sub:done", "", { worker: "3", status: "ok", steps: 1, text: "Nine routes have no test; `/children` and `/agent` among them." }, minsAgo(1)),
  ], false),
};

/** Background agents of w-all: one running, one queued with its task, one untitled queued, one finished with a report. */
export const workChildren: Row[] = [
  workRow({ id: "k-run0001", title: "Move handler tests to per-file suites", status: "running", spawnedBy: "w-all", lastAt: minsAgo(6) }),
  workRow({ id: "k-que0002", title: "Write the migration notes", status: "queued", queued: true, live: false, spawnedBy: "w-all", entries: 0, lastAt: minsAgo(2) }),
  workRow({ id: "k-que0003", title: "", status: "queued", queued: true, live: false, spawnedBy: "w-all", entries: 0, lastAt: minsAgo(1) }),
  workRow({ id: "k-don0004", title: "Find callers of writeJSON", status: "done", live: false, spawnedBy: "w-all", lastAt: minsAgo(20) }),
];

/** Every Work group at once: needs review, running, queued, history. */
export const workAllLines: Line[] = [
  ...subStates.unknown,
  L("input", "Keep going."),
  typedJob(21, "started", { cmd: "bun run build --watch" }, minsAgo(3)),
  legacyJob("job 20 [exited 1] go test ./internal/serve/ (6s)\n--- FAIL: TestChildrenQueued (0.02s)"),
  legacyJob("job 19 [exited 0] bun run check (12s)\n$ tsc --noEmit\n$ node design/check.mjs\nok"),
  L("sub:start", "Summarise what the /children endpoint returns.", { worker: "1" }, minsAgo(3)),
  L("sub:code", subCode, { worker: "1" }, minsAgo(3)),
  L("sub:done", "", { worker: "1", status: "ok", steps: 1, text: "Direct children only, queued ones included." }, minsAgo(2)),
  L("notice", "[agent Find callers of writeJSON · k-don0004 finished] Two call sites: api.go:88 and children.go:41."),
];

/** Twenty workers running at once: eight jobs, six subagents, six background agents. */
export const twentyLines: Line[] = [
  L("input", "Fan out: every package, every check."),
  ...Array.from({ length: 8 }, (_, i) => typedJob(60 + i, "started", { cmd: `go test ./internal/${["serve", "kernel", "tui", "models", "history", "vtreal", "rules", "wiki"][i]}/...` }, minsAgo(4))),
  ...Array.from({ length: 6 }, (_, i) => L("sub:start", `Review package ${["api", "deltas", "children", "orb", "status", "supervisor"][i]}.go for unchecked errors.`, { worker: String(i + 1) }, minsAgo(4))),
  ...Array.from({ length: 6 }, (_, i) => L("sub:code", `tools.read("internal/serve/${["api", "deltas", "children", "orb", "status", "supervisor"][i]}.go")`, { worker: String(i + 1) }, minsAgo(3))),
];
export const twentyJobs = Array.from({ length: 8 }, (_, i) => ({ id: 60 + i, cmd: "go test", started: minsAgo(4) }));
export const twentyChildren: Row[] = Array.from({ length: 6 }, (_, i) =>
  workRow({ id: `k-fan000${i}`, title: `Port ${["sessions", "projects", "hooks", "wiki", "orbs", "search"][i]} handlers to the new router`, spawnedBy: "w-twenty", lastAt: minsAgo(4 - i * 0.5) }));

/** The Background section with work in it: one running, one failed, one quiet. */
export const backgroundRows: Row[] = [
  rows[0],
  { id: "b1", title: "Nightly wiki ingest", cwd: "/w/bough", status: "running", background: true, live: true, archived: false, entries: 3, modified: minsAgo(3), lastAt: minsAgo(3) },
  { id: "b2", title: "Bench sweep · haiku", cwd: "/w/bench", status: "error", background: true, live: false, archived: false, entries: 6, modified: minsAgo(50), lastAt: minsAgo(50) },
  { id: "b3", title: "Replay scan", cwd: "/w/bough", status: "done", background: true, live: false, archived: false, entries: 4, modified: minsAgo(200), lastAt: minsAgo(200) },
];

/* Work segments: the stretches between replies, folded to one row each. */
const segAt = (base: number, s: number) => new Date(base + s * 1000).toISOString();
const sb = (c: string) => `tools.bash("${c}")`;
const segBase = Date.parse(hoursAgo(1));

/** A long finished turn: three stretches of work, two replies between them. */
export const segmentTurn: Line[] = [
  { seq: 1, at: segAt(segBase, 0), kind: "input", text: "The session list flickers on every refresh. Find out why and fix it." },
  { seq: 2, at: segAt(segBase, 2), kind: "thinking", text: "Probably a re-sort on every poll; check the list's key and the poll handler." },
  { seq: 3, at: segAt(segBase, 6), kind: "code", text: sb("rg -n \\\"sortRows\\\" src") },
  { seq: 4, at: segAt(segBase, 7), kind: "result", text: "src/app.tsx:141: sortRows(rows)", data: { code: sb("rg -n \\\"sortRows\\\" src"), exit: 0, ms: 90 } },
  { seq: 5, at: segAt(segBase, 12), kind: "code", text: 'tools.view("src/app.tsx")' },
  { seq: 6, at: segAt(segBase, 13), kind: "result", text: "…", data: { code: 'tools.view("src/app.tsx")', exit: 0 } },
  { seq: 7, at: segAt(segBase, 20), kind: "todo/add", text: "Keep the sort stable across polls", data: { id: 1 } },
  { seq: 8, at: segAt(segBase, 20), kind: "todo/add", text: "Add a test for the order", data: { id: 2 } },
  { seq: 9, at: segAt(segBase, 24), kind: "assistant", text: "The list re-sorts by `lastAt` on every poll, and rows with equal times swap places. I'll break ties by id." },
  { seq: 10, at: segAt(segBase, 30), kind: "code", text: 'tools.patch("src/app.tsx", "…")' },
  { seq: 11, at: segAt(segBase, 31), kind: "result", text: "patched", data: { code: 'tools.patch("src/app.tsx", "…")', exit: 0 } },
  { seq: 12, at: segAt(segBase, 40), kind: "code", text: sb("bun test test/sort.test.ts") },
  { seq: 13, at: segAt(segBase, 52), kind: "result", text: "1 fail\nexpected s2 before s1", data: { code: sb("bun test test/sort.test.ts"), exit: 1, ms: 11800 } },
  { seq: 14, at: segAt(segBase, 60), kind: "thinking", text: "Ties compare ids as numbers; they are strings." },
  { seq: 15, at: segAt(segBase, 70), kind: "code", text: sb("bun test test/sort.test.ts") },
  { seq: 16, at: segAt(segBase, 81), kind: "result", text: "4 pass", data: { code: sb("bun test test/sort.test.ts"), exit: 0, ms: 10900 } },
  { seq: 17, at: segAt(segBase, 82), kind: "todo/done", text: "", data: { id: 1 } },
  { seq: 18, at: segAt(segBase, 82), kind: "todo/done", text: "", data: { id: 2 } },
  { seq: 19, at: segAt(segBase, 90), kind: "assistant", text: "Fixed: ties now break by id, so a poll never reorders equal rows. The first test run caught a numeric compare on string ids." },
  { seq: 20, at: segAt(segBase, 95), kind: "thinking", text: "Worth checking the sidebar uses the same sort." },
  { seq: 21, at: segAt(segBase, 99), kind: "code", text: sb("rg -n \\\"lastAt\\\" src/projects.tsx") },
  { seq: 22, at: segAt(segBase, 100), kind: "result", text: "(no matches)", data: { code: sb("rg -n \\\"lastAt\\\" src/projects.tsx"), exit: 1 } },
  { seq: 23, at: segAt(segBase, 104), kind: "assistant", text: "The projects view doesn't sort by time, so nothing else needs the change." },
  { seq: 24, at: segAt(segBase, 105), kind: "done", text: "", data: { exit: 0, files: ["src/app.tsx", "test/sort.test.ts"], usage: { in: 48200, out: 2900, cost: 0.19, last_in: 30100 } } },
];

/** The same turn caught mid-work: its last stretch is running, and says what it is doing. */
export const segmentRunning = (): Line[] => {
  const base = Date.now() - 95_000;
  return segmentTurn.slice(0, 18).map((l) => ({ ...l, at: segAt(base, (Date.parse(l.at) - segBase) / 1000) }))
    .concat([{ seq: 30, at: segAt(base, 90), kind: "code", text: sb("bun run typecheck") }]);
};

/** A turn that ended on a failed command: its last stretch opens by itself. */
export const segmentFailed: Line[] = [
  ...segmentTurn.slice(0, 9),
  { seq: 30, at: segAt(segBase, 30), kind: "code", text: 'tools.patch("src/app.tsx", "…")' },
  { seq: 31, at: segAt(segBase, 31), kind: "result", text: "patched", data: { code: 'tools.patch("src/app.tsx", "…")', exit: 0 } },
  { seq: 32, at: segAt(segBase, 40), kind: "code", text: sb("bun test") },
  { seq: 33, at: segAt(segBase, 58), kind: "result", text: "test/sort.test.ts:\n✗ keeps equal rows in place\n  expected s2 before s1\n\n51 pass\n1 fail", data: { code: sb("bun test"), exit: 1, ms: 17600 } },
  { seq: 34, at: segAt(segBase, 60), kind: "done", text: "", data: { exit: 1, files: ["src/app.tsx"] } },
];

/** A live turn farmed out to subagents, then waiting on a question: both stay outside the folds. */
export const segmentSubagentsAsk = (): Line[] => {
  const base = Date.now() - 70_000;
  return [
    { seq: 1, at: segAt(base, 0), kind: "input", text: "Migrate the sessions table and check nothing else reads the old column." },
    { seq: 2, at: segAt(base, 3), kind: "thinking", text: "Two independent checks, then the migration itself." },
    { seq: 3, at: segAt(base, 5), kind: "code", text: sb("rg -n \\\"last_at\\\" --type go") },
    { seq: 4, at: segAt(base, 6), kind: "result", text: "store/sessions.go:88", data: { code: sb("rg -n \\\"last_at\\\" --type go"), exit: 0 } },
    { seq: 5, at: segAt(base, 8), kind: "assistant", text: "One reader in the store. I'll have two subagents check the web client and the CLI while I prepare the migration." },
    { seq: 6, at: segAt(base, 10), kind: "sub:start", text: "Find every read of last_at in the web client.", data: { worker: 1 } },
    { seq: 7, at: segAt(base, 10), kind: "sub:start", text: "Find every read of last_at in the CLI.", data: { worker: 2 } },
    { seq: 8, at: segAt(base, 20), kind: "sub:code", text: sb("rg -n lastAt src"), data: { worker: 1 } },
    { seq: 9, at: segAt(base, 30), kind: "sub:done", text: "", data: { worker: 2, status: "ok", steps: 1 } },
    { seq: 10, at: segAt(base, 50), kind: "code", text: 'tools.ask("The migration touches two tables. Run it against staging first?")' },
    { seq: 11, at: segAt(base, 50), kind: "ask", text: "The migration touches two tables. Run it against staging first?" },
  ];
};
