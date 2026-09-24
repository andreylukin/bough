// What the orb panel owes the image build flow
// (go/tests/model/specs/orb-image-build.fizz); the browser walk of that
// spec found both.
import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ProjectOrb, orbVerdict } from "../src/orb";
import type { OrbDetail, Project } from "../src/types";

const noop = () => {};
const detail = (over: Partial<Record<"orb" | "build", object>>) => ({
  files: { "project.yml": "repos: []\n" }, runtime: { name: "fake", available: true }, orbs: [],
  ...over,
}) as unknown as OrbDetail;
const render = (d: OrbDetail) => renderToStaticMarkup(
  <ProjectOrb project={{ name: "Web", slug: "web" } as Project} detail={d} log="" onSave={async () => {}}
              onBuild={noop} onStopOrb={noop} onRetry={noop} />);

test("a build over an image this definition already has says the orb is ready", () => {
  const busy = { build: { state: "building", tag: "bough-orb/web:abc" } };
  expect(orbVerdict(detail({ ...busy, orb: { image: "bough-orb/web:abc", built: true } })).word).toBe("Ready · rebuilding image");
  expect(orbVerdict(detail({ ...busy, orb: { image: "bough-orb/web:abc", built: false } })).word).toBe("Building image");
});

test("a definition that does not load cannot be built from the panel", () => {
  // serve answers 400 to that build; the button used to send it anyway,
  // and the refusal went nowhere the person could see.
  const html = render(detail({ orb: { image: "", built: false, error: "project.yml: yaml: line 2: did not find expected node content" }, build: {} }));
  expect(html).toMatch(/<button class="btn btn-primary" disabled="" title="Fix the definition first"[^>]*>Build image<\/button>/);
  expect(render(detail({ orb: { image: "bough-orb/web:abc", built: false }, build: {} })))
    .toMatch(/<button class="btn btn-primary">Build image<\/button>/);
});
