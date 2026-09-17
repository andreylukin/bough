import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ProjectsView } from "../src/projects";

test("an empty Projects page offers New project once, in the empty state", () => {
  const noop = () => {};
  const html = renderToStaticMarkup(
    <ProjectsView projects={[]} rows={[]} onOpen={noop} onAssign={noop} onAssignMany={async () => []}
                  onCreate={async () => ({ id: "p" })} onRename={async () => {}} onDelete={noop} />,
  );
  expect(html.split("New project…").length - 1).toBe(1);
});
