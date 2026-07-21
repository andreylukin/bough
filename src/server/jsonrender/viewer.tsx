/**
 * Browser entry for the artifact spec viewer. bundle.ts compiles this file (plus
 * catalog.ts / registry.tsx) into a single ESM bundle served at /artifact-viewer.js;
 * the wrapper page (also bundle.ts) inlines the spec as JSON and loads it. The spec
 * was validated against the catalog at publish time, so rendering is best-effort
 * trusting: a malformed element just renders nothing.
 */
/// <reference lib="dom" />
import { createRoot } from "react-dom/client";
import { JSONUIProvider, Renderer } from "@json-render/react";
import type { Spec } from "@json-render/core";
import { registry } from "./registry.tsx";

const specEl = document.getElementById("__ui_spec__");
const rootEl = document.getElementById("root");
if (specEl?.textContent && rootEl) {
  const spec = JSON.parse(specEl.textContent) as Spec;
  createRoot(rootEl).render(
    <JSONUIProvider registry={registry}>
      <Renderer spec={spec} registry={registry} />
    </JSONUIProvider>,
  );
}
