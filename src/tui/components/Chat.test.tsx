import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { render } from "ink";
import type { ReactElement } from "react";
import { Chat } from "./Chat.tsx";
import { Composer } from "./Composer.tsx";
import { MessageView } from "./Message.tsx";
import { activeTrigger, rankCompletions, setColorEnabled } from "../format.ts";
import { buildLines } from "../lines.ts";
import type { Message } from "../../schema/parts.ts";

// The whole point of the components being presentational: they render from a
// fixture with no server, no store and no terminal. `render` is pointed at a fake
// stdout that is not a TTY, so these run in CI exactly as they do locally.
setColorEnabled(false);

function draw(node: ReactElement, columns = 80): string {
  const out: string[] = [];
  const stdout = Object.assign(new EventEmitter(), {
    write: (s: string) => (out.push(s), true),
    columns,
    rows: 24,
    isTTY: false,
  });
  const stdin = Object.assign(new EventEmitter(), {
    isTTY: false,
    setRawMode() {},
    ref() {},
    unref() {},
    read: () => null,
    resume() {},
    pause() {},
  });
  const instance = render(node, {
    // deno-lint-ignore no-explicit-any -- Ink types the streams as Node's own.
    stdout: stdout as any,
    // deno-lint-ignore no-explicit-any
    stdin: stdin as any,
    exitOnCtrlC: false,
    patchConsole: false,
  });
  instance.unmount();
  return out.join("");
}

const thread: Message[] = [
  {
    id: "u1",
    sessionId: "s1",
    role: "user",
    parts: [{ type: "text", text: "add a test" }],
    pending: false,
    createdAt: 1,
  },
  {
    id: "a1",
    sessionId: "s1",
    role: "supervisor",
    parts: [
      { type: "reasoning", text: "I should look at the runner first.\nThen the test." },
      { type: "tool_call", id: "c1", name: "run_steps", input: { code: "await bash('ls')" } },
      { type: "tool_result", callId: "c1", output: "runner.ts", isError: false },
      { type: "text", text: "Added `runner.test.ts`." },
    ],
    pending: false,
    createdAt: 2,
  },
];

Deno.test("Chat renders a transcript, its meter and its scroll indicator from fixtures", () => {
  const lines = buildLines(thread, () => false, () => false, 80);
  const frame = draw(
    <Chat
      lines={lines}
      width={80}
      height={20}
      meter={{ model: "opus", costUsd: 0.42, contextTokens: 50_000, contextLimit: 200_000 }}
      activity="running the test suite"
      queued={["and fix the lint"]}
    />,
  );
  assert.ok(frame.includes("opus · $0.420 · 75% ctx left"), frame);
  assert.ok(frame.includes("running the test suite"));
  assert.ok(frame.includes("⧖ queued: and fix the lint"));
  // Folded by default: the step and a gist of its program are on the header, the
  // output block is behind the fold, and the reply prose is never folded.
  assert.ok(frame.includes("1 step"));
  assert.ok(frame.includes("await bash('ls')"));
  assert.ok(frame.includes("Added"));
  assert.equal(frame.includes("↳ output"), false);
  assert.equal(frame.includes("runner.ts"), false);

  // Scrolled up, the window says how much is below and where the top sits.
  const scrolled = draw(<Chat lines={lines} width={80} height={4} scrollOff={2} />);
  assert.ok(/↓ 2 more lines below · \d+%/.test(scrolled), scrolled);
});

Deno.test("Chat with an empty thread shows the placeholder, not a blank screen", () => {
  const frame = draw(<Chat lines={[]} width={80} height={4} />);
  assert.ok(frame.includes("one program per round"), frame);
});

Deno.test("MessageView renders one message standalone", () => {
  const frame = draw(<MessageView message={thread[1]} width={70} isExpanded={() => true} />);
  assert.ok(frame.includes("bough"));
  assert.ok(frame.includes("thinking (2 lines)"));
  assert.ok(frame.includes("await bash('ls')"));
  assert.ok(frame.includes("runner.test.ts"));
});

Deno.test("Composer shows the prompt, the placeholder and the mid-turn hint", () => {
  const empty = draw(<Composer input="" cursor={0} busy={false} width={60} maxRows={6} />);
  assert.ok(empty.includes("type a message · enter sends"), empty);

  const busy = draw(<Composer input="also this" cursor={9} busy width={60} maxRows={6} />);
  assert.ok(busy.includes("enter interjects this turn"));
  assert.ok(busy.includes("also this"));
});

Deno.test("Composer caps its height on a large paste and says what is off-screen", () => {
  const input = Array.from({ length: 30 }, (_v, i) => `line ${i}`).join("\n");
  const frame = draw(
    <Composer input={input} cursor={input.length} busy={false} width={60} maxRows={5} />,
  );
  assert.ok(/… \d+ lines above · \d+ below/.test(frame), frame);
  assert.equal(frame.includes("line 0"), false); // windowed to the cursor
  assert.ok(frame.includes("line 29"));
});

Deno.test("Composer renders the @ popup for the trigger under the cursor", () => {
  const text = "look at @app";
  const trigger = activeTrigger(text, text.length)!;
  const { items, total } = rankCompletions(
    [{ name: "server/app.ts" }, { name: "app.tsx" }, { name: "docs/app.md" }],
    trigger,
    2,
  );
  const frame = draw(
    <Composer
      input={text}
      cursor={text.length}
      busy={false}
      width={60}
      maxRows={4}
      trigger={trigger}
      completions={items}
      completionSel={0}
      completionMore={total - items.length}
    />,
  );
  assert.ok(frame.includes("@app.tsx"), frame);
  assert.ok(frame.includes("files & dirs"));
  assert.ok(frame.includes("↓ 1"));
});

Deno.test("Composer's / popup says so when nothing matches, rather than vanishing", () => {
  const text = "/zzz";
  const trigger = activeTrigger(text, text.length)!;
  const frame = draw(
    <Composer
      input={text}
      cursor={text.length}
      busy={false}
      width={60}
      maxRows={4}
      trigger={trigger}
      completions={[]}
    />,
  );
  assert.ok(frame.includes("no matching skills"), frame);
});
