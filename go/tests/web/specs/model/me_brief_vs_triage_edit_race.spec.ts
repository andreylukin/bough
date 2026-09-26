// go/tests/model/specs/me_brief_vs_triage_edit_race.fizz in the browser:
// the Me page's view of one signal ("k1") racing the background regenerate
// (the launchd tick's headless `/llm-wiki brief`) against the person's own
// triage edit. As in the Go adapter (go/tests/model/mbt/me_brief_vs_triage_edit_race_test.go),
// the regenerate is not run for real — spawning it would cost a model turn
// per gather and the race is entirely about file timing, not the skill's
// prose — so the walk plays its two file touches itself: GatherStart reads
// triage.json's dismissed set the way SKILL.md's "Then read
// topics/me/triage.json" does, and GatherWrite rewrites signals.json,
// filtered by that stale snapshot rather than by triage.json as it now
// stands. Dismiss, Pin and Unpin go through the real page (the only UI a
// person has for them); Undismiss has no UI (a dismissed row offers no
// restore control) and goes through the API a person cannot otherwise
// reach, followed by a reload so the page's own sticky triage override
// (me.tsx's `onTriage` sets `triage` in state once and keeps it until the
// next UI triage — see sticky_override_file_writers) does not mask a
// change it never made.
import * as fs from 'fs';
import * as path from 'path';
import type { Page } from '@playwright/test';
import { modelTests } from '../../helpers/model';
import type { Serve } from '../../helpers/serve';

const TITLE = 'Review';
const CITE = 'k1';

interface Ctx {
  page: Page;
  serve: Serve;
  // The background regenerate's own state, exactly as the Go adapter
  // tracks it: not read from the server, since nothing in the product
  // exposes "a gather is in flight" for this simulated job.
  gathering: boolean;
  snapDismissed: boolean;
}

const meDir = (c: Ctx) => path.join(c.serve.home, '.bough', 'wiki', 'topics', 'me');
const triagePath = (c: Ctx) => path.join(meDir(c), 'triage.json');
const signalsPath = (c: Ctx) => path.join(meDir(c), 'signals.json');

interface TriageWire { dismissed?: Record<string, string>; pinned?: string[] }

function readTriage(c: Ctx): TriageWire {
  try { return JSON.parse(fs.readFileSync(triagePath(c), 'utf8')); } catch { return {}; }
}

// runBrief's one rewrite of signals.json: "k1" present, or, dismissed at
// the snapshot GatherStart read, left out of items the way SKILL.md's
// "never list [dismissed keys] again... in rows" has it.
function writeSignals(c: Ctx, present: boolean): void {
  const items = present ? [{ kind: 'needs-you', source: 'gh', cite: CITE, title: TITLE }] : [];
  fs.writeFileSync(signalsPath(c), JSON.stringify({ asOf: new Date().toISOString(), items, sources: [] }));
}

const primary = (c: Ctx) => c.page.locator('.me-bar, .me-loading');

// The page's abstract state: dismissed and pinned come from triage.json
// through the DOM (the Pin/Unpin button's own label, the dismissed-count
// line — a dismissed row's Pin/Dismiss buttons are gone, so dismissed and
// present are both readable only through their combination: a shown row
// (present, not dismissed), a counted-but-hidden row (present, dismissed),
// or neither (not present, whichever way triage.json currently has it —
// the one combination the page truly cannot distinguish, same as the Go
// adapter's own read of GET /api/me, which is fresh every time but still
// only ever sees present && !dismissed as a row and present && dismissed
// as a count). gathering and snap_dismissed are the walk's own state, the
// same "tracked by the adapter" case the recipe allows for the client's
// own state.
async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const p = c.page;
  const shown = (await p.getByRole('button', { name: `Pin ${TITLE}`, exact: true }).count()) > 0
    || (await p.getByRole('button', { name: `Unpin ${TITLE}`, exact: true }).count()) > 0;
  const pinned = (await p.getByRole('button', { name: `Unpin ${TITLE}`, exact: true }).count()) > 0;
  const dismissedShown = (await p.locator('.me-dismissed').count()) > 0;
  return {
    present: shown || dismissedShown,
    dismissed: dismissedShown,
    pinned,
    gathering: c.gathering,
    snap_dismissed: c.snapDismissed,
  };
}

modelTests<Ctx>({
  spec: 'me_brief_vs_triage_edit_race',
  role: 'Page#0',

  async init(page, serve) {
    const c: Ctx = { page, serve, gathering: false, snapDismissed: false };
    fs.mkdirSync(meDir(c), { recursive: true });
    fs.rmSync(triagePath(c), { force: true });
    writeSignals(c, true);
    await page.goto(`${serve.url}/#/me`);
    await primary(c).first().waitFor();
    return c;
  },

  actions: {
    // No UI: the background regenerate's one read of triage.json, well
    // before it gathers every source, exactly as SKILL.md has it. Nothing
    // on screen changes yet.
    async GatherStart(c) {
      c.snapDismissed = Boolean(readTriage(c).dismissed?.[CITE]);
      c.gathering = true;
    },
    // No UI: runBrief's single rewrite of signals.json at the end of its
    // headless run, filtered by the stale snapshot above. The page only
    // notices on its 30 s poll (me.tsx: useLoad(wikiApi.me, "me",
    // 30_000)); force it past that boundary here rather than leaving it
    // to the harness's 5 s-per-read default, the same way me_page.spec's
    // NextDay does for its own 30 s poll.
    async GatherWrite(c) {
      c.gathering = false;
      writeSignals(c, !c.snapDismissed);
      await c.page.clock.fastForward(30_000);
    },
    // The menu's dismiss action. The fixture signal has no repo or author,
    // so the menu offers only "Just this one" (no rule); the rule half is
    // exercised by ui_me and me_page.
    async Dismiss(c) {
      await c.page.getByRole('button', { name: `Dismiss ${TITLE}`, exact: true }).click();
      await c.page.getByRole('menuitem', { name: 'Just this one', exact: true }).click();
    },
    // No UI: a dismissed row offers no restore control. Goes through the
    // same POST /api/me/triage a person's undo would reach some other
    // way, then reloads so the page's own sticky triage override (set by
    // the last UI triage's answer, and otherwise never refreshed from the
    // server) does not keep showing the old dismissal.
    async Undismiss(c) {
      const res = await c.serve.api.post('/api/me/triage', { data: { action: 'undismiss', key: CITE } });
      if (!res.ok()) throw new Error(`undismiss: ${res.status()} ${await res.text()}`);
      await c.page.reload();
      await primary(c).first().waitFor();
    },
    Pin: (c) => c.page.getByRole('button', { name: `Pin ${TITLE}`, exact: true }).click(),
    Unpin: (c) => c.page.getByRole('button', { name: `Unpin ${TITLE}`, exact: true }).click(),
  },

  read: readUiState,
  status: primary,
  // No session is driven here (the race is two file touches, not a model
  // turn), so nothing goes to the trace check.
  sessions: () => [],
});
