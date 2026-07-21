/**
 * The bough artifact UI catalog — the component vocabulary an agent may use in a
 * `*.ui.json` artifact spec (vercel-labs/json-render). The catalog is the guardrail:
 * publishArtifact validates every spec against it and rejects anything outside it,
 * so a spec artifact can never be malformed or off-catalog by the time it is served.
 *
 * Shared by the server (validation at publish time) and the browser viewer bundle
 * (src/server/jsonrender/registry.tsx maps these definitions to React components).
 * SPEC_GUIDE is the compact catalog reference the supervisor prompt embeds — the
 * full catalog.prompt() is ~14KB and patch-stream oriented, wrong for bough's
 * one-shot artifact() flow. A test pins SPEC_GUIDE to the catalog so they can't drift.
 */
import {
  autoFixSpec,
  defineCatalog,
  formatSpecIssues,
  type Spec,
  validateSpec,
} from "@json-render/core";
import { schema } from "@json-render/react/schema";
import { z } from "zod4";

const intent = z.enum(["info", "success", "warn", "error"]);

export const catalog = defineCatalog(schema, {
  components: {
    Page: {
      props: z.object({ title: z.string(), subtitle: z.string().optional() }),
      slots: ["default"],
      description: "Page shell with a header; use as the root element.",
    },
    Section: {
      props: z.object({ title: z.string().optional(), hint: z.string().optional() }),
      slots: ["default"],
      description: "Titled grouping of content; hint renders muted under the title.",
    },
    Columns: {
      props: z.object({ count: z.number().int().min(2).max(4).optional() }),
      slots: ["default"],
      description: "Lay children out in equal columns (stacks on narrow screens).",
    },
    Stat: {
      props: z.object({
        label: z.string(),
        value: z.string(),
        delta: z.string().optional(),
        intent: z.enum(["good", "bad", "neutral"]).optional(),
      }),
      description: "One headline number with a label; delta is a small colored change note.",
    },
    Text: {
      props: z.object({
        text: z.string(),
        muted: z.boolean().optional(),
        mono: z.boolean().optional(),
      }),
      description: "A paragraph of plain text.",
    },
    Callout: {
      props: z.object({ intent, title: z.string().optional(), text: z.string() }),
      description: "Highlighted note (info/success/warn/error).",
    },
    Badge: {
      props: z.object({ label: z.string(), intent: intent.optional() }),
      description: "Small inline status pill.",
    },
    Table: {
      props: z.object({
        columns: z.array(z.object({
          key: z.string(),
          label: z.string(),
          align: z.enum(["left", "right"]).optional(),
        })),
        rows: z.array(
          z.record(z.string(), z.union([z.string(), z.number(), z.boolean(), z.null()])),
        ),
        caption: z.string().optional(),
      }),
      description: "Data table; click a header to sort. rows are objects keyed by column key.",
    },
    KeyValue: {
      props: z.object({ pairs: z.array(z.object({ key: z.string(), value: z.string() })) }),
      description: "Compact two-column key/value listing.",
    },
    List: {
      props: z.object({ items: z.array(z.string()), ordered: z.boolean().optional() }),
      description: "Bullet or numbered list.",
    },
    Code: {
      props: z.object({
        code: z.string(),
        lang: z.string().optional(),
        title: z.string().optional(),
      }),
      description: "Preformatted code block.",
    },
    BarChart: {
      props: z.object({
        title: z.string().optional(),
        unit: z.string().optional(),
        bars: z.array(z.object({ label: z.string(), value: z.number() })),
      }),
      description:
        "Horizontal bar chart for one measure across categories; values are direct-labeled. " +
        'unit renders as a suffix, except currency symbols ("$", "€", "£", "¥") which prefix.',
    },
    Link: {
      props: z.object({ label: z.string(), href: z.string() }),
      description: "Hyperlink.",
    },
    Divider: {
      props: z.object({}),
      description: "Horizontal rule.",
    },
    Image: {
      props: z.object({ src: z.string(), alt: z.string().optional() }),
      description: "Image; src must be a relative path to a sibling artifact you published.",
    },
  },
  actions: {},
});

/**
 * Compact component reference for the supervisor prompt. Hand-kept next to the
 * definitions above; catalog.test.ts asserts every component name appears here.
 */
export const SPEC_GUIDE = [
  "Page{title,subtitle?}+children · Section{title?,hint?}+children ·",
  "Columns{count?:2-4}+children · Stat{label,value,delta?,intent?:good|bad|neutral} ·",
  "Text{text,muted?,mono?} · Callout{intent:info|success|warn|error,title?,text} ·",
  "Badge{label,intent?} · Table{columns:[{key,label,align?:left|right}],rows:[{<key>:cell}],caption?} ·",
  "KeyValue{pairs:[{key,value}]} · List{items:[str],ordered?} · Code{code,lang?,title?} ·",
  "BarChart{title?,unit?,bars:[{label,value:number}]} · Link{label,href} · Divider{} ·",
  "Image{src:relative-artifact-path,alt?}",
].join("\n");

/**
 * Parse + auto-fix + validate a `*.ui.json` artifact body against the catalog.
 * Returns the normalized (auto-fixed) spec; throws one Error carrying every issue,
 * phrased for the agent to repair and republish.
 */
export function validateUiSpec(content: string): Spec {
  let parsed: unknown;
  try {
    parsed = JSON.parse(content);
  } catch (e) {
    throw new Error(`ui spec is not valid JSON: ${(e as Error).message}`);
  }
  const { spec } = autoFixSpec(parsed as Spec);
  const problems: string[] = [];
  const structural = validateSpec(spec);
  if (!structural.valid) problems.push(formatSpecIssues(structural.issues));
  const result = catalog.validate(spec);
  if (!result.success) {
    for (const issue of result.error?.issues ?? []) {
      problems.push(`${issue.path.join(".")}: ${issue.message}`);
    }
  }
  if (problems.length > 0) {
    throw new Error(
      `ui spec rejected (fix these and publish again under the same name):\n${problems.join("\n")}`,
    );
  }
  return spec;
}
