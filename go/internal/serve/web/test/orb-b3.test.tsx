import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { OrbAddress, OrbPhases } from "../src/mode";
import { ProjectOrb } from "../src/orb";
import type { OrbDetail, OrbState, Project } from "../src/types";

const noop = () => {};
const running = {
  session: "s1", project: "web", status: "running", updatedAt: "2026-09-17T10:00:00Z", container: "bough-orb-s1", ip: "192.168.64.7",
  ports: [{ host: 3000, guest: 3000 }, { host: 8080, guest: 80 }, { host: 5173, guest: 5173, error: "127.0.0.1:5173 is in use on the host" }],
} as OrbState;

test("ORB-B3: the phase popover shows the container IP and each host forward", () => {
  const html = renderToStaticMarkup(<OrbPhases orb={running} now={0} />);
  expect(html).toContain("192.168.64.7");
  expect(html).toContain('href="http://127.0.0.1:3000"');
  expect(html).toContain("127.0.0.1:8080 → 80");
  // A skipped forward says why, instead of vanishing.
  expect(html).toContain("Not forwarded: 127.0.0.1:5173 is in use on the host");
});

test("ORB-B3: a stopped orb shows no address (the IP is gone with the VM)", () => {
  expect(renderToStaticMarkup(<OrbAddress orb={{ ...running, status: "stopped" }} />)).toBe("");
  expect(renderToStaticMarkup(<OrbAddress orb={{ ...running, ip: undefined, ports: undefined }} />)).toBe("");
});

test("ORB-B3: the project's orb list shows a running orb's address", () => {
  const detail = {
    files: { "project.yml": "" }, runtime: { name: "apple", available: true }, orb: { image: "i", built: true },
    build: { tag: "i", state: "ok" }, orbs: [running],
  } as unknown as OrbDetail;
  const html = renderToStaticMarkup(<ProjectOrb project={{ id: "p1", name: "Web", slug: "web" } as Project} detail={detail} log=""
    onAttach={noop} onDetach={noop} onSave={async () => {}} onBuild={noop} onStopOrb={noop} />);
  expect(html).toContain("192.168.64.7");
  expect(html).toContain("127.0.0.1:3000");
});

test("ORB-B3: address styles live in the ORB-B3 block of dist and design css", async () => {
  const { readFileSync } = await import("node:fs");
  for (const f of ["../dist/index.html", "../design/bough.css"]) {
    const css = readFileSync(new URL(f, import.meta.url), "utf8");
    expect(css).toMatch(/\/\* ORB-B3 \*\/[\s\S]*\.orb-addr\{[\s\S]*\/\* \/ORB-B3 \*\//);
  }
});
