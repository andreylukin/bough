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
  expect(sheetKey(ev({ key: "?", shiftKey: true }, "TEXTAREA"))).toBe(false);
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
