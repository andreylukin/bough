import { expect, test } from "bun:test";
import { idTail } from "../src/palette";

// Legacy ids ("2026-09-02T11:06:00Z-68755") read "-68755" when cut at six.
test("an id tail never starts with the separator", () => {
  expect(idTail("2026-09-02T11:06:00Z-68755")).toBe("68755");
  expect(idTail("01a09c94-407b-7a73-8c5f-1fcca77d7fa6")).toBe("7d7fa6");
});

// Every surface cuts the tail the same way: the sidebar, Projects and the orb table read "-17758" while the palette read "17758".
test("id tails come from idTail everywhere", () => {
  const { readFileSync, readdirSync } = require("fs");
  const offenders = readdirSync(`${import.meta.dir}/../src`).filter((f: string) => /\.tsx?$/.test(f) && f !== "palette.tsx")
    .filter((f: string) => /\.slice\(-6\)/.test(readFileSync(`${import.meta.dir}/../src/${f}`, "utf8")));
  expect(offenders).toEqual([]);
});
