import { expect, test } from "bun:test";
import { feedbackURL, pastedScreenshot, screenshotError } from "../src/feedback";

const png = (size = 1) => new File([new Uint8Array(size)], "shot.png", { type: "image/png" });
const data = (files: File[], items: Partial<DataTransferItem>[] = []) => ({ files, items }) as unknown as DataTransfer;

test("clipboard screenshot uses the file list once, even when items also expose it", () => {
  const first = png(), second = png();
  expect(pastedScreenshot(data([first, second], [{ kind: "file", type: "image/png", getAsFile: () => first }]))).toBe(first);
});

test("clipboard items provide a fallback when there is no file list entry", () => {
  const file = png();
  expect(pastedScreenshot(data([], [{ kind: "file", type: "image/png", getAsFile: () => file }]))).toBe(file);
  expect(pastedScreenshot(data([], [{ kind: "file", type: "image/png", getAsFile: () => null }]))).toBeNull();
});

test("text and non-image files are left to the normal paste behavior", () => {
  expect(pastedScreenshot(data([], [{ kind: "string", type: "text/plain" }]))).toBeNull();
  expect(pastedScreenshot(data([new File(["notes"], "notes.txt", { type: "text/plain" })]))).toBeNull();
});

test("unsupported images are still recognized so paste can explain the PNG requirement", () => {
  const jpeg = new File(["jpeg"], "shot.jpg", { type: "image/jpeg" });
  expect(pastedScreenshot(data([jpeg]))).toBe(jpeg);
  expect(screenshotError(jpeg)).toBe("Use a PNG screenshot.");
});

test("both paste and file selection reject empty or oversized screenshots", () => {
  expect(screenshotError(png(0))).toContain("empty");
  expect(screenshotError(png())).toBe("");
  expect(screenshotError(png(10 * 1024 * 1024))).toBe("");
  expect(screenshotError(png(10 * 1024 * 1024 + 1))).toContain("10 MB");
});

test("the GitHub handoff encodes only reviewed text and no local screenshot URL", () => {
  const url = new URL(feedbackURL("  Screen & paste?  ", "  First line\nSecond line & details  "));
  expect(url.origin + url.pathname).toBe("https://github.com/andreylukin/bough/issues/new");
  expect(url.searchParams.get("title")).toBe("Screen & paste?");
  expect(url.searchParams.get("body")).toBe("## What happened\nFirst line\nSecond line & details\n\n## Environment\nbough web");
  expect([...url.searchParams.keys()]).toEqual(["title", "body"]);
});
