import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { Thread } from "../src/app";
import type { Row } from "../src/types";

const props = { onSend: async () => null, onAnswer: async () => null, onInterrupt: () => {}, onArchive: () => {}, onRename: async () => {}, onModel: () => {},
  onEffort: () => {}, onAssign: () => {}, onBack: () => {}, onContext: () => {}, onAck: () => {}, projects: [], busy: false, jump: null };
const render = (r: Partial<Row>) => renderToStaticMarkup(<Thread row={{ id: "s1", cwd: "/tmp/x", status: "idle", ...r } as unknown as Row} lines={[]} sending={[]} {...props} />);

test("MB-composer: placeholder copy per state, aria-label without the ellipsis", () => {
  expect(render({})).toContain('placeholder="Describe the next task…"');
  expect(render({})).toContain('aria-label="Describe the next task"');
  expect(render({ archived: true } as Partial<Row>)).toContain('placeholder="Read-only"');
});

test("MB-composer: idle keys are chips; a running turn lists Esc stop; status sits outside the key hint", () => {
  const idle = render({});
  expect(idle).toMatch(/composer-hint[^>]*><span class="composer-key"><kbd>↵<\/kbd> send/);
  const running = render({ status: "running" } as Partial<Row>);
  expect(running).toContain("<kbd>Esc</kbd> stop");
  expect(running).toMatch(/composer-status-(waiting|working)/);
  expect(running).toContain('title="Stop (Esc)"');
});

test("MB-composer: the local badge drops the Local prefix and the archived note is a callout", () => {
  const html = render({ archived: true } as Partial<Row>);
  expect(html).toContain("Read-only");
  expect(html).not.toContain("Local · read-only");
  expect(html).toMatch(/archived-note composer-note/);
  expect(html).toContain("This session is read-only until unarchived.");
});
