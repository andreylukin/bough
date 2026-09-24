import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { JobGroupHead, StopWorkButton, WorkButton, WorkContext, WorkDialog } from "../src/work-ui";
import { workCounts, type Worker } from "../src/work";

const noop = () => {};
const w = (id: string, over: Partial<Worker>): Worker => ({
  key: `s:agent:${id}`, kind: "agent", session: "s", id, label: `Agent ${id}`, task: "", life: "finished",
  stepErrors: 0, notRun: 0, outputState: "none", seq: Number(id), live: true, canStop: false, ...over,
});
const review = { isReviewed: () => true, isNew: () => false, markReviewed: noop };
const dialog = (workers: Worker[], stops: Record<string, unknown> = {}) => renderToStaticMarkup(
  <WorkContext.Provider value={{ session: "s", live: true, workers, byKey: new Map(), jobs: new Map(), jobFirst: new Map(), review, stops, requestStop: noop } as never}>
    <WorkDialog workers={workers} sheet={false} childState="ok" onRetryChildren={noop} parent="s" onClose={noop} onView={noop} onOpenAgent={noop} />
  </WorkContext.Provider>,
);

test("MB-WORK: Finished folds while work is live and opens when nothing is", () => {
  const done = Array.from({ length: 12 }, (_, i) => w(String(i + 1), {}));
  const live = dialog([...done, w("99", { life: "running", canStop: true })]);
  expect(live).toMatch(/class="work-group-toggle" aria-expanded="false"/);
  expect(live).not.toContain("Agent 1<");
  const idle = dialog(done);
  expect(idle).toMatch(/class="work-group-toggle" aria-expanded="true"/);
  expect(idle).toContain("Show 2 more");
});

test("MB-WORK: filters show only with two kinds; the empty-agents note only under Agents", () => {
  const one = dialog([w("1", {})]);
  expect(one).not.toContain("work-filters");
  expect(one).not.toContain("No background agents");
  const two = dialog([w("1", {}), w("2", { kind: "job", key: "s:job:2", label: "Job 2" })]);
  expect(two).toContain(">Agents</button>");
  expect(two).toContain('aria-label="Close"');
});

test("MB-WORK: rows keep a time cell and a reserved Stop column", () => {
  const html = dialog([w("1", { life: "running", canStop: true }), w("2", { life: "queued" })]);
  expect(html).toContain("data-stops");
  expect((html.match(/class="work-row-time"/g) ?? []).length).toBe(2);
  expect(html).toContain("—</span>");
});

test("MB-WORK: Stop labels fit the column; aria keeps the full words", () => {
  const job = w("21", { kind: "job", key: "s:job:21", label: "Job 21", life: "running", canStop: true });
  const at = (state: string) => renderToStaticMarkup(
    <WorkContext.Provider value={{ stops: { [job.key]: { state } } } as never}><StopWorkButton w={job} /></WorkContext.Provider>);
  expect(at("stopping")).toContain("Stopping</button>");
  expect(at("timeout")).toContain(">No reply<");
  expect(at("timeout")).toContain('aria-label="Stop requested · status unavailable"');
  expect(at("error")).toContain(">Retry<");
  expect(at("error")).toContain("Try stop again");
});

test("MB-WORK: the Work button carries a glyph, and a job head dots only its failures", () => {
  const running = renderToStaticMarkup(<WorkButton counts={workCounts([w("1", { life: "running" })])} expanded={false} onClick={noop} />);
  expect(running).toContain("spin-mark");
  const failed = renderToStaticMarkup(<WorkButton counts={workCounts([w("1", { life: "failed" })])} narrow expanded={false} onClick={noop} />);
  expect(failed).toContain("work-dot");
  expect(failed).not.toContain("work-summary-2");
  const head = renderToStaticMarkup(<JobGroupHead workers={[w("1", { life: "running" }), w("2", { life: "failed" })]} />);
  expect(head).toContain('<span class="work-dot" aria-hidden="true"></span>1 failed');
});

test("MB-WORK: a queued agent's row has Stop, though it is not live", () => {
  expect(dialog([w("1", { life: "queued", live: false, canStop: true })])).toContain('aria-label="Stop agent"');
});
