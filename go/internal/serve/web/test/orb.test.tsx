import { afterEach, expect, test } from "bun:test";
import { readFileSync } from "fs";
import { join } from "path";
import { renderToStaticMarkup } from "react-dom/server";
import { api } from "../src/api";
import { ModeChip, ModePicker } from "../src/mode";
import { ProjectOrb } from "../src/orb";
import type { OrbDetail, Project, Row } from "../src/types";

const noop = () => {};
const row = (over: Partial<Row>) => ({ id: "s1", ...over }) as Row;

test("a project row shows its project and real orb state", () => {
  // A failed setup is its own labelled indicator under the project's display name, not a session status.
  const failed = renderToStaticMarkup(<ModeChip name="Web app" row={row({ mode: "project", orb: { project: "p1", status: "failed" } as Row["orb"] })} />);
  expect(failed).toContain("Setup failed");
  expect(failed).toContain("Web app: setup failed");
  expect(failed).not.toContain("p1");
  expect(failed).not.toContain("mode-running");
  const running = renderToStaticMarkup(<ModeChip row={row({ mode: "project", orb: { project: "web", status: "running" } as Row["orb"] })} />);
  expect(running).toContain("mode-running");
  expect(running).toContain("web · Running");
});

test("a local row shows no chip", () => {
  expect(renderToStaticMarkup(<ModeChip row={row({ mode: "local" })} />)).toBe("");
});

test("the picker offers Local and only projects that carry an orb", () => {
  const projects = [{ id: "p1", name: "Web", slug: "web" }, { id: "p2", name: "Label only" }] as Project[];
  const html = renderToStaticMarkup(<ModePicker projects={projects} value={{ mode: "local" }} onChange={noop} />);
  expect(html).toContain("Local");
  expect(html).toContain('aria-pressed="true"');
  const none = renderToStaticMarkup(<ModePicker projects={[projects[1]]} value={{ mode: "local" }} onChange={noop} />);
  expect(none).toContain("No project has an orb yet");
});

const origFetch = globalThis.fetch;
afterEach(() => { globalThis.fetch = origFetch; });

test("creating a session sends mode and project", async () => {
  let body = "";
  globalThis.fetch = (async (_: string, init?: RequestInit) => {
    body = String(init?.body);
    return new Response(JSON.stringify({ session: { id: "x" } }), { status: 200 });
  }) as typeof fetch;
  await api.create("/w", "hi", "project", "p1");
  expect(JSON.parse(body)).toMatchObject({ mode: "project", project: "p1" });
});

test("the project page shows the editor, the build failure and the log", () => {
  const detail = {
    files: { "project.yml": "repos: []\n" },
    runtime: { name: "apple", available: true },
    orb: { image: "bough-orb/web:abc", built: false },
    build: { tag: "bough-orb/web:abc", state: "failed", error: "clone failed" },
    orbs: [{ session: "2026-09-13-abcdef", status: "failed", error: "boom" }],
  } as unknown as OrbDetail;
  const html = renderToStaticMarkup(
    <ProjectOrb project={{ id: "p1", name: "Web", slug: "web" } as Project} detail={detail} log="step 1"
                onAttach={noop} onDetach={noop} onSave={async () => {}} onBuild={noop} onStopOrb={noop} />,
  );
  expect(html).toContain('aria-label="project.yml"');
  expect(html).toContain("repos: []");
  expect(html).toContain("Build failed");
  expect(html).toContain(" · clone failed");
  expect(html).toContain("step 1");
  expect(html).toContain("abcdef");
  expect(html).not.toContain("Stop orb");
});

test("no phone rule hides the mode chip", () => {
  const css = readFileSync(join(import.meta.dir, "../dist/index.html"), "utf8");
  for (const m of css.matchAll(/@media[^{]*max-width:\s*7\d\dpx[^{]*\{((?:[^{}]*\{[^}]*\})*)[^}]*\}/g)) {
    expect(m[1]).not.toMatch(/mode-chip[^{]*\{[^}]*display:\s*none/);
  }
});
