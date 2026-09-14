import type { Meta, StoryObj } from "@storybook/react-vite";
import { FireInspection, HooksView, type Fire, type Hook } from "../hooks";
import { TurnHooks } from "../app";

const path = "/w/demo/.bough/hooks/guard.js";
const homePath = "/Users/a/.bough/hooks/guard.js";
const body = 'export default { event: "pre-code-exec", run(ctx) { return null; } };\n';
const load = () => Promise.resolve(body);
const save = () => Promise.resolve();
const base: Fire = {
  at: "2026-06-10T12:00:00Z", session: "inspection", event: "pre-code-exec", name: "guard.js",
  ms: 2, decision: "", error: "", notice: "", truncated: [], path,
};
const captured: Fire = { ...base, description: "Rewrite the greeting before code execution.", input: { code: 'console.log("hello")', context: { cwd: "/w/demo", files: ["src/main.ts"] } },
  output: { code: 'console.log("hello, world")' }, decision: "rewrote" };
const quietFires: Fire[] = [
  { ...base, at: "2026-06-10T12:03:00Z", input: { code: "1 + 1" }, output: null },
  { ...base, at: "2026-06-10T12:02:00Z", input: { code: "2 + 2" }, output: null },
  { ...base, at: "2026-06-10T12:01:00Z", input: { code: "3 + 3" }, output: {} },
];
const edgeFires: Fire[] = [
  captured,
  { ...base, name: "legacy", path: undefined },
  { ...base, name: "go-hook", path: undefined, input: { result: "ok" }, output: null },
  { ...base, name: "oversize", inputBytes: 90000, inputTruncated: true, outputBytes: 70000, outputTruncated: true },
  { ...base, name: "capture-error", inputError: "cannot serialize input", outputError: "cannot serialize output" },
  { ...base, name: "thrown", input: { code: "throw Error()" }, output: null, error: "hook threw" },
  { ...base, name: "empty-values", input: "", output: false },
  { ...base, name: "long-value", input: { text: "unbroken".repeat(200) }, output: [] },
];
const installed: Hook[] = [
  { id: "guard-project", description: "Check generated code before execution.", name: "guard.js", event: base.event, path, scope: "project", off: false,
    shadowed: false, lastFired: base.at, lastDecision: "rewrote", failing: false, error: "" },
  { id: "guard-home", name: "guard.js", event: base.event, path: homePath, scope: "home", off: false,
    shadowed: true, lastFired: null, lastDecision: "", failing: false, error: "" },
];
const data = { hooks: installed, fires: [...quietFires, ...edgeFires], watchers: [], rules: [], plugins: [] };
const meta: Meta<typeof FireInspection> = {
  title: "Hooks/Inspection",
  component: FireInspection,
  args: { fire: captured, load, save },
  parameters: { layout: "padded" },
};
export default meta;
type S = StoryObj<typeof FireInspection>;

export const Captured: S = {};
export const NoOutput: S = { args: { fire: quietFires[0] } };
export const Legacy: S = { args: { fire: edgeFires[1] } };
export const GoHook: S = { args: { fire: edgeFires[2] } };
export const Oversize: S = { args: { fire: edgeFires[3] } };
export const CaptureErrors: S = { args: { fire: edgeFires[4] } };
export const Thrown: S = { args: { fire: edgeFires[5] } };
export const EmptyValues: S = { args: { fire: edgeFires[6] } };
export const LongValuePhone: S = { args: { fire: edgeFires[7] }, globals: { viewport: { value: "mobile1" } } };
export const MissingDefinition: S = { args: { load: () => Promise.reject(new Error("File no longer exists")) } };
export const SaveFailure: S = { args: { save: () => Promise.reject(new Error("File is read-only")) } };
export const DuplicateDefinitions: S = {
  render: () => <><FireInspection fire={captured} load={load} save={save} /><FireInspection fire={quietFires[0]} load={load} save={save} /></>,
};
export const GroupedAndInstalled: S = {
  render: () => <div className="app" style={{ height: "100vh" }}>
    <HooksView data={data} load={load} save={save}
      dryrun={() => Promise.resolve({ result: null, error: "", ms: 1 })} setOff={() => Promise.resolve()} />
  </div>,
};
export const GroupedPhone: S = { ...GroupedAndInstalled, globals: { viewport: { value: "mobile1" } } };
export const InTurn: S = {
  render: () => <TurnHooks load={load} save={save} lines={[...quietFires, ...edgeFires].map((fire, seq) => ({
    seq, at: fire.at, kind: "hook", text: "", data: { ...fire },
  }))} />,
};
