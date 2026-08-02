import assert from "node:assert/strict";
import { test } from "bun:test";
import { testRender } from "@opentui/react/test-utils";
import type { ReactNode } from "react";
import { askPromptLines } from "./App.tsx";
import { Chat } from "./Chat.tsx";
import { Composer } from "./Composer.tsx";
import { MessageView } from "./Message.tsx";
import { activeTrigger, rankCompletions, setColorEnabled } from "../format.ts";
import { buildLines } from "../lines.ts";
import type { Message } from "../../schema/parts.ts";

// The whole point of the components being presentational: they render from a
// fixture with no server, no store and no terminal. OpenTUI's test renderer paints
// into an in-memory cell grid with no tty attached, so these run in CI exactly as
// they do locally.
setColorEnabled(false);

/**
 * The painted cells, as text.
 *
 * `captureCharFrame()` reads the render buffer back row by row, so what a test sees
 * is what a terminal would show — not the escape sequences on the way there. It is
 * a FIXED GRID: anything past `columns` wraps or is clipped, and anything past
 * `rows` is simply not painted. `rows` is therefore sized to the tallest thing the
 * component can produce rather than to a real terminal.
 */
async function draw(node: ReactNode, columns = 80, rows = 40): Promise<string> {
  // `testRender` mounts inside React's `act`, which is what makes the first commit
  // land before this returns — a bare `createRoot().render()` commits on a later
  // task and reads back an empty grid. The act ENVIRONMENT is then switched off:
  // nothing after the mount is act-wrapped, and leaving it on turns every later
  // repaint into a console warning.
  const t = await testRender(node, { width: columns, height: rows, exitOnCtrlC: false });
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
  // `renderOnce` is MANDATORY: without a render pass the buffer holds uninitialised
  // glyphs rather than spaces, and every assertion below fails confusingly.
  await t.renderOnce();
  const frame = t.captureCharFrame();
  t.renderer.destroy();
  return frame;
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

test("Chat renders a transcript and its scroll indicator from fixtures", async () => {
  const lines = buildLines(thread, () => false, () => false, 80);
  const frame = await draw(
    <Chat
      lines={lines}
      width={80}
      height={20}
      activity="running the test suite"
      queued={["and fix the lint"]}
    />,
  );
  // The meter is NOT here any more: it renders BELOW the composer, which is where
  // every comparable harness puts the session status. `App` owns it now.
  assert.equal(frame.includes("ctx left"), false, frame);
  assert.ok(frame.includes("running the test suite"));
  assert.ok(frame.includes("⧖ queued: and fix the lint"));
  // Folded by default: the step and a gist of its program are on the header, the
  // output block is behind the fold, and the reply prose is never folded.
  assert.ok(frame.includes("1 step"));
  // The collapsed header names what the program DID, not its first line of code.
  assert.ok(frame.includes("ran 1 command"), frame);
  assert.ok(frame.includes("Added"));
  assert.equal(frame.includes("↳ output"), false);
  assert.equal(frame.includes("runner.ts"), false);

  // Scrolled up, the window says how much is below and where the top sits.
  const scrolled = await draw(<Chat lines={lines} width={80} height={4} scrollOff={2} />);
  assert.ok(/↓ 2 more lines below · \d+%/.test(scrolled), scrolled);
});

test("Chat with an empty thread shows the placeholder, not a blank screen", async () => {
  const frame = await draw(<Chat lines={[]} width={80} height={4} />);
  assert.ok(frame.includes("one program per round"), frame);
});

test("MessageView renders one message standalone", async () => {
  const frame = await draw(<MessageView message={thread[1]} width={70} isExpanded={() => true} />);
  assert.ok(frame.includes("bough"));
  assert.ok(frame.includes("thinking (2 lines)"));
  assert.ok(frame.includes("await bash('ls')"));
  assert.ok(frame.includes("runner.test.ts"));
});

test("Composer shows the prompt, the placeholder and the mid-turn hint", async () => {
  const empty = await draw(<Composer input="" cursor={0} busy={false} width={60} maxRows={6} />);
  assert.ok(empty.includes("type a message · enter sends"), empty);

  const busy = await draw(<Composer input="also this" cursor={9} busy width={60} maxRows={6} />);
  assert.ok(busy.includes("enter interjects this turn"));
  assert.ok(busy.includes("also this"));
});

test("Composer caps its height on a large paste and says what is off-screen", async () => {
  const input = Array.from({ length: 30 }, (_v, i) => `line ${i}`).join("\n");
  const frame = await draw(
    <Composer input={input} cursor={input.length} busy={false} width={60} maxRows={5} />,
  );
  assert.ok(/… \d+ lines above · \d+ below/.test(frame), frame);
  assert.equal(frame.includes("line 0"), false); // windowed to the cursor
  assert.ok(frame.includes("line 29"));
});

test("Composer renders the @ popup for the trigger under the cursor", async () => {
  const text = "look at @app";
  const trigger = activeTrigger(text, text.length)!;
  const { items, total } = rankCompletions(
    [{ name: "server/app.ts" }, { name: "app.tsx" }, { name: "docs/app.md" }],
    trigger,
    2,
  );
  const frame = await draw(
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

test("Composer's / popup says so when nothing matches, rather than vanishing", async () => {
  const text = "/zzz";
  const trigger = activeTrigger(text, text.length)!;
  const frame = await draw(
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
  assert.ok(frame.includes("no matching commands or skills"), frame);
});

/**
 * A question's height is its LINE COUNT, and the layout has to agree with the card.
 *
 * `App` lays the ask card out in a fixed region computed as `3 + prompt lines +
 * options`. That number was a bare `4 + options`, which is right for exactly the
 * shape `ask()` shipped with — one line, "Deploy to prod or staging?" — and paints a
 * multi-line question's later rows ON TOP of the options for anything else. The
 * workflow approval card is multi-line by design (its phase list is the point), and
 * it rendered as `Revieweeachkf*.js"fileeforjdescription` until both halves counted
 * the same rows.
 */
test("a multi-line ask reports every line, and clips instead of overflowing", () => {
  assert.deepEqual(askPromptLines("one line?", 46), ["one line?"]);

  const three = askPromptLines("Run it?\n\n  1. describe\n  2. summarize", 46);
  assert.equal(three.length, 4, "blank lines are rows too — they are what spaces the card");

  // A question taller than a third of the screen is clipped, and says so: silently
  // dropping the tail of a spend confirmation is the one thing it may not do.
  const long = Array.from({ length: 40 }, (_, i) => `line ${i}`).join("\n");
  const clipped = askPromptLines(long, 30);
  assert.equal(clipped.length, 10, "capped at rows/3");
  assert.match(clipped.at(-1) ?? "", /… 31 more lines/);
});

/**
 * The line that says how to STOP a run was the one being cut off. Splitting on
 * newlines fixed the overpaint but left every line clipped at the card's width, so
 * the workflow approval card ended `…\`x\` in the workflows t` at 120 columns.
 */
test("a long question wraps to the card instead of being clipped mid-word", () => {
  const sentence = "It runs detached and fans out subagents in parallel, so it can spend a lot " +
    "of tokens quickly. `x` in the workflows tab (^w) stops a run at any point.";
  const wrapped = askPromptLines(sentence, 60, 60);
  assert.ok(wrapped.length > 1, "one logical line becomes several rows");
  assert.ok(wrapped.every((l) => l.length <= 56), "every row fits inside border + padding");
  // The tail survives: the whole point is that the escape hatch stays readable.
  assert.ok(wrapped.join(" ").includes("stops a run at any point."));
});

test("a question narrower than the width is left alone", () => {
  assert.deepEqual(askPromptLines("prod or staging?", 46, 120), ["prod or staging?"]);
});


test("Composer renders queued long text as a removable compact item", async () => {
  const frame = await draw(
    <Composer input="" cursor={0} busy={false} width={60} maxRows={6}
      attachments={["Pasted text #1"]} attachmentSel={0} />,
  );
  assert.ok(frame.includes("[image: Pasted text #1]"), frame);
  assert.ok(frame.includes("❯"), frame);
});
