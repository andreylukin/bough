import assert from "node:assert/strict";
import { test } from "bun:test";
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
  humanizeRetryReason,
  linkAt,
  md,
  meterLine,
  programSummary,
  rankCompletions,
  segmentParts,
  setColorEnabled,
  shortenPath,
  surface,
  toolSummary,
  truncateAnsi,
  unitLine,
  width,
  windowAround,
  wordLeft,
  wordRight,
  wrapLine,
  urlAt,
  urlAcross,
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

test("color: NO_COLOR is honored and every styled helper degrades to plain text", () => {
  withoutColor(() => {
    assert.equal(colorEnabled(), false);
    assert.equal(md("**bold** and `code`"), "bold and code");
    assert.equal(surface("hi", 10), "hi"); // no background, no padding
  });
});

// ---- wrapping ---------------------------------------------------------------

test("wrapLine: wraps at the width and keeps leading indentation", () => {
  // `trim: false` — the two leading columns are indentation the caller meant, and
  // the break keeps its space rather than reflowing the row.
  assert.deepEqual(wrapLine("  alpha beta gamma delta", 20), ["  alpha beta gamma ", "delta"]);
  assert.deepEqual(wrapLine("alpha beta", 40), ["alpha beta"]); // fits: one row
});

test("wrapLine: a word longer than the width is split, never overhung", () => {
  const out = wrapLine("x".repeat(50), 20);
  assert.equal(out.length, 3);
  for (const l of out) assert.ok(width(l) <= 20, `row too wide: ${width(l)}`);
});

test("wrapLine: a wrapped styled line measures by display width, not characters", () => {
  withColor(() => {
    const styled = md("**alphabet** soup with rather a lot of words in it");
    assert.ok(styled.length > 49); // escapes inflate the character count…
    for (const l of wrapLine(styled, 20)) {
      assert.ok(width(l) <= 20, `row measured ${width(l)}: ${JSON.stringify(l)}`);
    }
  });
});

test("wrapLine: a sub-minimum width clamps instead of producing one column", () => {
  const out = wrapLine("alpha beta gamma", 1);
  for (const l of out) assert.ok(width(l) <= 20);
  assert.ok(out.length <= 2);
});

// ---- ANSI-safe truncation ---------------------------------------------------

test("truncateAnsi: cuts to display columns and keeps escapes intact", () => {
  withColor(() => {
    const styled = md("**abcdefghij** klmno");
    const cut = truncateAnsi(styled, 6);
    assert.equal(width(cut), 6);
    assert.ok(cut.includes("\x1b["), "escapes must survive the slice");
    // slice-ansi closes what it opened, so nothing bleeds into the next row.
    assert.ok(cut.endsWith("\x1b[22m") || cut.endsWith("m"), cut);
  });
});

test("truncateAnsi: zero-width escapes are not counted as content", () => {
  withColor(() => {
    const plain = "abcdef";
    const styled = md("`abcdef`"); // same six visible columns, plus escapes
    assert.equal(width(styled), 6);
    assert.equal(truncateAnsi(plain, 10), plain); // short enough: untouched
    assert.equal(truncateAnsi(styled, 10), styled);
    assert.equal(width(truncateAnsi(styled, 3)), 3);
  });
});

test("truncateAnsi: a wide glyph never half-fills the last column", () => {
  const text = "日本語テキスト"; // 2 columns each
  assert.equal(width(text), 14);
  assert.equal(width(truncateAnsi(text, 5)), 4); // 2 glyphs fit, the third would not
  assert.equal(width(truncateAnsi(text, 6)), 6);
});

test("truncateAnsi: the ellipsis is charged against the budget", () => {
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

test("segmentParts: consecutive tool parts fold into ONE step, prose splits them", () => {
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

test("segmentParts: reasoning and a settled ask each stand alone", () => {
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

test("toolSummary: names the running call and reports error/interrupt state", () => {
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

test("codeGist: the first meaningful program line, comments skipped", () => {
  assert.equal(codeGist({ code: "// setup\n\nawait bash('ls')" }), "await bash('ls')");
  assert.equal(codeGist({ path: "x.ts" }), '{"path":"x.ts"}');
  assert.equal(codeGist(undefined), "");
  assert.equal(codeGist({ code: "x".repeat(100) }).length, 61); // clipped + ellipsis
});

test("clip / windowAround", () => {
  assert.equal(clip("abcdef", 3), "abc…");
  assert.equal(clip("abc", 3), "abc");
  assert.deepEqual(windowAround(0, 3, 10), { start: 0, end: 10 }); // shorter than the view
  assert.deepEqual(windowAround(50, 100, 10), { start: 45, end: 55 });
  assert.deepEqual(windowAround(99, 100, 10), { start: 90, end: 100 }); // clamps at the end
});

// ---- markdown-lite ----------------------------------------------------------

const LINK_OPEN = (url: string) => `\x1b]8;;${url}\x1b\\`;
const LINK_CLOSE = "\x1b]8;;\x1b\\";

test("md: markdown links become one OSC 8 hyperlink, not two", () => {
  withColor(() => {
    const out = md("see [the docs](https://example.com/x)");
    assert.ok(out.includes(LINK_OPEN("https://example.com/x")));
    assert.ok(out.includes(LINK_CLOSE));
    assert.equal(out.split("]8;;").length - 1, 2); // the dimmed (url) is not re-linked
  });
});

test("md: a code span that IS a url is clickable; one inside a command stays literal", () => {
  withColor(() => {
    const url = "http://localhost:4321/artifacts/s1/x.html";
    assert.ok(md(`\`${url}\``).includes(LINK_OPEN(url)));
    assert.ok(md(`**${url}**`).includes(LINK_OPEN(url)));
    assert.equal(md("run `curl https://example.com`").includes("]8;;"), false);
  });
});

test("md: fenced code sits on a raised surface when a width is given", () => {
  withColor(() => {
    const out = md("```js\nconst x = 1\n```", 40);
    assert.ok(out.includes("\x1b[48;"), "the block needs a background");
    assert.equal(md("plain prose", 40).includes("\x1b[48;"), false);
    const line = surface("hi", 10);
    assert.ok(line.endsWith(" ".repeat(8) + "\x1b[0m"));
  });
});

test("linkAt: resolves the hyperlink under a display column", () => {
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

test("fmtTokens / fmtUsd / ctxPctLeft", () => {
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

test("meterLine: an unknown context limit shows tokens, never a made-up percent", () => {
  assert.equal(
    meterLine({ model: "opus", costUsd: 1.5, contextTokens: 50_000, contextLimit: 200_000 }),
    "opus · $1.50 · 75% ctx left",
  );
  assert.equal(meterLine({ model: "opus", contextTokens: 50_000 }), "opus · 50k ctx");
  assert.equal(meterLine({}), "");
});

test("coldCacheNote: fires only for stale, substantial contexts", () => {
  const now = 1_000_000_000;
  assert.equal(coldCacheNote({ contextTokens: 180_000, lastLlmAt: now - 60_000 }, now), null);
  assert.equal(
    coldCacheNote({ contextTokens: 180_000, lastLlmAt: now - 6 * 60_000 }, now),
    "❄ re-caches ~180k",
  );
  assert.equal(coldCacheNote({ contextTokens: 5_000, lastLlmAt: now - 6 * 60_000 }, now), null);
  assert.equal(coldCacheNote({ contextTokens: 180_000, lastLlmAt: null }, now), null);
});

test("disconnectNote: a quiet blip first, then escalates with the elapsed time", () => {
  const t0 = 1_000_000;
  assert.deepEqual(disconnectNote(t0, t0 + 5_000), { text: "reconnecting…", urgent: false });
  const late = disconnectNote(t0, t0 + 42_000);
  assert.equal(late.urgent, true);
  assert.ok(late.text.includes("server unreachable for 42s"));
});

// ---- composer ---------------------------------------------------------------

test("wordLeft/wordRight: readline word boundaries", () => {
  const t = "what does this do";
  assert.equal(wordLeft(t, t.length), 15);
  assert.equal(wordLeft(t, 15), 10);
  assert.equal(wordLeft(t, 0), 0);
  assert.equal(wordRight(t, 0), 4);
  assert.equal(wordRight(t, 4), 9);
  assert.equal(wordRight(t, t.length), t.length);
  assert.equal(wordLeft("ab   cd", 5), 0); // a whitespace run collapses into the jump
});

test("fuzzyScore: prefix > word boundary > substring > subsequence > none", () => {
  assert.equal(fuzzyScore("exa", "ex"), 4);
  assert.equal(fuzzyScore("user-testing", "test"), 3);
  assert.equal(fuzzyScore("src/server/app.ts", "server"), 3); // "/" is a boundary too
  assert.equal(fuzzyScore("restish", "tish"), 2);
  assert.equal(fuzzyScore("wiki", "wk"), 1);
  assert.equal(fuzzyScore("commit", "xyz"), 0);
  assert.equal(fuzzyScore("anything", ""), 1);
});

test("fuzzyPositions: marks the characters that made it match", () => {
  assert.deepEqual(fuzzyPositions("exa", "ex"), [0, 1]);
  assert.deepEqual(fuzzyPositions("user-testing", "test"), [5, 6, 7, 8]);
  assert.deepEqual(fuzzyPositions("restish", "tish"), [3, 4, 5, 6]);
  assert.deepEqual(fuzzyPositions("wiki", "wk"), [0, 2]);
  assert.deepEqual(fuzzyPositions("commit", "xyz"), []);
  assert.deepEqual(fuzzyPositions("anything", ""), []);
});

test("activeTrigger: @ and / fire at ANY word boundary, not just position 0", () => {
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

test("activeTrigger: a marker mid-word is not a marker", () => {
  assert.equal(activeTrigger("src/server/app", 14), null); // a path, not a skill
  assert.equal(activeTrigger("user@host", 9), null); // an address, not a reference
  assert.equal(activeTrigger("a/b @c/d", 8)?.kind, "file"); // …but a real one still fires
});

test("activeTrigger: a finished reference stops completing", () => {
  assert.equal(activeTrigger("@src/x.ts now what", 18), null);
  assert.equal(activeTrigger("plain text", 10), null);
  assert.equal(activeTrigger("", 0), null);
});

test("activeTrigger: the token under the cursor is replaced whole, not split", () => {
  // Cursor sits mid-token; `end` runs to the next whitespace so accepting a
  // completion cannot leave the tail of the old word behind.
  const t = activeTrigger("@ser/app.ts tail", 4)!;
  assert.equal(t.query, "ser");
  assert.equal(t.end, 11);
});

test("rankCompletions + applyCompletion: replace the token, report what was hidden", () => {
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

test("rankCompletions: a directory candidate inserts without a trailing space", () => {
  const trigger = activeTrigger("@sr", 3)!;
  const { items } = rankCompletions([{ name: "src/" }], trigger);
  assert.equal(items[0].insert, "@src/"); // keep typing into the directory
});

test("rankCompletions: a skill trigger marks rows with the slash it will insert", () => {
  const trigger = activeTrigger("/his", 4)!;
  const { items } = rankCompletions([{ name: "history", detail: "query bough's SQLite" }], trigger);
  assert.equal(items[0].label, "/history");
  assert.equal(items[0].insert, "/history ");
  assert.equal(items[0].detail, "query bough's SQLite");
});

// ---------------------------------------------------------------------------
// The header's context line
// ---------------------------------------------------------------------------

test("the meter carries the whole session status, in one line at the bottom", () => {
  // Everything a user needs before pressing enter, beside the composer rather
  // than on a top line a screenful away: where it runs, what it costs, what is
  // left, and how to get help.
  assert.equal(
    meterLine({
      workspace: "~/repos/x",
      model: "claude-opus-5",
      costUsd: 0.0072,
      contextTokens: 18_000,
      contextLimit: 200_000,
      help: true,
    }),
    "~/repos/x · claude-opus-5 · $0.007 · 91% ctx left · ? help",
  );
  // A fresh conversation has no model, no spend and no context — and must still
  // say where it will run and how to get help, because that is the screen a
  // first-run user is looking at.
  assert.equal(
    meterLine({ workspace: "~/repos/x", help: true }),
    "~/repos/x · ? help",
  );
  // An unpriced model has no window in the catalog, so the raw count stands in
  // rather than a fabricated percentage.
  assert.equal(
    meterLine({ model: "who/knows", contextTokens: 18_000, contextLimit: null }),
    "who/knows · 18k ctx",
  );
});

test("a narrow terminal degrades the meter instead of wrapping it", () => {
  // A status bar that reflows onto a second row steals a line from the transcript
  // and reads as a rendering bug — which is what 60 columns used to produce.
  const m = {
    workspace: "~/repos/bough",
    model: "moonshotai/kimi-k3",
    costUsd: 0.007,
    contextTokens: 18_000,
    contextLimit: 1_048_576,
    help: true,
  };
  assert.equal(
    meterLine({ ...m, width: 200 }),
    "~/repos/bough · moonshotai/kimi-k3 · $0.007 · 98% ctx left · ? help",
  );
  // Every degraded form still fits, and the two live numbers survive longest.
  for (const w of [70, 60, 50, 40, 30, 20, 12]) {
    const line = meterLine({ ...m, width: w });
    assert.ok(width(line) <= w, `width ${w} produced ${width(line)} cols: ${line}`);
  }
  // The workspace shortens to its basename before it disappears entirely.
  assert.match(meterLine({ ...m, width: 60 }), /bough/);
  // At the narrowest, context left is the last thing standing.
  assert.equal(meterLine({ ...m, width: 14 }), "98% ctx left");
});

test("shortenPath only abbreviates a real home prefix", () => {
  assert.equal(shortenPath("/Users/me", "/Users/me"), "~");
  assert.equal(shortenPath("/Users/me/x", "/Users/me/"), "~/x");
  // A sibling directory that merely starts with the same characters is not home.
  assert.equal(shortenPath("/Users/mentor/x", "/Users/me"), "/Users/mentor/x");
  assert.equal(shortenPath("/a/b", ""), "/a/b");
});

test("the busy line always names motion, elapsed time, and the way out", () => {
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

test("the busy line carries the turn's own tokens while it runs — but not its cost", () => {
  // G3: `busyLine` accepted tokens from the day it was written and nobody passed them,
  // so a ten-minute turn said "17s" and nothing else.
  // Cost per turn was then asked for and REMOVED: the session total on the status row
  // is the number that matters, and a dollar figure per turn is noise at the density a
  // transcript is read at.
  assert.equal(
    busyLine({ activity: null, elapsedMs: 42_000, tick: 0, tokens: 3_200 }),
    "⠋ working · 42s · 3.2k tok · esc interrupts",
  );
  // A provider that reports usage only at the end leaves zeros here, and a zero is
  // omitted rather than printed: the line degrades to what it always said.
  assert.equal(
    busyLine({ elapsedMs: 1_000, tick: 0, tokens: 0 }),
    "⠋ working · 1s · esc interrupts",
  );
});

test("a rail row attributes elapsed, tokens and spend to ONE unit", () => {
  // G5: every row used to read `◆ <title>  ⋯ working`, which cannot tell a stuck agent
  // from a slow one — two identical rows, one of them wedged.
  const base = {
    id: "s1",
    sessionId: "s1",
    title: "review app.ts",
    elapsedMs: 132_000,
    tokens: 3_200,
    costUsd: 0.021,
    progress: null,
    detail: null,
  };
  const shell = {
    ...base,
    kind: "shell" as const,
    title: "bg_7",
    tokens: null,
    costUsd: null,
    detail: "sleep 90",
  };
  withoutColor(() => {
    assert.equal(
      unitLine({ ...base, kind: "subagent" }, 80),
      "◆ review app.ts  2m12s · 3.2k tok · $0.021",
    );
    // A shell spends nothing and IS its command, so the command is what identifies it.
    assert.equal(unitLine(shell, 80), "⚙ bg_7  2m12s · sleep 90");
    // The detail is the only thing that clips, and it is dropped whole rather than
    // rendered as an ellipsis when there is no room: the numbers are the message.
    assert.equal(unitLine(shell, 20), "⚙ bg_7  2m12s");
  });
});

test("a bar is drawn only from a fraction somebody actually knows", () => {
  // Spec §9's failure mode is the invented percentage, not the missing one: a null
  // progress renders NO bar rather than an empty trough.
  const run = {
    kind: "workflow" as const,
    id: "r1",
    sessionId: "r1",
    title: "nightly bench",
    elapsedMs: 8_000,
    tokens: null,
    costUsd: null,
    detail: null,
  };
  withoutColor(() => {
    assert.equal(unitLine({ ...run, progress: 0.5 }, 80), "⧉ nightly bench  8s · ████░░░░ 50%");
    assert.equal(unitLine({ ...run, progress: 1 }, 80), "⧉ nightly bench  8s · ████████ 100%");
    assert.equal(unitLine({ ...run, progress: null }, 80), "⧉ nightly bench  8s");
  });
});

test("fmtDuration stays readable from one second to hours", () => {
  assert.equal(fmtDuration(0), "0s");
  assert.equal(fmtDuration(9_400), "9s");
  assert.equal(fmtDuration(59_999), "59s");
  assert.equal(fmtDuration(64_000), "1m04s");
  assert.equal(fmtDuration(3_600_000), "1h00m");
  assert.equal(fmtDuration(-5), "0s");
});

test("a retry reason is reduced to something a person can read", () => {
  // The real one, off a first-run user's screen mid-turn.
  assert.equal(
    humanizeRetryReason('openrouter: 429 {"error":{"message":"Provider returned error"}}'),
    "rate limited · Provider returned error",
  );
  // A bare status still names itself — the number IS the meaning.
  assert.equal(humanizeRetryReason("503"), "provider overloaded");
  // Prose with no payload passes through unchanged.
  assert.equal(humanizeRetryReason("connection reset"), "connection reset");
  // An unrecognized reason is shown, just shorter — never classified by guess.
  assert.equal(humanizeRetryReason("something odd happened"), "something odd happened");
  assert.equal(humanizeRetryReason(""), "no reason given");
  // Always bounded, so it cannot crowd the notice row.
  const long = humanizeRetryReason("x".repeat(500));
  assert.ok(long.length <= 60, long.length.toString());
  // No JSON ever reaches the screen.
  for (const raw of ['{"error":{"code":429}}', 'openrouter: 500 {"a":[1,2]}', "429 {}"]) {
    const out = humanizeRetryReason(raw);
    assert.ok(!out.includes("{") && !out.includes('"'), `leaked JSON: ${out}`);
  }
});

test("a step is headlined by what the program did, not by its first line of code", () => {
  // The old header was a clipped source line — debug output, not a UI:
  //   ▸ 1 step  run_steps · const out = await bash(`node --input-type=module -e "
  assert.equal(
    programSummary('const out = await bash("node -e 1"); console.log(out);'),
    "ran 1 command",
  );
  assert.equal(programSummary('console.log(await view("/tmp/x/app.mjs"));'), "read app.mjs");
  assert.equal(
    programSummary('await write("src/a.ts", body); await bash("deno test");'),
    "wrote a.ts · ran 1 command",
  );
  // Several files collapse rather than running off the row.
  assert.equal(
    programSummary('await edit("a.ts", x); await edit("b.ts", y); await edit("c.ts", z);'),
    "wrote a.ts +2 more",
  );
  assert.equal(
    programSummary('await Promise.all([agent("one"), agent("two")]);'),
    "2 subagents",
  );
  // Unrecognized programs yield "", so the caller falls back to the code gist
  // rather than to an empty header.
  assert.equal(programSummary("const x = 1 + 1;"), "");
  assert.equal(programSummary(""), "");
  // Always bounded — this shares a row with the step count and the status chips.
  assert.ok(programSummary('await bash("x");'.repeat(40)).length <= 64);
});

// ---- bare URLs, on surfaces that emit no OSC 8 -------------------------------
// The transcript marks its links; a panel message, a rail row and a job card are
// plain text. The mcp tab's authorization URL lives in one of those, wrapped over
// five rows — the one URL in the product nobody can retype.

test("urlAt finds the address under a column, and only under it", () => {
  const row = "open this: https://example.com/a?b=1 for details";
  assert.equal(urlAt(row, 11)?.url, "https://example.com/a?b=1");
  assert.equal(urlAt(row, 35)?.url, "https://example.com/a?b=1"); // last char
  assert.equal(urlAt(row, 5), null); // before it
  assert.equal(urlAt(row, 40), null); // after it
});

test("urlAt drops trailing sentence punctuation", () => {
  assert.equal(urlAt("see https://example.com/x.", 10)?.url, "https://example.com/x");
});

test("urlAcross rejoins a URL wrapped over several rows", () => {
  // Exactly the shape of an OAuth link in the mcp tab: it runs to the edge of each
  // row and continues on the next with no space.
  // Equal-width rows, because that is what a wrap produces: each row ran out of
  // room. The last is short — it is where the address ended.
  const rows = [
    "open this, then come back: https://mcp.example.com/authorize?response_ty",
    "pe=code&client_id=abc&code_challenge=xyz&redirect_uri=http%3A%2F%2F127.0",
    ".0.1%3A4399&scope=read+write",
  ];
  assert.equal(
    urlAcross(rows, 0, 30),
    "https://mcp.example.com/authorize?response_type=code" +
      "&client_id=abc&code_challenge=xyz&redirect_uri=http%3A%2F%2F127.0" +
      ".0.1%3A4399&scope=read+write",
  );
});

test("urlAcross does NOT glue the next row onto a URL that already ended", () => {
  // The guard that keeps it from inventing addresses: a URL ending mid-row is
  // whole, whatever happens to be on the row below it.
  const rows = ["see https://example.com/a and more text", "notacontinuation"];
  assert.equal(urlAcross(rows, 0, 10), "https://example.com/a");
});

test("urlAcross stops when a continuation row carries anything else", () => {
  // A row that begins with a URL-ish token but goes on to hold other words is not a
  // continuation at all: an address has no spaces, so a row with one is prose or the
  // next list entry. Nothing of it is taken.
  const rows = ["https://example.com/averylongpathrunningtotheedge", "tail and then words"];
  assert.equal(urlAcross(rows, 0, 5), "https://example.com/averylongpathrunningtotheedge");
});

test("urlAcross does not weld the row BELOW a finished address onto it", () => {
  // The exact shape that broke it live: the last row of a wrapped authorization
  // URL is short — the address ended there — and the mcp list row under it starts
  // with "1". Joining on "both sides look URL-ish" opened the link with a stray
  // digit welded on. A wrap only happens on a row that ran out of room.
  const rows = [
    "open this: https://mcp.example.com/authorize?response_type=code&client_i",
    "d=abc&resource=https%3A%2F%2Fmcp.example.com%2Fmcp",
    "1 linear  off · needs auth",
  ];
  const url = urlAcross(rows, 1, 10);
  assert.equal(
    url,
    "https://mcp.example.com/authorize?response_type=code&client_i" +
      "d=abc&resource=https%3A%2F%2Fmcp.example.com%2Fmcp",
  );
  assert.equal(url?.endsWith("1"), false, `a row below leaked in: ${url}`);
});

test("clicking a CONTINUATION row resolves the whole address", () => {
  // The click usually lands mid-URL, on a row carrying no scheme at all — which is
  // why the search has to run backward before it runs forward.
  const rows = [
    "open this: https://mcp.example.com/authorize?response_type=code&client_i",
    "d=abc&scope=read",
  ];
  assert.equal(
    urlAcross(rows, 1, 3),
    "https://mcp.example.com/authorize?response_type=code&client_id=abc&scope=read",
  );
});
