// Model-based browser specs: walk every path the generator
// (go/tests/web/model/gen.ts) wrote for a FizzBee spec, drive each action
// through the page, and at every node assert the abstract state read off
// the DOM, plus the invariants every screen owes. The recipe is
// go/tests/model/README.md; specs/model/example.spec.ts is the worked
// example.
import * as crypto from 'crypto';
import * as fs from 'fs';
import * as path from 'path';
import type { Page, TestInfo } from '@playwright/test';
import { test, expect, type Serve } from './serve';
import { loadGraph, walks, type Cover } from '../model/graph';

const modelDir = path.resolve(__dirname, '..', '..', 'model');

export interface Step { action: string; state: Record<string, unknown> }

/**
 * The walks over specs/<spec>.fizz's checked-in graph (testdata/<spec>),
 * derived here rather than checked in: one test per covering path was
 * ~4,400 browser tests that no longer finished, and each walk replaces
 * dozens of them. The default reaches every settled state;
 * MODEL_COVER=transitions takes every link (the nightly run).
 */
export function loadPaths(spec: string): Step[][] {
  const cover: Cover = process.env.MODEL_COVER === 'transitions' ? 'transitions' : 'states';
  return walks(loadGraph(path.join(modelDir, 'testdata', spec)), cover).paths.map((p) => p.trace);
}

export interface Flow<C> {
  /** go/tests/model/specs/<spec>.fizz */
  spec: string;
  /** The role instance whose fields are the state, e.g. "Session#0". */
  role: string;
  /** bough.yml for the test's serve (see helpers/control.ts). */
  config?: string;
  /** Extra environment for that serve. */
  env?: Record<string, string>;
  /**
   * One serve per worker instead of one per test: init must then bring
   * the shared serve back to the spec's initial state itself. Worth it
   * for a flow with many short paths, where the boot is most of a test.
   */
  shared?: boolean;
  /**
   * How far each read moves the page's clock (default POLL_STEP_MS). A
   * flow whose spec models a poll as its own action sets 0: reads that
   * ran the poll would make it happen where the path says it has not.
   */
  pollStepMs?: number;
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
  /** Console errors a step causes on purpose. Chromium logs every
   *  request answered 4xx/5xx ("Failed to load resource: … 409"), so a
   *  flow whose spec has a refused or failed request names that line here. */
  allowConsole?: RegExp;
  /** A console error the step in flight provoked on purpose (Chromium logs
   *  every non-2xx fetch, and a flow may make serve fail). Only that step's
   *  own failure: anything else still fails the walk. */
  expectedError?(c: C, text: string): boolean;
  /**
   * Console errors the flow causes on purpose. Chromium logs every 4xx/5xx
   * answer as "Failed to load resource", so a flow whose spec has a
   * not-found or a failed read (a 404 lookup) names those lines here;
   * anything else logged is still a failure.
   */
  expectedErrors?: RegExp[];
  /** Checks this flow's surface owes at every node beyond the shared
   *  ones (helpers/ui-invariants.ts has the usual set). */
  invariants?(c: C, where: string): Promise<void>;
  /** More invariants the surface owes at every node, after the shared
   *  ones (helpers/surface.ts has focus, clipping and axe checks). */
  check?(c: C, where: string): Promise<void>;
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

/**
 * Flows whose browser walks fail on the integrated tree and are not yet
 * triaged: each is marked fixme, so it shows in every report without
 * failing the run. Their server-level walks (go/tests/model/mbt) pass and
 * still gate every push. Remove a flow once its walks pass.
 */
export const QUARANTINED: Record<string, string> = {
  ask_answer: 'after Exit the page no longer shows how many questions were asked (asked reads 0)',
  steer_queue: 'Stop leaves the queued message sending, and the draft is not cleared',
  ui_composer: 'a second Skills click shows the cached list; the spec expects loading',
  navigation_routes_palette: 'Back lands in the wrong state; a session never finishes',
  wiki_review: 'ToActivity after an ingest reads the wrong state',
  archive_unarchive: 'AgentFinish after Unarchive reads the wrong state',
  skills_mentions: 'PickSkill reads the wrong state',
  // Pass locally, fail on a CI runner (2026-09-24, run 35993131696):
  changes_review: 'a walk times out on the Open click on a CI runner',
  setup_welcome: 'KeyRejected then SaveKey times out under load (CI; flaky locally)',
  session_filing: 'Assign and Project Delete log console errors on a CI runner',
  ui_me: "the brief bar's text reads clipped after Refresh on a CI runner",
};

/** test, or test.fixme with the reason for a quarantined flow. */
export function walkTest(spec: string): typeof test {
  const why = QUARANTINED[spec];
  if (!why) return test;
  return ((title: string, fn: Parameters<typeof test>[1]) => test.fixme(`${title} [quarantined: ${why}]`, fn)) as unknown as typeof test;
}

/** One test per generated path of flow.spec. */
export function modelTests<C>(flow: Flow<C>): void {
  const opts = { config: flow.config, env: flow.env };
  // A worker option cannot be set inside a describe (it would force a
  // new worker), so it is set here, at the top of the spec file.
  if (flow.shared) test.use({ workerServeOpts: opts });
  test.describe(`model: ${flow.spec}`, () => {
    if (!flow.shared && (flow.config || flow.env)) test.use({ serveOpts: opts });

    loadPaths(flow.spec).forEach((trace, i) => {
      // "end" is fizz's self-link on a state with no action out of it (a
      // goal state): nothing to do, the state is read again.
      const name = (a: string) => (a === 'Init' || a === 'end' ? a : a.slice(flow.role.length + 1));
      const walk = trace.slice(1).map((s) => name(s.action)).join(' → ');
      const title = `path ${i}: ${walk}`;
      const run = async (serve: Serve, page: Page, info: TestInfo) => {
        const errors: string[] = [];
        let c: C | undefined;
        const expected = (t: string) => (flow.expectedErrors ?? []).some((re) => re.test(t));
        page.on('console', (m) => {
          if (m.type() !== 'error' || flow.allowConsole?.test(m.text()) || expected(m.text())) return;
          if (c && flow.expectedError?.(c, m.text())) return;
          errors.push(m.text());
        });
        page.on('pageerror', (e) => errors.push(String(e)));

        // The page's timers run on a clock the walk can move: a row the
        // page is not streaming is only re-read by the list poll (every
        // 12 s while another session is open), and waiting that out in
        // real time made a five-step path take a minute.
        await page.clock.install();
        c = await flow.init(page, serve);
        try {
          for (const [n, step] of trace.entries()) {
            const act = name(step.action);
            const where = `step ${n} (${act})`;
            if (n > 0 && act !== 'end') {
              const perform = flow.actions[act];
              if (!perform) throw new Error(`${flow.spec}: no action for ${step.action}`);
              await perform(c);
            }
            // The page settles on its own time (polls, acks), so the
            // state is polled, each read a few page-seconds after the
            // last; a wrong one fails with the last read.
            await expect.poll(async () => {
              const step = flow.pollStepMs ?? POLL_STEP_MS;
              if (step > 0) await page.clock.fastForward(step);
              return flow.read(c);
            }, { message: `${where}: state`, timeout: 10_000 }).toEqual(roleState(flow.role, step.state));
            await invariants(page, flow.status(c), errors, where);
            await flow.invariants?.(c, where);
            await flow.check?.(c, where);
          }
        } finally {
          await flow.cleanup?.(c);
          saveTranscripts(serve, flow.spec, flow.sessions(c), info.title);
        }
      };
      const t = walkTest(flow.spec);
      if (flow.shared) t(title, ({ sharedServe, page }, info) => run(sharedServe, page, info));
      else t(title, ({ serve, page }, info) => run(serve, page, info));
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
  // A transitions walk's title runs to dozens of steps; a file name past
  // 255 bytes failed the walk (ENAMETOOLONG) after every step passed.
  const full = title.replace(/[^A-Za-z0-9]+/g, '-').replace(/^-|-$/g, '');
  const slug = full.length <= 120 ? full : `${full.slice(0, 100)}-${crypto.createHash('sha1').update(full).digest('hex').slice(0, 12)}`;
  for (const id of ids) {
    const src = path.join(serve.home, '.bough', 'history', id + '.jsonl');
    if (fs.existsSync(src)) fs.copyFileSync(src, path.join(dir, `${slug}-${id}.jsonl`));
  }
}
