import assert from "node:assert/strict";
import {
  activeTrigger,
  applyCompletion,
  busyLine,
  clip,
  codeGist,
  coldCacheNote,
  colorEnabled,
  ctxPctLeft,
  disconnectNote,
  fmtDuration,
  fmtTokens,
  fmtUsd,
  fuzzyPositions,
  fuzzyScore,
  headerContext,
  linkAt,
  md,
  meterLine,
  rankCompletions,
  segmentParts,
  setColorEnabled,
  shortenPath,
  surface,
  toolSummary,
  truncateAnsi,
  width,
  windowAround,
  wordLeft,
  wordRight,
  wrapLine,
} from "./format.ts";
import type { Part } from "../schema/parts.ts";

// Every assertion below runs with no terminal attached. The color-dependent ones
// force color ON rather than skipping, so a CI run under NO_COLOR still covers the
// escape-sequence rules (the port's tests silently vanished in that environment).
function withColor<T>(fn: () => T): T {
  const was = setColorEnabled(true);
  try {
    return fn();
  } finally {
    setColorEnabled(was);
  }
}

function withoutColor<T>(fn: () => T): T {
  const was = setColorEnabled(false);
  try {
    return fn();
  } finally {
    setColorEnabled(was);
  }
}

Deno.test("color: NO_COLOR is honored and every styled helper degrades to plain text", () => {
  withoutColor(() => {
    assert.equal(colorEnabled(), false);
    assert.equal(md("**bold** and `code`"), "bold and code");
    assert.equal(surface("hi", 10), "hi"); // no background, no padding
  });
});

// ---- wrapping ---------------------------------------------------------------

Deno.test("wrapLine: wraps at the width and keeps leading indentation", () => {
  // `trim: false` — the two leading columns are indentation the caller meant, and
  // the break keeps its space rather than reflowing the row.
  assert.deepEqual(wrapLine("  alpha beta gamma delta", 20), ["  alpha beta gamma ", "delta"]);
  assert.deepEqual(wrapLine("alpha beta", 40), ["alpha beta"]); // fits: one row
});

Deno.test("wrapLine: a word longer than the width is split, never overhung", () => {
  const out = wrapLine("x".repeat(50), 20);
  assert.equal(out.length, 3);
  for (const l of out) assert.ok(width(l) <= 20, `row too wide: ${width(l)}`);
});

Deno.test("wrapLine: a wrapped styled line measures by display width, not characters", () => {
  withColor(() => {
    const styled = md("**alphabet** soup with rather a lot of words in it");
    assert.ok(styled.length > 49); // escapes inflate the character count…
    for (const l of wrapLine(styled, 20)) {
      assert.ok(width(l) <= 20, `row measured ${width(l)}: ${JSON.stringify(l)}`);
    }
  });
});

Deno.test("wrapLine: a sub-minimum width clamps instead of producing one column", () => {
  const out = wrapLine("alpha beta gamma", 1);
  for (const l of out) assert.ok(width(l) <= 20);
  assert.ok(out.length <= 2);
});

// ---- ANSI-safe truncation ---------------------------------------------------

Deno.test("truncateAnsi: cuts to display columns and keeps escapes intact", () => {
  withColor(() => {
    const styled = md("**abcdefghij** klmno");
    const cut = truncateAnsi(styled, 6);
    assert.equal(width(cut), 6);
    assert.ok(cut.includes("\x1b["), "escapes must survive the slice");
    // slice-ansi closes what it opened, so nothing bleeds into the next row.
    assert.ok(cut.endsWith("\x1b[22m") || cut.endsWith("m"), cut);
  });
});

Deno.test("truncateAnsi: zero-width escapes are not counted as content", () => {
  withColor(() => {
    const plain = "abcdef";
    const styled = md("`abcdef`"); // same six visible columns, plus escapes
    assert.equal(width(styled), 6);
    assert.equal(truncateAnsi(plain, 10), plain); // short enough: untouched
    assert.equal(truncateAnsi(styled, 10), styled);
    assert.equal(width(truncateAnsi(styled, 3)), 3);
  });
});

Deno.test("truncateAnsi: a wide glyph never half-fills the last column", () => {
  const text = "日本語テキスト"; // 2 columns each
  assert.equal(width(text), 14);
  assert.equal(width(truncateAnsi(text, 5)), 4); // 2 glyphs fit, the third would not
  assert.equal(width(truncateAnsi(text, 6)), 6);
});

Deno.test("truncateAnsi: the ellipsis is charged against the budget", () => {
  assert.equal(truncateAnsi("abcdefghij", 5, "…"), "abcd…");
  assert.equal(width(truncateAnsi("abcdefghij", 5, "…")), 5);
  assert.equal(truncateAnsi("abc", 5, "…"), "abc"); // fits: no ellipsis
  assert.equal(truncateAnsi("abcdef", 1, "…"), ""); // no room for content at all
  assert.equal(truncateAnsi("abcdef", 0), "");
});

// ---- folding rules ----------------------------------------------------------

const call = (id: string, code: string): Part => ({
  type: "tool_call",
  id,
  name: "run_steps",
  input: { code },
});
const result = (callId: string, output: string, extra: Partial<Part> = {}): Part => ({
  type: "tool_result",
  callId,
  output,
  isError: false,
  ...extra,
} as Part);

Deno.test("segmentParts: consecutive tool parts fold into ONE step, prose splits them", () => {
  const segs = segmentParts([
    { type: "text", text: "first" },
    call("c1", "a()"),
    result("c1", "ok"),
    call("c2", "b()"),
    result("c2", "ok"),
    { type: "text", text: "second" },
    call("c3", "c()"),
  ]);
  assert.deepEqual(segs.map((s) => s.kind), ["text", "tools", "text", "tools"]);
  // Two calls and two results in the first group — one collapsed step, not four.
  assert.equal((segs[1] as { parts: Part[] }).parts.length, 4);
  assert.equal((segs[3] as { parts: Part[] }).parts.length, 1);
});

Deno.test("segmentParts: reasoning and a settled ask each stand alone", () => {
  const segs = segmentParts([
    { type: "reasoning", text: "thinking" },
    call("c1", "a()"),
    { type: "ask", id: "q1", question: "ok?", status: "answered", answer: "yes" },
    result("c1", "ok"),
  ]);
  // The ask breaks the tool run: it is a human exchange, not tool plumbing, and
  // folding it would hide an answer the user gave.
  assert.deepEqual(segs.map((s) => s.kind), ["reasoning", "tools", "ask", "tools"]);
});

Deno.test("toolSummary: names the running call and reports error/interrupt state", () => {
  const done = toolSummary([call("c1", "a()"), result("c1", "ok")]);
  assert.equal(done.running, undefined);
  assert.equal(done.hasError, false);
  assert.equal(done.interrupted, false);

  const live = toolSummary([call("c1", "a()"), result("c1", "ok"), call("c2", "b()")]);
  assert.equal(live.running?.id, "c2");

  const bad = toolSummary([call("c1", "a()"), result("c1", "boom", { isError: true })]);
  assert.equal(bad.hasError, true);

  const stopped = toolSummary([call("c1", "a()"), result("c1", "partial", { interrupted: true })]);
  assert.equal(stopped.interrupted, true);
  assert.equal(stopped.hasError, false); // "you stopped it" ≠ "it failed"
});

Deno.test("codeGist: the first meaningful program line, comments skipped", () => {
  assert.equal(codeGist({ code: "// setup\n\nawait bash('ls')" }), "await bash('ls')");
  assert.equal(codeGist({ path: "x.ts" }), '{"path":"x.ts"}');
  assert.equal(codeGist(undefined), "");
  assert.equal(codeGist({ code: "x".repeat(100) }).length, 61); // clipped + ellipsis
});

Deno.test("clip / windowAround", () => {
  assert.equal(clip("abcdef", 3), "abc…");
  assert.equal(clip("abc", 3), "abc");
  assert.deepEqual(windowAround(0, 3, 10), { start: 0, end: 10 }); // shorter than the view
  assert.deepEqual(windowAround(50, 100, 10), { start: 45, end: 55 });
  assert.deepEqual(windowAround(99, 100, 10), { start: 90, end: 100 }); // clamps at the end
});

// ---- markdown-lite ----------------------------------------------------------

const LINK_OPEN = (url: string) => `\x1b]8;;${url}\x1b\\`;
const LINK_CLOSE = "\x1b]8;;\x1b\\";

Deno.test("md: markdown links become one OSC 8 hyperlink, not two", () => {
  withColor(() => {
    const out = md("see [the docs](https://example.com/x)");
    assert.ok(out.includes(LINK_OPEN("https://example.com/x")));
    assert.ok(out.includes(LINK_CLOSE));
    assert.equal(out.split("]8;;").length - 1, 2); // the dimmed (url) is not re-linked
  });
});

Deno.test("md: a code span that IS a url is clickable; one inside a command stays literal", () => {
  withColor(() => {
    const url = "http://localhost:4321/artifacts/s1/x.html";
    assert.ok(md(`\`${url}\``).includes(LINK_OPEN(url)));
    assert.ok(md(`**${url}**`).includes(LINK_OPEN(url)));
    assert.equal(md("run `curl https://example.com`").includes("]8;;"), false);
  });
});

Deno.test("md: fenced code sits on a raised surface when a width is given", () => {
  withColor(() => {
    const out = md("```js\nconst x = 1\n```", 40);
    assert.ok(out.includes("\x1b[48;"), "the block needs a background");
    assert.equal(md("plain prose", 40).includes("\x1b[48;"), false);
    const line = surface("hi", 10);
    assert.ok(line.endsWith(" ".repeat(8) + "\x1b[0m"));
  });
});

Deno.test("linkAt: resolves the hyperlink under a display column", () => {
  withColor(() => {
    const line = md("go **now** [docs](https://example.com/x) end");
    assert.equal(linkAt(line, 0), null);
    assert.equal(linkAt(line, 7), "https://example.com/x");
    assert.equal(linkAt(line, 34), "https://example.com/x");
    assert.equal(linkAt(line, 37), null);
    assert.equal(linkAt(line, 200), null);
  });
});

// ---- numbers ----------------------------------------------------------------

Deno.test("fmtTokens / fmtUsd / ctxPctLeft", () => {
  assert.equal(fmtTokens(999), "999");
  assert.equal(fmtTokens(1234), "1.2k");
  assert.equal(fmtTokens(184_000), "184k");
  assert.equal(fmtUsd(1.234), "$1.23");
  assert.equal(fmtUsd(0.0042), "$0.004");
  assert.equal(fmtUsd(0.00004), "$0.0000");
  assert.equal(ctxPctLeft({ contextTokens: 50_000, contextLimit: 200_000 }), 75);
  assert.equal(ctxPctLeft({ contextTokens: 300_000, contextLimit: 200_000 }), 0);
  assert.equal(ctxPctLeft({ contextTokens: 10, contextLimit: null }), null);
});

Deno.test("meterLine: an unknown context limit shows tokens, never a made-up percent", () => {
  assert.equal(
    meterLine({ model: "opus", costUsd: 1.5, contextTokens: 50_000, contextLimit: 200_000 }),
    "opus · $1.50 · 75% ctx left",
  );
  assert.equal(meterLine({ model: "opus", contextTokens: 50_000 }), "opus · 50k ctx");
  assert.equal(meterLine({}), "");
});

Deno.test("coldCacheNote: fires only for stale, substantial contexts", () => {
  const now = 1_000_000_000;
  assert.equal(coldCacheNote({ contextTokens: 180_000, lastLlmAt: now - 60_000 }, now), null);
  assert.equal(
    coldCacheNote({ contextTokens: 180_000, lastLlmAt: now - 6 * 60_000 }, now),
    "❄ re-caches ~180k",
  );
  assert.equal(coldCacheNote({ contextTokens: 5_000, lastLlmAt: now - 6 * 60_000 }, now), null);
  assert.equal(coldCacheNote({ contextTokens: 180_000, lastLlmAt: null }, now), null);
});

Deno.test("disconnectNote: a quiet blip first, then escalates with the elapsed time", () => {
  const t0 = 1_000_000;
  assert.deepEqual(disconnectNote(t0, t0 + 5_000), { text: "reconnecting…", urgent: false });
  const late = disconnectNote(t0, t0 + 42_000);
  assert.equal(late.urgent, true);
  assert.ok(late.text.includes("server unreachable for 42s"));
});

// ---- composer ---------------------------------------------------------------

Deno.test("wordLeft/wordRight: readline word boundaries", () => {
  const t = "what does this do";
  assert.equal(wordLeft(t, t.length), 15);
  assert.equal(wordLeft(t, 15), 10);
  assert.equal(wordLeft(t, 0), 0);
  assert.equal(wordRight(t, 0), 4);
  assert.equal(wordRight(t, 4), 9);
  assert.equal(wordRight(t, t.length), t.length);
  assert.equal(wordLeft("ab   cd", 5), 0); // a whitespace run collapses into the jump
});

Deno.test("fuzzyScore: prefix > word boundary > substring > subsequence > none", () => {
  assert.equal(fuzzyScore("exa", "ex"), 4);
  assert.equal(fuzzyScore("user-testing", "test"), 3);
  assert.equal(fuzzyScore("src/server/app.ts", "server"), 3); // "/" is a boundary too
  assert.equal(fuzzyScore("restish", "tish"), 2);
  assert.equal(fuzzyScore("wiki", "wk"), 1);
  assert.equal(fuzzyScore("commit", "xyz"), 0);
  assert.equal(fuzzyScore("anything", ""), 1);
});

Deno.test("fuzzyPositions: marks the characters that made it match", () => {
  assert.deepEqual(fuzzyPositions("exa", "ex"), [0, 1]);
  assert.deepEqual(fuzzyPositions("user-testing", "test"), [5, 6, 7, 8]);
  assert.deepEqual(fuzzyPositions("restish", "tish"), [3, 4, 5, 6]);
  assert.deepEqual(fuzzyPositions("wiki", "wk"), [0, 2]);
  assert.deepEqual(fuzzyPositions("commit", "xyz"), []);
  assert.deepEqual(fuzzyPositions("anything", ""), []);
});

Deno.test("activeTrigger: @ and / fire at ANY word boundary, not just position 0", () => {
  assert.deepEqual(activeTrigger("@src", 4), { kind: "file", query: "src", start: 0, end: 4 });
  assert.deepEqual(activeTrigger("look at @src", 12), {
    kind: "file",
    query: "src",
    start: 8,
    end: 12,
  });
  assert.deepEqual(activeTrigger("/com", 4), { kind: "skill", query: "com", start: 0, end: 4 });
  assert.deepEqual(activeTrigger("fix this /com", 13), {
    kind: "skill",
    query: "com",
    start: 9,
    end: 13,
  });
  // A bare marker completes everything — the menu opens on the marker alone.
  assert.deepEqual(activeTrigger("@", 1), { kind: "file", query: "", start: 0, end: 1 });
});

Deno.test("activeTrigger: a marker mid-word is not a marker", () => {
  assert.equal(activeTrigger("src/server/app", 14), null); // a path, not a skill
  assert.equal(activeTrigger("user@host", 9), null); // an address, not a reference
  assert.equal(activeTrigger("a/b @c/d", 8)?.kind, "file"); // …but a real one still fires
});

Deno.test("activeTrigger: a finished reference stops completing", () => {
  assert.equal(activeTrigger("@src/x.ts now what", 18), null);
  assert.equal(activeTrigger("plain text", 10), null);
  assert.equal(activeTrigger("", 0), null);
});

Deno.test("activeTrigger: the token under the cursor is replaced whole, not split", () => {
  // Cursor sits mid-token; `end` runs to the next whitespace so accepting a
  // completion cannot leave the tail of the old word behind.
  const t = activeTrigger("@ser/app.ts tail", 4)!;
  assert.equal(t.query, "ser");
  assert.equal(t.end, 11);
});

Deno.test("rankCompletions + applyCompletion: replace the token, report what was hidden", () => {
  const trigger = activeTrigger("look at @app", 12)!;
  const files = [
    { name: "server/app.ts" },
    { name: "app.tsx", detail: "" },
    { name: "components/Chat.tsx" },
    { name: "apparatus/x.ts" },
    { name: "a/p/p.ts" },
    { name: "docs/app.md" },
    { name: "old/app.js" },
    { name: "zap/app.rs" },
  ];
  const { items, total } = rankCompletions(files, trigger, 3);
  assert.equal(items.length, 3);
  assert.equal(total, 7); // "components/Chat.tsx" does not match at all
  assert.equal(items[0].label, "@app.tsx"); // exact prefix wins
  assert.ok(items[0].hl!.length > 0);
  const applied = applyCompletion("look at @app", trigger, items[0]);
  assert.deepEqual(applied, { text: "look at @app.tsx ", cursor: 17 });
});

Deno.test("rankCompletions: a directory candidate inserts without a trailing space", () => {
  const trigger = activeTrigger("@sr", 3)!;
  const { items } = rankCompletions([{ name: "src/" }], trigger);
  assert.equal(items[0].insert, "@src/"); // keep typing into the directory
});

Deno.test("rankCompletions: a skill trigger marks rows with the slash it will insert", () => {
  const trigger = activeTrigger("/his", 4)!;
  const { items } = rankCompletions([{ name: "history", detail: "query bough's SQLite" }], trigger);
  assert.equal(items[0].label, "/history");
  assert.equal(items[0].insert, "/history ");
  assert.equal(items[0].detail, "query bough's SQLite");
});

// ---------------------------------------------------------------------------
// The header's context line
// ---------------------------------------------------------------------------

Deno.test("the header says where the turn will run, and always offers help", () => {
  // bough edits the real checkout as the user (spec §2), so the workspace is a
  // fact you need BEFORE pressing enter — including on a fresh conversation,
  // whose title ("new conversation") tells you nothing.
  assert.equal(
    headerContext("/Users/me/repos/x", "/Users/me"),
    "~/repos/x · ? help",
  );
  assert.equal(headerContext("/srv/app", "/Users/me"), "/srv/app · ? help");
  // No workspace yet is not a reason to hide the only route to the keymap.
  assert.equal(headerContext(null, "/Users/me"), "? help");
  assert.equal(headerContext("", null), "? help");
});

Deno.test("shortenPath only abbreviates a real home prefix", () => {
  assert.equal(shortenPath("/Users/me", "/Users/me"), "~");
  assert.equal(shortenPath("/Users/me/x", "/Users/me/"), "~/x");
  // A sibling directory that merely starts with the same characters is not home.
  assert.equal(shortenPath("/Users/mentor/x", "/Users/me"), "/Users/mentor/x");
  assert.equal(shortenPath("/a/b", ""), "/a/b");
});

Deno.test("the busy line always names motion, elapsed time, and the way out", () => {
  // The regression it prevents: a running turn that has printed nothing looked
  // identical to a hung terminal, and esc — the fix for a hung terminal — was
  // documented on no screen.
  const line = busyLine({ activity: null, elapsedMs: 9_000, tick: 0 });
  assert.equal(line, "⠋ working · 9s · esc interrupts");
  // The cheap-tier blurb rides along when there is one, instead of replacing it.
  assert.equal(
    busyLine({ activity: "reading keys.ts", elapsedMs: 0, tick: 1 }),
    "⠙ reading keys.ts · 0s · esc interrupts",
  );
  // A blank blurb is the same as no blurb — never an empty middle field.
  assert.equal(
    busyLine({ activity: "   ", elapsedMs: 0, tick: 0 }),
    "⠋ working · 0s · esc interrupts",
  );
  // The spinner cycles rather than running off the end of the frame list.
  const frames = new Set(
    Array.from({ length: 40 }, (_, i) => busyLine({ elapsedMs: 0, tick: i }).slice(0, 1)),
  );
  assert.equal(frames.size, 10);
});

Deno.test("fmtDuration stays readable from one second to hours", () => {
  assert.equal(fmtDuration(0), "0s");
  assert.equal(fmtDuration(9_400), "9s");
  assert.equal(fmtDuration(59_999), "59s");
  assert.equal(fmtDuration(64_000), "1m04s");
  assert.equal(fmtDuration(3_600_000), "1h00m");
  assert.equal(fmtDuration(-5), "0s");
});
