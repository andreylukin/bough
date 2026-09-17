import { expect, mock, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
// No DOM under bun: markdown sanitising is not what this asserts.
mock.module("dompurify", () => ({ default: { sanitize: (s: string) => s } }));
const { Markdown } = await import("../src/render");

test("a kept output fence renders its lines as pre, not one paragraph", () => {
  const html = renderToStaticMarkup(<Markdown text={"It printed:\n```text\nline one\nline two\n```"} />);
  expect(html).toContain("<pre>");
  expect(html).toMatch(/line one\nline two/);
});
