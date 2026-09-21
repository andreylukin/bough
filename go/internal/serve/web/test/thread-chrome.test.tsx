import { expect, mock, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
mock.module("dompurify", () => ({ default: { sanitize: (s: string) => s } }));
const { ChangesChip, JobBlock, SessionChanges, Thread } = await import("../src/app");
import type { Line, Row } from "../src/types";

const props = { onSend: async () => null, onAnswer: async () => null, onInterrupt: () => {}, onArchive: () => {}, onRename: async () => {}, onModel: () => {},
  onEffort: () => {}, onAssign: () => {}, onBack: () => {}, onContext: () => {}, onAck: () => {}, projects: [], busy: false, jump: null };
const thread = (r: Partial<Row>, extra: Record<string, unknown> = {}) =>
  renderToStaticMarkup(<Thread row={{ id: "s1", cwd: "/tmp/x", status: "idle", ...r } as unknown as Row} lines={[]} sending={[]} {...props} {...extra} />);

test("a local session outside a checkout says No repo in the strip, the sentence on hover", () => {
  const read = { files: [], repo: "", failed: false } as unknown as never;
  const html = renderToStaticMarkup(
    <SessionChanges.Provider value={{ session: read, tree: read, turn: undefined, turnSeq: undefined, retry: () => {} } as never}>
      <ChangesChip row={{ id: "s1", cwd: "/tmp/x" } as Row} />
    </SessionChanges.Provider>,
  );
  expect(html).toContain(">No repo<");
  expect(html).toContain('title="No Git repository"');
  expect(html).not.toContain(">No Git repository<");
});

test("an agent notice names the agent by its first clause, in prose, not its whole prompt", () => {
  const line: Line = { seq: 3, at: "2026-01-02T10:00:00Z", kind: "job", text: "[agent Reply with the single word PONG and nothing else. Do not run any code · 01a0 finished] PONG" };
  const html = renderToStaticMarkup(<JobBlock line={line} />);
  expect(html).toContain("agent-notice-name");
  expect(html).toContain(">Reply with the single word PONG and nothing else…<");
  expect(html).not.toContain(">Reply with the single word PONG and nothing else. Do not run any code<");
});

test("an empty transcript has a title and one line; a spawned agent says it has not run", () => {
  expect(thread({})).toContain("Nothing here yet");
  expect(thread({ spawnedBy: "p1" } as Partial<Row>)).toContain("This background agent has not run yet");
  expect(thread({})).not.toContain("No recorded turns yet");
});

test("the Read-only badge yields to the footer's Start project session offer", () => {
  expect(thread({})).toContain("Read-only");
  const offered = thread({}, { onStartProject: () => {}, projects: [{ slug: "b", name: "bough" }] });
  expect(offered).not.toContain("Read-only");
  expect(offered).toContain("Start project session");
});

test("a child session's parent link is a header row of its own, not inside the title row", () => {
  const html = thread({ spawnedBy: "p1" } as Partial<Row>);
  expect(html).toMatch(/<header class="thread-head"><button[^>]*class="child-parent-link"/);
  expect(html).not.toMatch(/head-main"><button[^>]*child-parent-link/);
});
