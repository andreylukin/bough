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
  expect(mixed).not.toContain("[guessed output omitted]");
  expect(mixed).toContain("A code block with guessed output was removed");
  const table = renderToStaticMarkup(<Entry line={{ seq: 1, at: "", kind: "assistant", text: "| a | b |\n|---|---|\n| 1 | 2 |\n\n[guessed output omitted]" }} codes={[]} />);
  expect(table).toContain("<table");
  expect(table).not.toContain("[guessed output omitted]");
  expect(table).toContain("exec-note-quiet");
  // A sentinel line quoted inside a fenced block is code, not a removal: the fence must stay whole.
  const fenced = renderToStaticMarkup(<Entry line={{ seq: 1, at: "", kind: "assistant", text: "The loop writes:\n\n```go\nx := 1\n[guessed output omitted]\ny := 2\n```" }} codes={[]} />);
  expect(fenced).not.toContain("exec-note-quiet");
  expect(fenced.match(/<pre/g)?.length).toBe(1);
});

test("R2-E: the running and finished prompt rows share one structure, so nothing shifts when the turn ends", async () => {
  const { Thread } = await import("../src/app");
  const at = "2026-09-16T10:00:00Z";
  const row = { id: "s1", cwd: "/tmp/x", status: "running" } as never;
  const props = { onSend: async () => null, onAnswer: async () => null, onInterrupt: () => {}, onArchive: () => {}, onRename: async () => {},
    onModel: () => {}, onEffort: () => {}, onAssign: () => {}, onBack: () => {}, onContext: () => {}, onAck: () => {}, projects: [], busy: false, jump: null } as const;
  const running = renderToStaticMarkup(<Thread {...props} row={row} lines={[]} sending={[{ id: "p1", text: "write an essay", after: 0, at }]} stream={[{ kind: "text", text: "Bonsai is" }] as never} />);
  const done = renderToStaticMarkup(<Thread {...props} row={{ ...(row as object), status: "idle" } as never} lines={[
    { seq: 1, at, kind: "input", text: "write an essay" }, { seq: 2, at, kind: "assistant", text: "Bonsai is" }, { seq: 3, at, kind: "done", text: "" }] as never} />);
  const shape = (html: string) => {
    const t = html.slice(html.indexOf('aria-label="Transcript"'));
    const sections = t.match(/<(section|div) class="turn[" ]/g)?.length ?? 0;
    const prompt = t.match(/<div class="prompt">.*?<\/div><\/div>/)?.[0] ?? "";
    return { sections, time: /class="num prompt-time"[^>]*>[^<]+</.test(t), mark: prompt.includes("prompt-mark"), acts: t.includes("msg-acts prompt-acts") };
  };
  expect(shape(running)).toEqual({ sections: 1, time: true, mark: false, acts: true });
  expect(shape(running)).toEqual(shape(done));
});

test("a prompt sent right after a /model switch streams under its own prompt, not the command's", async () => {
  const { Thread } = await import("../src/app");
  const at = "2026-09-16T10:00:00Z";
  const props = { onSend: async () => null, onAnswer: async () => null, onInterrupt: () => {}, onArchive: () => {}, onRename: async () => {},
    onModel: () => {}, onEffort: () => {}, onAssign: () => {}, onBack: () => {}, onContext: () => {}, onAck: () => {}, projects: [], busy: false, jump: null } as const;
  const html = renderToStaticMarkup(<Thread {...props} row={{ id: "s1", cwd: "/tmp/x", status: "running" } as never} lines={[
    { seq: 1, at, kind: "input", text: "hi" }, { seq: 2, at, kind: "assistant", text: "Hello" }, { seq: 3, at, kind: "done", text: "" },
    { seq: 4, at, kind: "command", text: "/model llm-openai gpt-5.4-mini" }, { seq: 5, at, kind: "system", text: "model: llm-openai · gpt-5.4-mini" }] as never}
    sending={[{ id: "p1", text: "write an essay", after: 5, at }]} stream={[{ kind: "text", text: "Bonsai is" }] as never} />);
  expect(html.indexOf("Bonsai is")).toBeGreaterThan(html.indexOf("write an essay"));
});

// R4-D: a tool block whose result landed is done, even while the activity label lags.
test("a segment with a result renders no running label", async () => {
  const { TurnView } = await import("../src/app");
  const { groupTurns } = await import("../src/render");
  const c = 'tools.bash("ls -la")', d = 'tools.bash("pwd")';
  const at = (s: number) => new Date(Date.parse("2026-01-02T10:00:00Z") + s * 1000).toISOString();
  const lines = [
    { seq: 1, at: at(0), kind: "input", text: "list" },
    { seq: 2, at: at(1), kind: "code", text: c },
    { seq: 3, at: at(2), kind: "result", text: "ok", data: { code: c, exit: 0 } },
    { seq: 4, at: at(3), kind: "code", text: d },
    { seq: 5, at: at(4), kind: "result", text: "ok", data: { code: d, exit: 0 } },
  ];
  const html = renderToStaticMarkup(<TurnView turn={groupTurns(lines as never)[0]} working="Running pwd" />);
  expect(html).not.toContain("Running pwd");
  expect(html).not.toMatch(/>Running /);
});
