import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { PendingThread, Thread, composerStatus } from "../src/app";
import type { Row } from "../src/types";

const row = { id: "s1", cwd: "/tmp/x", status: "idle" } as unknown as Row;
const sent = [{ id: "p1", text: "fix the tests", after: 0 }];

test("the composer status names the phase: Sending, then Waiting, then Streaming", () => {
  expect(composerStatus({ sending: true, running: false, streamed: false, activity: "" })).toBe("Sending");
  expect(composerStatus({ sending: false, running: true, streamed: false, activity: "" })).toBe("Waiting");
  expect(composerStatus({ sending: false, running: true, streamed: true, activity: "" })).toBe("Working");
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
  expect(html).toMatch(/composer-hint[^>]*>Waiting for model…/);
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
  expect(composerStatus({ sending: false, running: true, streamed: true, activity: "" })).toBe("Working");
  const accepted = renderToStaticMarkup(<Thread row={{ ...row, status: "done" } as Row} lines={[]} sending={[{ ...sent[0], accepted: true }]} {...props} />);
  expect(accepted).toContain("Waiting for model…");
  expect(accepted).toContain("breath-dot");
  const waiting = renderToStaticMarkup(<Thread row={{ ...row, status: "running" } as Row} lines={[]} sending={sent} {...props} />);
  expect(waiting).toContain("Waiting for model…");
  const streaming = renderToStaticMarkup(<Thread row={{ ...row, status: "running" } as Row} lines={[]} sending={[]} activity="Reading app.tsx" {...props} />);
  expect(streaming).toContain("typing-dots");
  expect(streaming).not.toContain("Waiting for model…");
});

// R3-C: Esc before the first token really stops the server turn.
test("R3-C: a send not yet running already offers Stop, so Esc before the first token is not lost", () => {
  const html = renderToStaticMarkup(<Thread row={{ ...row, status: "done" } as Row} lines={[]} sending={[{ ...sent[0], accepted: true }]} {...props} />);
  expect(html).toContain("composer-stop");
});

test("R3-C: the composer says Stopping until the server records the stop", () => {
  expect(composerStatus({ sending: false, running: true, streamed: true, activity: "", stopping: true })).toBe("Stopping");
  expect(composerStatus({ sending: true, accepted: true, running: false, streamed: false, activity: "", stopping: true })).toBe("Stopping");
  expect(composerStatus({ sending: false, running: false, streamed: false, activity: "", stopping: true })).toBe("");
});

const threadProps = {
  onSend: async () => null, onAnswer: async () => null, onInterrupt: () => {}, onArchive: () => {}, onRename: async () => {},
  onModel: () => {}, onEffort: () => {}, onAssign: () => {}, onBack: () => {}, onContext: () => {}, onAck: () => {},
  projects: [], busy: false, jump: null,
};

test("R3-D: a /model send lands on its command record, not on an assistant line", () => {
  const at = "2026-01-02T10:00:00Z";
  const lines = [
    { seq: 1, at, kind: "input", text: "hi" }, { seq: 3, at, kind: "done", text: "" },
    { seq: 4, at, kind: "command", text: "/model openai/gpt-5" }, { seq: 5, at, kind: "system", text: "model: openai/gpt-5" },
  ] as any;
  const cmd = [{ id: "c1", text: "/model openai/gpt-5", after: 3, accepted: true }];
  const html = renderToStaticMarkup(<Thread row={row} lines={lines} sending={cmd as any} {...threadProps} />);
  expect(html).toContain("Model changed");
  expect(html).not.toContain("Sending…");
  expect(html).not.toMatch(/thread-head[\s\S]*>(Sending|Working)</);
  expect(html).not.toMatch(/composer-hint[^>]*>(Sending|Waiting) ·/);
});

test("R3-D: an errored session never reads Working or Sending for a send it did not start", () => {
  const cmd = [{ id: "c2", text: "/model nope", after: 0, accepted: true }];
  const html = renderToStaticMarkup(<Thread row={{ ...row, status: "error" } as Row} lines={[]} sending={cmd as any} {...threadProps} />);
  expect(html).not.toMatch(/thread-head[\s\S]*>(Sending|Working)</);
  expect(html).not.toContain("Waiting for model");
});

test("R4-D: one word per phase — Waiting before output, then Working, never Streaming", () => {
  expect(composerStatus({ sending: false, running: true, streamed: true, activity: "" })).toBe("Working");
  expect(composerStatus({ sending: false, running: true, streamed: false, activity: "Running ls" })).toBe("Working");
});

test("R4-D: elapsed since equals the prompt time across two steps", async () => {
  const { TurnView } = await import("../src/app");
  const { groupTurns } = await import("../src/render");
  const at = (s: number) => new Date(Date.parse("2026-01-02T10:00:00Z") + s * 1000).toISOString();
  const c = 'tools.bash("ls")', d = 'tools.bash("pwd")';
  const html = renderToStaticMarkup(<TurnView turn={groupTurns([
    { seq: 2, at: at(1), kind: "code", text: c },
    { seq: 3, at: at(2), kind: "result", text: "ok", data: { code: c, exit: 0 } },
    { seq: 4, at: at(30), kind: "code", text: d },
  ] as never)[0]} working="Working" />);
  // No prompt recorded: no clock re-anchored at a step's own start.
  expect(html).not.toContain("work-seg-time");
});

test("R4-D: context does not decrease on a cancelled done without usage", async () => {
  const { sessionUsage } = await import("../src/render");
  const lines = [
    { seq: 1, at: "", kind: "done", text: "", data: { usage: { in: 90000, out: 10, last_in: 42000 } } },
    { seq: 2, at: "", kind: "cancelled", text: "" },
    { seq: 3, at: "", kind: "done", text: "", data: { usage: { in: 571, out: 0, last_in: 571 } } },
    { seq: 4, at: "", kind: "cancelled", text: "", data: { usage: { in: 300, out: 0, last_in: 300 } } },
    { seq: 5, at: "", kind: "done", text: "" },
  ];
  expect(sessionUsage(lines as never)?.lastIn).toBe(42000);
});

test("while the turn waits for the model, the composer offers to steer, not a new task", () => {
  const html = renderToStaticMarkup(<Thread row={row} lines={[]} sending={[{ ...sent[0], accepted: true }]} onSend={async () => null} onAnswer={async () => null}
    onInterrupt={() => {}} onArchive={() => {}} onRename={async () => {}} onModel={() => {}} onEffort={() => {}} onAssign={() => {}}
    onBack={() => {}} onContext={() => {}} onAck={() => {}} projects={[]} busy={false} jump={null} />);
  expect(html).toMatch(/composer-hint[^>]*>Waiting for model… ·/);
  expect(html).toContain('placeholder="Steer the running turn…"');
});
