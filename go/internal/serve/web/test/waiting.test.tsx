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

const props = { onSend: async () => null, onAnswer: async () => null, onInterrupt: () => {}, onArchive: () => {}, onRename: async () => {}, onModel: () => {},
  onEffort: () => {}, onAssign: () => {}, onBack: () => {}, onContext: () => {}, onAck: () => {}, projects: [], busy: false, jump: null };

test("R2-B: while a send is pending the header never says Done", () => {
  const html = renderToStaticMarkup(<Thread row={{ ...row, status: "done" } as Row} lines={[]} sending={sent} {...props} />);
  const head = html.slice(html.indexOf("thread-head"), html.indexOf("</header>"));
  expect(head).not.toContain(">Done<");
  expect(head).toContain("Sending");
});

test("R2-B: the status goes Sending, then Waiting once accepted, then Streaming", () => {
  expect(composerStatus({ sending: true, accepted: false, running: false, streamed: false, activity: "" })).toBe("Sending");
  expect(composerStatus({ sending: true, accepted: true, running: false, streamed: false, activity: "" })).toBe("Waiting");
  expect(composerStatus({ sending: false, running: true, streamed: true, activity: "" })).toBe("Streaming");
  const accepted = renderToStaticMarkup(<Thread row={{ ...row, status: "done" } as Row} lines={[]} sending={[{ ...sent[0], accepted: true }]} {...props} />);
  expect(accepted).toContain("Waiting for model…");
  expect(accepted).toContain("breath-dot");
  const waiting = renderToStaticMarkup(<Thread row={{ ...row, status: "running" } as Row} lines={[]} sending={sent} {...props} />);
  expect(waiting).toContain("Waiting for model…");
  const streaming = renderToStaticMarkup(<Thread row={{ ...row, status: "running" } as Row} lines={[]} sending={[]} activity="Reading app.tsx" {...props} />);
  expect(streaming).toContain("typing-dots");
  expect(streaming).not.toContain("Waiting for model…");
});
