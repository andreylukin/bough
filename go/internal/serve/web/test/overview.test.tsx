import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ControlOverview } from "../src/app";
import type { Row } from "../src/types";

const noop = () => {};
const now = new Date().toISOString();
const row = (o: Partial<Row>): Row => ({ id: "s1", title: "Untitled", status: "idle", lastAt: now, ...o } as Row);
const render = (rows: Row[]) => renderToStaticMarkup(
  <ControlOverview rows={rows} onReveal={noop} onOpenFailure={noop} loadedAt={Date.now()} loadErr={null} onRetry={noop} />,
);

test("an empty session whose orb failed to set up lands in Needs you, as in the sidebar", () => {
  const html = render([row({ empty: true, live: false, mode: "project", orb: { project: "bough", status: "failed" } as Row["orb"] })]);
  expect(html).toContain("Needs you");
  expect(html).toContain("Setup failed");
  expect(html).not.toContain("Nothing needs your attention");
});

test("an empty dead session with a healthy orb stays out, and the empty state keeps its title over the running link", () => {
  const html = render([
    row({ empty: true, live: false }),
    row({ id: "s2", title: "Running one", status: "running" }),
  ]);
  expect(html).not.toContain("Needs you");
  expect(html).toContain("Nothing needs your attention");
  expect(html).toContain("1 session running");
  expect(html).not.toContain("Filter the list");
});
