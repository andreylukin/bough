// Model-based browser specs: walk every path the generator
// (go/tests/web/model/gen.ts) wrote for a FizzBee spec, drive each action
// through the page, and at every node assert the abstract state read off
// the DOM, plus the invariants every screen owes. The recipe is
// go/tests/model/README.md; specs/model/example.spec.ts is the worked
// example.
import * as fs from 'fs';
import * as path from 'path';
import type { Page } from '@playwright/test';
import { test, expect, type Serve } from './serve';

const modelDir = path.resolve(__dirname, '..', '..', 'model');

export interface Step { action: string; state: Record<string, unknown> }

/** The covering paths checked in for specs/<spec>.fizz. */
export function loadPaths(spec: string): Step[][] {
  const file = path.join(modelDir, 'testdata', spec, 'paths.json');
  const out = JSON.parse(fs.readFileSync(file, 'utf8')) as { paths: { trace: Step[] }[] };
  return out.paths.map((p) => p.trace);
}

export interface Flow<C> {
  /** go/tests/model/specs/<spec>.fizz */
  spec: string;
  /** The role instance whose fields are the state, e.g. "Session#0". */
  role: string;
  /** bough.yml for the test's serve (see helpers/control.ts). */
  config?: string;
  /** The Init step: bring the page to the spec's initial state. */
  init(page: Page, serve: Serve): Promise<C>;
  /** One per action of the role, keyed by the bare action name. */
  actions: Record<string, (c: C) => Promise<void>>;
  /** readUiState: the role's abstract state as the page shows it. Every field of the role, read from the DOM only. */
  read(c: C): Promise<Record<string, unknown>>;
  /** The DOM element that carries the state; it must be on screen at every node. */
  status(c: C): ReturnType<Page['locator']>;
  /** Sessions whose transcripts go to $MODEL_TRACE_DIR for the history check. */
  sessions(c: C): string[];
  /** Let anything still held (a blocked turn) go before serve stops. */
  cleanup?(c: C): Promise<void>;
  /**
   * Console errors the flow causes on purpose. Chromium logs every 4xx/5xx
   * answer as "Failed to load resource", so a flow whose spec has a
   * not-found or a failed read (a 404 lookup) names those lines here;
   * anything else logged is still a failure.
   */
  expectedErrors?: RegExp[];
}

/** The role's fields out of a graph state, keyed by bare field name. */
export function roleState(role: string, state: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(state)) if (k.startsWith(role + '.')) out[k.slice(role.length + 1)] = v;
  return out;
}

// Every node, whatever the flow: the page does not scroll sideways, the
// state's carrier is visible, and nothing was logged as an error.
async function invariants(page: Page, carrier: ReturnType<Page['locator']>, errors: string[], where: string): Promise<void> {
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow, `${where}: page scrolls sideways by ${overflow}px`).toBeLessThanOrEqual(0);
  await expect(carrier, `${where}: status not visible`).toBeVisible();
  expect(errors, `${where}: console errors`).toEqual([]);
}

// How far the page's clock moves before each read of the state: longer
// than the list poll (4 s), so every read sees a fresh list.
const POLL_STEP_MS = 5_000;

/** One test per generated path of flow.spec. */
export function modelTests<C>(flow: Flow<C>): void {
  test.describe(`model: ${flow.spec}`, () => {
    if (flow.config) test.use({ serveOpts: { config: flow.config } });

    loadPaths(flow.spec).forEach((trace, i) => {
      const walk = trace.slice(1).map((s) => s.action.slice(flow.role.length + 1)).join(' → ');
      test(`path ${i}: ${walk}`, async ({ serve, page }, info) => {
        const errors: string[] = [];
        const expected = (t: string) => (flow.expectedErrors ?? []).some((re) => re.test(t));
        page.on('console', (m) => { if (m.type() === 'error' && !expected(m.text())) errors.push(m.text()); });
        page.on('pageerror', (e) => errors.push(String(e)));

        // The page's timers run on a clock the walk can move: a row the
        // page is not streaming is only re-read by the list poll (every
        // 12 s while another session is open), and waiting that out in
        // real time made a five-step path take a minute.
        await page.clock.install();
        const c = await flow.init(page, serve);
        try {
          for (const [n, step] of trace.entries()) {
            const name = step.action === 'Init' ? 'Init' : step.action.slice(flow.role.length + 1);
            const where = `step ${n} (${name})`;
            if (n > 0) {
              const act = flow.actions[name];
              if (!act) throw new Error(`${flow.spec}: no action for ${step.action}`);
              await act(c);
            }
            // The page settles on its own time (polls, acks), so the
            // state is polled, each read a few page-seconds after the
            // last; a wrong one fails with the last read.
            await expect.poll(async () => {
              await page.clock.fastForward(POLL_STEP_MS);
              return flow.read(c);
            }, { message: `${where}: state`, timeout: 10_000 }).toEqual(roleState(flow.role, step.state));
            await invariants(page, flow.status(c), errors, where);
          }
        } finally {
          await flow.cleanup?.(c);
          saveTranscripts(serve, flow.spec, flow.sessions(c), info.title);
        }
      });
    });
  });
}

// The transcripts a walk wrote, for TestHistoryTraces in
// go/tests/model/mbt to replay against the spec's graph.
function saveTranscripts(serve: Serve, spec: string, ids: string[], title: string): void {
  const root = process.env.MODEL_TRACE_DIR;
  if (!root) return;
  const dir = path.join(root, spec);
  fs.mkdirSync(dir, { recursive: true });
  const slug = title.replace(/[^A-Za-z0-9]+/g, '-').replace(/^-|-$/g, '');
  for (const id of ids) {
    const src = path.join(serve.home, '.bough', 'history', id + '.jsonl');
    if (fs.existsSync(src)) fs.copyFileSync(src, path.join(dir, `${slug}-${id}.jsonl`));
  }
}
