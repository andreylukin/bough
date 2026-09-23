import { expect, mock, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
mock.module("dompurify", () => ({ default: { sanitize: (s: string) => s } }));
const { TurnView } = await import("../src/app");
const { groupTurns } = await import("../src/render");
const { foldPastes, parsePrompt, wrapPaste } = await import("../src/prompt");
import type { Line } from "../src/types";

const at = (s: number) => new Date(Date.parse("2026-01-02T10:00:00Z") + s * 1000).toISOString();
const log = Array.from({ length: 40 }, (_, i) => `PASTED_LOG_LINE_${i}`).join("\n");
const skill = "[skill: exa]\nEXA_SKILL_BODY: search the web";

test("a wrapped paste and an injected skill fold to chips; what you typed stays", () => {
  const p = parsePrompt(`look at ${wrapPaste(log, 40)} then /exa it\n\n${skill}`);
  expect(p.atts.map((a) => [a.kind, a.label])).toEqual([["paste", "Pasted 40 lines"], ["skill", "/exa"]]);
  expect(p.atts[0].body).toBe(log);
  expect(p.plain).toBe("look at [Pasted 40 lines] then /exa it");
  // Copy and edit still get the whole message.
  expect(p.raw).toBe(`look at ${wrapPaste(log, 40)} then /exa it`);
});

test("a paste that holds an attachment header does not split the prompt there", () => {
  const src = "a\n\n[file: x.go]\npackage x";
  const p = parsePrompt(`see ${wrapPaste(src, 4)}`);
  expect(p.atts).toHaveLength(1);
  expect(p.atts[0].body).toBe(src);
});

test("a sent paste folds back into its composer tag on edit", () => {
  const kept: string[] = [];
  expect(foldPastes(`x ${wrapPaste(log, 40)} y`, (b) => kept.push(b))).toBe("x [Pasted text #1 +40 lines] y");
  expect(kept).toEqual([log]);
});

test("a steer with a paste and a skill shows chips, not the pasted text or SKILL.md", () => {
  const lines: Line[] = [
    { seq: 1, at: at(0), kind: "input", text: "start" },
    { seq: 2, at: at(1), kind: "input", text: `also ${wrapPaste(log, 40)} /exa\n\n${skill}`, data: { steer: true } },
  ];
  const html = renderToStaticMarkup(<TurnView turn={groupTurns(lines)[0]} />);
  expect(html).toContain("steer-bubble");
  expect(html).toContain("Pasted 40 lines");
  expect(html).toContain(">/exa</button>");
  expect(html).not.toContain("PASTED_LOG_LINE_1");
  expect(html).not.toContain("EXA_SKILL_BODY");
  expect(html).not.toContain("pasted-text");
});

test("a recorded prompt's paste is a chip in place, and the clamp measures the folded text", () => {
  const lines: Line[] = [{ seq: 1, at: at(0), kind: "input", text: `fix ${wrapPaste(log, 40)} please` }];
  const html = renderToStaticMarkup(<TurnView turn={groupTurns(lines)[0]} />);
  expect(html).toMatch(/fix <button[^>]*prompt-chip-paste[^>]*>Pasted 40 lines<\/button> please/);
  expect(html).not.toContain("PASTED_LOG_LINE_1");
  expect(html).not.toContain("Show full prompt");
});
