import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ContextView, type ContextData, type ContextSkill } from "../src/context";

for (const [source, label] of [["builtin", "Built-in"], ["plugin", "Plugin"], ["pool", "Pool"]] as const) {
  test(`context skill source ${source} has its own truthful label`, () => {
    const skill: ContextSkill = { id: "context-toolkit", name: "context-toolkit", summary: "Manage working notes", source, off: false };
    const html = renderToStaticMarkup(
      <ContextView data={{ cwd: "/w", rules: [], contextFiles: [], skills: [skill] } as ContextData}
                   load={async () => ""} save={async () => {}} setOff={async () => {}} />,
    );
    expect(html).toContain(`class="hk2-tag">${label}</span>`);
    expect(html).toContain("/context-toolkit");
    expect(html).toContain("Disable globally");
  });
}
