// MEMORY.md in the project panel: it is named by its filename, counted
// in lines because the warning is line-based, and saved only on purpose.
// The thresholds here are the ones internal/serve/context.go gates the
// control room's context view on, so a file that reads "long" on the
// project page reads "long" there too.
import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { FileEditor, filePlaceholder } from "../src/orb";
import { PROJECT_FILES, lineCount, lineTone } from "../src/project";
import { LONG, TOO_LONG } from "../src/context";

const lines = (n: number) => Array.from({ length: n }, (_, i) => `line ${i + 1}`).join("\n");

const meta = (f: string, text: string) => {
  const n = lineCount(text);
  return (
    <p className="file-meta">
      <span className="mono">{f}</span> · <span className={("num " + lineTone(n)).trim()}>{n} {n === 1 ? "line" : "lines"}</span>
      {n > LONG && <span className={lineTone(n)}> · long — injected every turn</span>}
    </p>
  );
};

const editor = (text: string) =>
  renderToStaticMarkup(
    <FileEditor order={PROJECT_FILES} files={{ "MEMORY.md": text }} tab="MEMORY.md" onTab={() => {}}
                onSave={async () => {}} announce meta={meta}
                note="Prepended to every session in this project. Nothing writes this but you and the agent." />,
  );

test("the pane is headed by the filename and a line count, never by the word memory", () => {
  const html = editor(lines(84));
  expect(html).toContain("MEMORY.md");
  expect(html).toContain("84 lines");
  expect(html).toContain("Prepended to every session in this project. Nothing writes this but you and the agent.");
  expect(html.toLowerCase()).not.toContain(">memory<");
  // One file, one pane: the tabs are the file list.
  for (const f of PROJECT_FILES) expect(html).toContain(`>${f}<`);
});

test("one line is not plural", () => {
  expect(editor("just this")).toContain(">1 line<");
  expect(editor(lines(2))).toContain(">2 lines<");
});

test("the counter turns amber past 200 lines and red past 400", () => {
  expect(lineTone(LONG)).toBe("");
  expect(lineTone(LONG + 1)).toBe("ctx-lines-long");
  expect(lineTone(TOO_LONG)).toBe("ctx-lines-long");
  expect(lineTone(TOO_LONG + 1)).toBe("ctx-lines-max");

  const fine = editor(lines(LONG));
  expect(fine).not.toContain("ctx-lines-long");
  expect(fine).not.toContain("long — injected every turn");

  const long = editor(lines(LONG + 1));
  expect(long).toContain("201 lines");
  expect(long).toContain("ctx-lines-long");
  expect(long).toContain("long — injected every turn");
  expect(long).not.toContain("ctx-lines-max");

  const huge = editor(lines(TOO_LONG + 1));
  expect(huge).toContain("401 lines");
  expect(huge).toContain("ctx-lines-max");
});

test("Save is disabled until the buffer is dirty", () => {
  // Nothing typed yet: the button is there and cannot be pressed. The
  // enabling is a keystroke, which this layer has no DOM for — the
  // Playwright spec (tests/web/specs/project.spec.ts) types and saves.
  expect(editor(lines(3))).toContain('<button class="btn" disabled="">Save</button>');
});

test("an absent MEMORY.md is a placeholder in the pane, and saving creates it", () => {
  const html = editor("");
  expect(html).toContain("Empty. Write what every session in this project should know: what it is, where things live, decisions already made.");
  expect(html).toContain(">0 lines<");
  // An empty MEMORY.md is a file, not a delete: the scripts say the other thing.
  expect(filePlaceholder("MEMORY.md")).not.toContain("removes the file");
  expect(filePlaceholder("setup.sh")).toContain("saving removes the file");
});
