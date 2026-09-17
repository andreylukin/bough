import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ModePicker } from "../src/mode";
import { OrbFailureBody, failedBuild } from "../src/orb";
import type { Project } from "../src/types";

const noop = () => {};

test("a build failure shows the error lines, links to Projects → Orb and offers only Rebuild", () => {
  const html = renderToStaticMarkup(
    <OrbFailureBody projectId="p1" name="Web" onRebuild={noop} onRetry={noop}
      log={{ phase: "build", log: "build.log", error: "image: exit status 1\n  E: Unable to locate package nope\nfull log: /h/build.log", text: "step\nE: Unable to locate package nope\n" }} />,
  );
  expect(html).toContain("Image build failed");
  expect(html).toContain("E: Unable to locate package nope");
  expect(html).toContain('href="#/projects/p1/orb"');
  expect(html).toContain("Rebuild image");
  expect(html).not.toContain("Retry");
  expect(html).not.toContain("Edit resume.sh");
  expect(html).not.toContain("bough project");
  expect(html).not.toContain("resume.log is empty");
});

test("a setup failure offers Edit resume.sh and Retry, not Rebuild", () => {
  const html = renderToStaticMarkup(
    <OrbFailureBody projectId="p1" name="Web" onRebuild={noop} onRetry={noop}
      log={{ phase: "setup", log: "resume.log", error: "resume.sh: exit status 3\n  npm ERR! missing script: dev", text: "npm ERR! missing script: dev\n" }} />,
  );
  expect(html).toContain("Setup failed");
  expect(html).toContain("Edit resume.sh");
  expect(html).toContain("Retry");
  expect(html).not.toContain("Rebuild");
});

test("a start failure offers no rebuild or retry", () => {
  const html = renderToStaticMarkup(
    <OrbFailureBody projectId="p1" name="Web" onRebuild={noop} onRetry={noop}
      log={{ phase: "start", log: "resume.log", error: "worktree app: no such directory", text: "" }} />,
  );
  expect(html).toContain("Orb failed to start");
  expect(html).toContain('href="#/projects/p1/orb"');
  expect(html).not.toContain("Rebuild");
  expect(html).not.toContain("Retry");
});

test("the picker warns about a project whose last image build failed", () => {
  const projects = [{ id: "p1", name: "Web", slug: "web", orb: { slug: "web", image: "", built: false, build: "failed" } }] as Project[];
  expect(failedBuild(projects[0])).toBe(true);
  const html = renderToStaticMarkup(<ModePicker projects={projects} value={{ mode: "project", project: "p1" }} onChange={noop} />);
  expect(html).toContain("Last image build failed");
});
