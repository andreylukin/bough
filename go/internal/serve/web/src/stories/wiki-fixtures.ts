import type {
  WikiActivityData, WikiCounts, WikiIndexData, WikiPageData, WikiReviewData, WikiSourceData,
} from "../wiki";

// Sample data in the shape the /api/wiki routes answer. Session ids are
// real-looking UUIDs, so the markers read the way they do in the app.
const minsAgo = (m: number) => new Date(Date.now() - m * 60_000).toISOString();
const counts = (c: Partial<WikiCounts>): WikiCounts => ({ cited: 0, inferred: 0, uncited: 0, unsupported: 0, superseded: 0, ...c });

const REPLAY = "01a09a12-7c44-7e19-9d0e-5f3a2b1c8d90";
const CIGREEN = "01a08b3e-1f02-7a55-b1c4-9e77d0a4c211";

export const wikiIndex: WikiIndexData = {
  dir: "~/.bough/wiki",
  exists: true,
  health: {
    installed: true, every: "5m0s", pending: 3, ingesting: false, lastIngest: minsAgo(41),
    unsupported: 1, superseded: 1, uncited: 1,
  },
  thin: 3,
  orphans: 1,
  topics: [
    {
      name: "bough",
      pages: [
        { path: "topics/bough/serve-control-room.md", topic: "bough", title: "The serve control room",
          summary: "One stylesheet, no router, dist/app.js committed and embedded.", updated: "2026-09-12",
          counts: counts({ cited: 14, inferred: 2 }) },
        { path: "topics/bough/macos-binary-overwrite.md", topic: "bough", title: "Overwriting a running binary on macOS",
          summary: "cp over a Mach-O SIGKILLs it; rm first or cp-to-temp and mv.", updated: "2026-09-04",
          counts: counts({ cited: 5, unsupported: 1 }) },
        { path: "topics/bough/subagents.md", topic: "bough", title: "Subagents are branches, not processes",
          summary: "One workspace per subagent, depth capped at one.", updated: "2026-09-03", counts: counts({ cited: 9 }) },
      ],
    },
    {
      name: "go-testing",
      pages: [
        { path: "topics/go-testing/ghost-mode-pty.md", topic: "go-testing", title: "Ghost-mode PTY tests are flaky under -race",
          summary: "The vtreal suite loses the first frame when the scheduler is loaded.", updated: "2026-09-11",
          counts: counts({ cited: 2, inferred: 1, uncited: 1, superseded: 1 }) },
        { path: "topics/go-testing/read-the-gate.md", topic: "go-testing", title: "Read the gate, don't tail it",
          summary: "go test ./... | tail -3 hid a red gate; grep ^FAIL instead.", updated: "2026-09-09",
          counts: counts({ cited: 4 }) },
      ],
    },
  ],
};

const cite = (session: string, seq: number, label: string, excerpt: string) => ({ session, seq, label, excerpt });

export const wikiPage: WikiPageData = {
  path: "topics/go-testing/ghost-mode-pty.md",
  topic: "go-testing",
  title: "Ghost-mode PTY tests are flaky under -race",
  summary: "The real-PTY suite drops its first frame when the machine is loaded.",
  updated: "2026-09-11",
  counts: counts({ cited: 2, inferred: 1, uncited: 1, superseded: 1 }),
  sessions: [REPLAY, CIGREEN],
  body: "# Ghost-mode PTY tests are flaky under -race\n\n…",
  linkedFrom: [wikiIndex.topics[1].pages[1]],
  blocks: [
    { kind: "lede", line: 3, end: 3, raw: "", bullet: false, cites: [],
      text: "The real-PTY suite drops its first frame when the machine is loaded, so a run that passes alone fails in CI. The cause is the harness, not the terminal code." },
    { kind: "heading", line: 5, end: 5, raw: "", bullet: false, cites: [], text: "What actually happens" },
    { kind: "claim", state: "cited", line: 7, end: 7, raw: "", bullet: false,
      text: "Under `-race` the vtreal harness reads the pty before the child has drawn anything, and an empty first read is treated as a frame.",
      cites: [cite(REPLAY, 214, "output", "--- FAIL: TestGhostMode/first_frame (0.41s)\n    vtreal.go:88: want 3 frames, got 2")] },
    { kind: "claim", state: "cited", line: 9, end: 9, raw: "", bullet: false,
      text: "Raising the settle timeout to 250 ms made 628 replays pass in 45 s, which is why the fix is a wait and not a retry.",
      cites: [cite(REPLAY, 281, "output", "ok  628 replays in 45.2s"), cite(REPLAY, 283, "assistant", "Settle 250ms holds.")] },
    { kind: "claim", state: "inferred", line: 11, end: 11, raw: "", bullet: false, cites: [],
      text: "The same race probably explains the two macOS-only CI flakes, since both wait on a first read." },
    { kind: "claim", state: "uncited", line: 13, end: 13, raw: "", bullet: false, cites: [],
      text: "The tmux path is unaffected: it buffers a full screen before the harness ever reads." },
    { kind: "heading", line: 15, end: 15, raw: "", bullet: false, cites: [], text: "Superseded" },
    { kind: "claim", state: "superseded", line: 17, end: 17, raw: "", bullet: true,
      text: "Setting `-parallel 1` is the only reliable workaround.",
      cites: [cite(CIGREEN, 88, "ran", "go test ./plugins/replay -run Ghost -parallel 1\nok  20 runs, 0 failures — 4m 11s")],
      supersededBy: cite(REPLAY, 281, "output", "ok  628 replays in 45.2s") },
    { kind: "heading", line: 19, end: 19, raw: "", bullet: false, cites: [], text: "See also" },
    { kind: "links", line: 20, end: 20, raw: "", bullet: true, cites: [], text: "[Read the gate, don't tail it](read-the-gate.md)" },
  ],
};

export const wikiSource: WikiSourceData = {
  session: { id: REPLAY, title: "ghost-mode PTY race", repo: "/Users/a/repos/bough", branch: "replay-scenarios", cwd: "/Users/a/repos/bough" },
  seq: 214,
  at: "2026-09-11T14:22:00Z",
  total: 397,
  lines: [
    { seq: 211, label: "user", text: "the ghost-mode test fails in CI but never locally — find out why" },
    { seq: 212, label: "assistant", text: "Running it twenty times under -race to see whether load is the variable." },
    { seq: 213, label: "ran", text: "go test ./plugins/replay -run Ghost -race -count 20" },
    { seq: 214, label: "output", text: "--- FAIL: TestGhostMode/first_frame (0.41s)\n    vtreal.go:88: want 3 frames, got 2\n    vtreal.go:88: first read returned 0 bytes\nFAIL    github.com/andreylukin/bough/plugins/replay     9.402s" },
    { seq: 215, label: "assistant", text: "Two of twenty. The empty first read is counted as a frame, so the harness is a frame short before anything is drawn." },
    { seq: 216, label: "ran", text: "rg -n \"first read\" plugins/replay/vtreal.go" },
    { seq: 217, label: "output", text: "88:  // first read may be empty while the child starts" },
  ],
  citedBy: [wikiIndex.topics[1].pages[0], wikiIndex.topics[1].pages[1]],
};

export const wikiReview: WikiReviewData = {
  flags: [
    { kind: "unsupported", page: "topics/bough/macos-binary-overwrite.md", title: "Overwriting a running binary on macOS",
      line: 14, end: 14, raw: "", why: "cites a session that does not exist: 01a07f00",
      claim: "Copying over a running Mach-O binary makes the kernel SIGKILL the process, so an install must `rm` first.",
      evidence: "wiki check: cites a session that does not exist: 01a07f00-0000-7000-8000-000000000000" },
    { kind: "superseded", page: "topics/go-testing/ghost-mode-pty.md", title: "Ghost-mode PTY tests are flaky under -race",
      line: 17, end: 17, raw: "", why: "a later session replaced it",
      claim: "Setting `-parallel 1` is the only reliable workaround for the ghost-mode flake.",
      evidence: "ok  628 replays in 45.2s   (-parallel 4, settle 250ms)",
      cite: cite(REPLAY, 281, "output", "ok  628 replays in 45.2s") },
    { kind: "uncited", page: "topics/go-testing/ghost-mode-pty.md", title: "Ghost-mode PTY tests are flaky under -race",
      line: 13, end: 13, raw: "", why: "a claim with no entry behind it", evidence: "",
      claim: "The tmux path is unaffected: it buffers a full screen before the harness ever reads." },
  ],
  pending: [
    { id: "01a09b20-3d11-7c02-8e55-aa0c3f9b7e12", title: "bough artifact port hijack", entries: 62, last: minsAgo(60 * 26) },
    { id: "01a09c41-9a77-7d10-b2f3-51e0c8d4a603", title: "serve: the @ picker searched depth-first", entries: 41, last: minsAgo(60 * 50) },
  ],
};

const today = (h: number, m: number) => { const d = new Date(); d.setHours(h, m, 0, 0); return d.toISOString(); };

export const wikiActivity: WikiActivityData = {
  every: "5m0s",
  pending: 3,
  spent: 4.18,
  today: { runs: 3, ingested: 2, noMaterial: 1, spent: 0.41 },
  runs: [
    { session: "01a09d01-0000-7000-8000-000000000001", command: `/llm-wiki ingest ${REPLAY}`, at: today(16, 41),
      done: today(16, 42), ms: 43_000, cost: 0.22, running: false, commit: "a4f10c2",
      outcomes: [{ id: REPLAY, title: "ghost-mode PTY race", disposition: "Update", pages: ["topics/go-testing/ghost-mode-pty.md"] }],
      files: ["log.md", "index.md", "topics/go-testing/ghost-mode-pty.md"] },
    { session: "01a09d01-0000-7000-8000-000000000002", command: "/llm-wiki ingest 01a09c00-1111-7000-8000-000000000000", at: today(11, 17),
      done: today(11, 17), ms: 9_000, cost: 0.01, running: false, commit: "7c1e880",
      outcomes: [{ id: "01a09c00-1111-7000-8000-000000000000", title: "what time is it in Lisbon", disposition: "No material", pages: [] }],
      files: ["log.md"] },
    { session: "01a09d01-0000-7000-8000-000000000003", command: "/llm-wiki ingest 01a09b77-2222-7000-8000-000000000000", at: today(9, 44),
      done: today(9, 45), ms: 72_000, cost: 0.18, running: false, commit: "31b9de4",
      outcomes: [{ id: "01a09b77-2222-7000-8000-000000000000", title: "serve: copy control on the block", disposition: "New", pages: ["topics/bough/serve-control-room.md"] }],
      files: ["log.md", "index.md", "topics/bough/serve-control-room.md"] },
  ],
};
