import { expect, test } from "bun:test";
import { readFileSync } from "fs";
import { join } from "path";
import { renderToStaticMarkup } from "react-dom/server";
import { ProjectOrb } from "../src/orb";
import type { OrbDetail, Project } from "../src/types";

const noop = () => {};

test("ORB-B4: the orb panel shows one preflight line per check, failures with their reason", () => {
  const detail = {
    files: { "project.yml": "" }, runtime: { name: "apple", available: true },
    orb: { image: "i", built: true }, build: { tag: "i", state: "ok" }, orbs: [],
    preflight: [
      { kind: "runtime", name: "apple", status: "ok" },
      { kind: "clone", name: "webshop", status: "warn", detail: "remote unreachable, the cached clone is used: x" },
      { kind: "gh", name: "GitHub token", status: "ok" },
      { kind: "secret", name: "DEVPI_URL", status: "fail", detail: "keychain:bough/w/DEVPI_URL not found in the keychain" },
    ],
  } as unknown as OrbDetail;
  const html = renderToStaticMarkup(
    <ProjectOrb project={{ id: "p1", name: "Web", slug: "web" } as Project} detail={detail} log=""
                onAttach={noop} onDetach={noop} onSave={async () => {}} onBuild={noop} onStopOrb={noop} />,
  );
  expect(html).toContain("<dt>Preflight</dt>");
  expect(html).toContain('class="orb-preflight"');
  expect(html.match(/<li/g)?.length).toBeGreaterThanOrEqual(4);
  expect(html).toContain("Runtime <span class=\"mono\">apple</span>");
  expect(html).toContain("Clone <span class=\"mono\">webshop</span>");
  expect(html).toContain("GitHub token");
  expect(html).toContain("Secret <span class=\"mono\">DEVPI_URL</span>");
  expect(html).toContain("not found in the keychain");
  expect(html).toContain("1 failing");
  expect(html).not.toContain("<dt>Runtime</dt>");
  for (const f of ["../dist/index.html", "../design/bough.css"]) {
    const css = readFileSync(join(import.meta.dir, f), "utf8");
    expect(css).toMatch(/\/\* ORB-B4 \*\/[\s\S]*\.orb-preflight\{[\s\S]*\/\* \/ORB-B4 \*\//);
  }
});

test("ORB-B4: without a preflight the runtime row still shows", () => {
  const detail = {
    files: {}, runtime: { name: "apple", available: false, error: "start it" },
    orb: { image: "i", built: true }, build: { tag: "i", state: "ok" }, orbs: [],
  } as unknown as OrbDetail;
  const html = renderToStaticMarkup(
    <ProjectOrb project={{ id: "p1", name: "Web", slug: "web" } as Project} detail={detail} log=""
                onAttach={noop} onDetach={noop} onSave={async () => {}} onBuild={noop} onStopOrb={noop} />,
  );
  expect(html).toContain("<dt>Runtime</dt>");
  expect(html).toContain("Unavailable");
});
