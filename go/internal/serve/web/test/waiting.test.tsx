import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { PendingThread, Thread, composerStatus } from "../src/app";
import type { Row } from "../src/types";

const row = { id: "s1", cwd: "/tmp/x", status: "idle" } as unknown as Row;
const sent = [{ id: "p1", text: "fix the tests", after: 0 }];

test("the composer status names the phase: Sending, then Waiting, then Streaming", () => {
  expect(composerStatus({ sending: true, running: false, streamed: false, activity: "" })).toBe("Sending");
  expect(composerStatus({ sending: false, running: true, streamed: false, activity: "" })).toBe("Waiting");
  expect(composerStatus({ sending: false, running: true, streamed: true, activity: "" })).toBe("Streaming");
  expect(composerStatus({ sending: false, running: false, streamed: false, activity: "" })).toBe("");
});

test("a sent prompt shows at once with a waiting dot and a Sending hint", () => {
  const html = renderToStaticMarkup(<Thread row={row} lines={[]} sending={sent} onSend={async () => null} onAnswer={async () => null}
    onInterrupt={() => {}} onArchive={() => {}} onRename={async () => {}} onModel={() => {}} onEffort={() => {}} onAssign={() => {}}
    onBack={() => {}} onContext={() => {}} onAck={() => {}} projects={[]} busy={false} jump={null} />);
  expect(html).toContain("fix the tests");
  expect(html).toContain("waiting-dot");
  expect(html).toMatch(/composer-hint[^>]*>Sending ·/);
});

test("a session started from the welcome shows its prompt instead of Loading session", () => {
  const html = renderToStaticMarkup(<PendingThread sending={sent} />);
  expect(html).toContain("fix the tests");
  expect(html).toContain("waiting-dot");
  expect(html).not.toContain("Loading session");
});

test("once the turn runs, the sent prompt is solid and the hint says Waiting", () => {
  const html = renderToStaticMarkup(<Thread row={{ ...row, status: "running" } as Row} lines={[]} sending={sent} onSend={async () => null} onAnswer={async () => null}
    onInterrupt={() => {}} onArchive={() => {}} onRename={async () => {}} onModel={() => {}} onEffort={() => {}} onAssign={() => {}}
    onBack={() => {}} onContext={() => {}} onAck={() => {}} projects={[]} busy={false} jump={null} />);
  expect(html).toContain("fix the tests");
  expect(html).not.toContain("turn-sending");
  expect(html).not.toContain("Sending…");
  expect(html).toMatch(/composer-hint[^>]*>Waiting ·/);
});

test("resending a stopped turn's prompt shows Sending, then lands on the new input", () => {
  const at = "2026-01-02T10:00:00Z";
  const stopped = [{ seq: 1, at, kind: "input", text: "fix the tests" }, { seq: 2, at, kind: "cancelled", text: "" }] as any;
  const again = [{ id: "p2", text: "fix the tests", after: 2, seen: ["1|" + at] }];
  const render = (lines: any) => renderToStaticMarkup(<Thread row={row} lines={lines} sending={again} onSend={async () => null} onAnswer={async () => null}
    onInterrupt={() => {}} onArchive={() => {}} onRename={async () => {}} onModel={() => {}} onEffort={() => {}} onAssign={() => {}}
    onBack={() => {}} onContext={() => {}} onAck={() => {}} projects={[]} busy={false} jump={null} />);
  expect(render(stopped)).toMatch(/composer-hint[^>]*>Sending ·/);
  expect(render([...stopped, { seq: 3, at, kind: "input", text: "fix the tests" }])).not.toMatch(/composer-hint[^>]*>Sending ·/);
});

test("a send from a switched view still lands when the new session's input reuses a stale seq", () => {
  const at = "2026-01-02T10:00:00Z";
  const pending = [{ id: "p3", text: "new prompt", after: 1, seen: ["1|2026-01-01T09:00:00Z"] }];
  const html = renderToStaticMarkup(<Thread row={row} lines={[{ seq: 1, at, kind: "input", text: "new prompt" }] as any} sending={pending as any} onSend={async () => null} onAnswer={async () => null}
    onInterrupt={() => {}} onArchive={() => {}} onRename={async () => {}} onModel={() => {}} onEffort={() => {}} onAssign={() => {}}
    onBack={() => {}} onContext={() => {}} onAck={() => {}} projects={[]} busy={false} jump={null} />);
  expect(html).not.toMatch(/composer-hint[^>]*>Sending ·/);
});
