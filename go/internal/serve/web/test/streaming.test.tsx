import { expect, mock, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
// No DOM under bun: markdown sanitising is not what these assert.
mock.module("dompurify", () => ({ default: { sanitize: (s: string) => s } }));
const { Entry } = await import("../src/app");
const { holdPartial, highlightFences, Markdown } = await import("../src/render");

test("a streaming reply holds back an unclosed inline backtick", () => {
  expect(holdPartial("It exports `sub(a")).toBe("It exports ");
  expect(holdPartial("It exports `sub(a, b)`")).toBe("It exports `sub(a, b)`");
  expect(renderToStaticMarkup(<Markdown text="It exports `sub(a" live />)).not.toContain("`");
});

test("a streaming reply holds back an unclosed link bracket", () => {
  expect(holdPartial("See [the docs")).toBe("See ");
  expect(holdPartial("See [the docs](http://x)")).toBe("See [the docs](http://x)");
});

test("an open fence mid-stream is left alone", () => {
  const t = "Run:\n\n```ts\nconst a = `x";
  expect(holdPartial(t)).toBe(t);
});

test("finished fences are highlighted", () => {
  const html = highlightFences('<pre><code class="language-ts">const a = &quot;x&quot; &lt; 1\n</code></pre>');
  expect(html).toContain("hljs-keyword");
  expect(html).toContain('class="language-ts hljs"');
  expect(renderToStaticMarkup(<Markdown text={"```ts\nconst a = 1\n```"} />)).toContain("hljs-keyword");
});

test("an unknown fence language stays plain", () => {
  const html = '<pre><code class="language-cobol">MOVE A</code></pre>';
  expect(highlightFences(html)).toBe(html);
});

test("a reply that was only guessed output is a quiet note, prose around it stays", () => {
  const only = renderToStaticMarkup(<Entry line={{ seq: 1, at: "", kind: "assistant", text: "[guessed output omitted]" }} codes={[]} />);
  expect(only).toContain("exec-note-quiet");
  expect(only).not.toContain("[guessed output omitted]");
  const mixed = renderToStaticMarkup(<Entry line={{ seq: 1, at: "", kind: "assistant", text: "It printed:\n[guessed output omitted]\nThat is all." }} codes={[]} />);
  expect(mixed).toContain("That is all.");
});
