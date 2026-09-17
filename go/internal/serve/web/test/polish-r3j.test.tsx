import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import type { Row } from "../src/types";

// R3-J: the Changes page mutes a zero deletion count and folds the home dir to ~.
test("Changes: −0 is muted and the cwd reads from ~", async () => {
  const { ChangesBody } = await import("../src/changes");
  const row = { id: "s1", title: "t", cwd: "/Users/me/code/proj", status: "done", live: false, archived: false, entries: 3 } as unknown as Row;
  const read = (files: any[]) => ({ files, repo: true, failed: false, at: 1 });
  const data = { session: read([{ path: "a.txt", add: 3, del: 0, patch: true }]), tree: read([]), retry: () => {} } as any;
  for (const cards of [false, true]) {
    const html = renderToStaticMarkup(<ChangesBody row={row} data={data} scope="session" onScope={() => {}} cards={cards} />);
    expect(html).not.toMatch(/class="rt-del"[^>]*>−0</);
    expect(html).toMatch(/rt-zero[^>]*>−0</);
    expect(html).toContain(">~/code/proj<");
  }
});

// R3-J: the user's prompt is a bubble, so it reads apart from the answer.
test("a recorded and a sending prompt both render as a bubble", async () => {
  const { Thread } = await import("../src/app");
  const at = "2026-09-16T10:00:00Z";
  const props = { onSend: async () => null, onAnswer: async () => null, onInterrupt: () => {}, onArchive: () => {}, onRename: async () => {},
    onModel: () => {}, onEffort: () => {}, onAssign: () => {}, onBack: () => {}, onContext: () => {}, onAck: () => {}, projects: [], busy: false, jump: null } as const;
  const row = { id: "s1", cwd: "/tmp/x", status: "running" } as never;
  const sending = renderToStaticMarkup(<Thread {...props} row={row} lines={[]} sending={[{ id: "p1", text: "hi", after: 0, at }]} />);
  const done = renderToStaticMarkup(<Thread {...props} row={{ ...(row as object), status: "idle" } as never} lines={[
    { seq: 1, at, kind: "input", text: "hi" }, { seq: 2, at, kind: "assistant", text: "yo" }, { seq: 3, at, kind: "done", text: "" }] as never} />);
  for (const html of [sending, done]) expect(html).toContain('class="prompt-text prompt-bubble"');
});
