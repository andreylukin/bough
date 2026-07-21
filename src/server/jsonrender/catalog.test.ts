import { assert, assertEquals, assertThrows } from "jsr:@std/assert@1";
import { catalog, SPEC_GUIDE, validateUiSpec } from "./catalog.ts";

const good = JSON.stringify({
  root: "page",
  elements: {
    page: { type: "Page", props: { title: "T" }, children: ["s"] },
    s: { type: "Stat", props: { label: "solved", value: "14/16" }, children: [] },
  },
});

Deno.test("validateUiSpec accepts a catalog-conformant spec", () => {
  const spec = validateUiSpec(good) as { root: string };
  assertEquals(spec.root, "page");
});

Deno.test("validateUiSpec rejects non-JSON", () => {
  assertThrows(() => validateUiSpec("not json"), Error, "not valid JSON");
});

Deno.test("validateUiSpec aggregates every issue in one agent-repairable error", () => {
  const bad = JSON.stringify({
    root: "x",
    elements: { x: { type: "Nope", props: { junk: 1 }, children: ["ghost"] } },
  });
  const err = assertThrows(() => validateUiSpec(bad), Error) as Error;
  assert(err.message.includes("publish again"), "tells the agent to retry");
  assert(err.message.includes("ghost"), "dangling child reported");
  assert(
    err.message.includes("Nope") || err.message.includes("elements.x.type"),
    "bad type reported",
  );
});

Deno.test("validateUiSpec auto-fixes visible-inside-props before validating", () => {
  const fixable = JSON.stringify({
    root: "t",
    elements: {
      t: { type: "Text", props: { text: "hi", visible: { "$state": "/x" } }, children: [] },
    },
  });
  const spec = validateUiSpec(fixable) as {
    elements: Record<string, { props: Record<string, unknown>; visible?: unknown }>;
  };
  assertEquals(spec.elements.t.props.visible, undefined);
  assert(spec.elements.t.visible !== undefined);
});

Deno.test("SPEC_GUIDE names every catalog component (drift guard)", () => {
  for (const name of catalog.componentNames) {
    assert(SPEC_GUIDE.includes(name + "{"), `SPEC_GUIDE is missing ${name}`);
  }
});
