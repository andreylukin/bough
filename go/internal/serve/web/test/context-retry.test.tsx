import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { Pending } from "../src/context";

// A failed Context page keeps its error while a retry is out (refresh()
// clears it only when an answer lands), so the Retry button is the one
// thing that can say the retry is under way. Without it a click showed
// nothing and a second click sent a second read.
test("a retry in flight shows on its button and cannot be sent again", () => {
  const html = renderToStaticMarkup(<Pending title="Context" what="context" err="boom" busy onRetry={() => {}} />);
  expect(html).toContain("Context did not load: boom");
  expect(html).toMatch(/<button[^>]*disabled=""[^>]*aria-busy="true"[^>]*>Retrying…<\/button>/);
});

test("with no retry out the button is a plain Retry", () => {
  const html = renderToStaticMarkup(<Pending title="Context" what="context" err="boom" onRetry={() => {}} />);
  expect(html).toMatch(/<button class="link">Retry<\/button>/);
});
