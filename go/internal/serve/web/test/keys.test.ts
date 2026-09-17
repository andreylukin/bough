import { expect, test } from "bun:test";
import { BINDINGS, overviewKeys, paletteKeyOpens, sheetKey, treeKey } from "../src/keys";

const ev = (o: Partial<{ key: string; metaKey: boolean; ctrlKey: boolean; altKey: boolean; shiftKey: boolean }>, tag = "DIV") =>
  ({ key: "k", metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, ...o, target: { tagName: tag } as unknown as EventTarget });

// Ctrl+K in a Mac text field is kill-line; it used to open the palette.
test("Ctrl+K in a Mac text field is left to the field", () => {
  expect(paletteKeyOpens(ev({ ctrlKey: true }, "TEXTAREA"), true)).toBe(false);
  expect(paletteKeyOpens(ev({ ctrlKey: true }, "INPUT"), true)).toBe(false);
  expect(paletteKeyOpens(ev({ metaKey: true }, "TEXTAREA"), true)).toBe(true);
  expect(paletteKeyOpens(ev({ ctrlKey: true }, "DIV"), true)).toBe(true);
  expect(paletteKeyOpens(ev({ ctrlKey: true }, "TEXTAREA"), false)).toBe(true);
  expect(paletteKeyOpens(ev({ key: "j", metaKey: true }), true)).toBe(false);
});

test("? opens the sheet only outside typing", () => {
  expect(sheetKey(ev({ key: "?", shiftKey: true }))).toBe(true);
  expect(sheetKey(ev({ key: "?", shiftKey: true }, "INPUT"))).toBe(false);
  expect(sheetKey(ev({ key: "?", metaKey: true }))).toBe(false);
});

test("j and k walk the sidebar like the arrows", () => {
  expect(treeKey("j")).toBe("ArrowDown");
  expect(treeKey("k")).toBe("ArrowUp");
  expect(treeKey("ArrowLeft")).toBe("ArrowLeft");
});

test("one bindings table: every section filled, overview hints drawn from it", () => {
  for (const s of ["Global", "Session", "Composer"]) expect(BINDINGS.some((b) => b.section === s)).toBe(true);
  const ov = overviewKeys("⌘");
  expect(ov.length).toBeGreaterThan(0);
  expect(ov.every((o) => BINDINGS.some((b) => b.label === o.label))).toBe(true);
  expect(BINDINGS.some((b) => b.keys.includes("?"))).toBe(true);
  expect(BINDINGS.some((b) => b.keys.includes("F6"))).toBe(true);
});

import { focusComposerKey, newSessionKey, switchKey } from "../src/keys";
type Ev = Parameters<typeof switchKey>[0];
const kev = (o: Partial<Ev> & { value?: string }, tag = "DIV"): Ev =>
  ({ key: "", code: "", metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, ...o, target: { tagName: tag, value: o.value ?? "" } as unknown as EventTarget });

test("? in an empty composer opens the sheet; with text it is typed", () => {
  expect(sheetKey(kev({ key: "?", shiftKey: true }, "TEXTAREA"))).toBe(true);
  expect(sheetKey(kev({ key: "?", shiftKey: true, value: "why" }, "TEXTAREA"))).toBe(false);
  expect(sheetKey(kev({ key: "?", shiftKey: true }, "INPUT"))).toBe(false);
});

test("Mod+P switches sessions, from a text field too", () => {
  expect(switchKey(kev({ key: "p", metaKey: true }, "TEXTAREA"))).toBe(true);
  expect(switchKey(kev({ key: "p", ctrlKey: true }))).toBe(true);
  expect(switchKey(kev({ key: "p", metaKey: true, shiftKey: true }))).toBe(false);
  expect(switchKey(kev({ key: "p" }))).toBe(false);
});

test("⌥N starts a session and ⌥I focuses the composer, by physical key", () => {
  expect(newSessionKey(kev({ key: "˜", code: "KeyN", altKey: true }, "TEXTAREA"))).toBe(true);
  expect(newSessionKey(kev({ key: "n", code: "KeyN" }))).toBe(false);
  expect(focusComposerKey(kev({ key: "ˆ", code: "KeyI", altKey: true }))).toBe(true);
  expect(focusComposerKey(kev({ key: "i", code: "KeyI", altKey: true, metaKey: true }))).toBe(false);
});

test("the sheet lists the switcher, new session and composer keys", () => {
  const all = BINDINGS.flatMap((b) => b.keys);
  for (const k of ["ModP", "AltN", "AltI"]) expect(all).toContain(k);
});
