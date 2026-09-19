import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ProjectsView } from "../src/projects";

test("an empty Projects page offers New project once, in the empty state", () => {
  const noop = () => {};
  const html = renderToStaticMarkup(
    <ProjectsView projects={[]} rows={[]} onOpen={noop} onAssign={noop} onAssignMany={async () => []}
                  onCreate={async () => ({ slug: "p" })} onRename={async () => {}} onDelete={noop} />,
  );
  expect(html.split("New project…").length - 1).toBe(1);
});

// MB-PAGES: with no sessions there is nothing to filter, and the empty state is one compact row.
test("an empty Projects page hides the filter and keeps the empty state to one row", () => {
  const noop = () => {};
  const html = renderToStaticMarkup(
    <ProjectsView projects={[]} rows={[]} onOpen={noop} onAssign={noop} onAssignMany={async () => []}
                  onCreate={async () => ({ slug: "p" })} onRename={async () => {}} onDelete={noop} />,
  );
  expect(html).not.toContain("Filter sessions");
  expect(html).toContain("proj-empty-row");
  expect(html).not.toContain("empty-state");
});
