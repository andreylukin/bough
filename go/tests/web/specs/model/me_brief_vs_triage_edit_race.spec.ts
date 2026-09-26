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
// person has for them) whenever the signal has a row, and through the API
// when the last regenerate left it out; Undismiss has no UI (a dismissed row offers no
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

// The page's abstract state: present, dismissed and pinned come through
// the DOM while the row is on screen (the Pin/Unpin button's own label,
// the dismissed-count line — a dismissed row's Pin/Dismiss buttons are
// gone, so a shown row is present && !dismissed and a counted-but-hidden
// row is present && dismissed). A signal the last regenerate left out of
// signals.json (present == False, reachable whenever a GatherStart read
// a dismissal) has neither a row nor a count, so the page shows nothing
// of its triage at all: dismissed and pinned are then read from GET
// /api/me, the same triage.json the page renders from and the same read
// the Go adapter makes. Reading them off the DOM there reported
// dismissed=false for a signal triage.json still dismisses.
// gathering and snap_dismissed are the walk's own state, the same
// "tracked by the adapter" case the recipe allows for the client's own
// state.
async function shownRow(c: Ctx): Promise<boolean> {
  const p = c.page;
  return (await p.getByRole('button', { name: `Pin ${TITLE}`, exact: true }).count()) > 0
    || (await p.getByRole('button', { name: `Unpin ${TITLE}`, exact: true }).count()) > 0;
}

// A triage with no UI to reach it: the same POST /api/me/triage the
// page's own controls send, then a reload so the page's own sticky
// triage override (set by the last UI triage's answer, and otherwise
// never refreshed from the server) does not mask a change it never made.
async function mark(c: Ctx, action: string): Promise<void> {
  const res = await c.serve.api.post('/api/me/triage', { data: { action, key: CITE } });
  if (!res.ok()) throw new Error(`${action}: ${res.status()} ${await res.text()}`);
  await c.page.reload();
  await primary(c).first().waitFor();
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const p = c.page;
  const shown = await shownRow(c);
  const pinned = (await p.getByRole('button', { name: `Unpin ${TITLE}`, exact: true }).count()) > 0;
  const dismissedShown = (await p.locator('.me-dismissed').count()) > 0;
  let dismissed = dismissedShown;
  let pinnedNow = pinned;
  if (!shown && !dismissedShown) {
    const res = await c.serve.api.get('/api/me');
    if (!res.ok()) throw new Error(`GET /api/me: ${res.status()} ${await res.text()}`);
    const t: TriageWire = (await res.json()).triage ?? {};
    dismissed = CITE in (t.dismissed ?? {});
    pinnedNow = (t.pinned ?? []).includes(CITE);
  }
  return {
    present: shown || dismissedShown,
    dismissed,
    pinned: pinnedNow,
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
    // exercised by ui_me and me_page. The spec lets Dismiss, Pin and
    // Unpin land while the last regenerate left the signal out
    // (present == False); the page then has no row to triage from, so
    // those go through the API like Undismiss.
    async Dismiss(c) {
      if (!(await shownRow(c))) return mark(c, 'dismiss');
      await c.page.getByRole('button', { name: `Dismiss ${TITLE}`, exact: true }).click();
      await c.page.getByRole('menuitem', { name: 'Just this one', exact: true }).click();
    },
    // No UI: a dismissed row offers no restore control.
    Undismiss: (c) => mark(c, 'undismiss'),
    async Pin(c) {
      if (!(await shownRow(c))) return mark(c, 'pin');
      await c.page.getByRole('button', { name: `Pin ${TITLE}`, exact: true }).click();
    },
    async Unpin(c) {
      if (!(await shownRow(c))) return mark(c, 'unpin');
      await c.page.getByRole('button', { name: `Unpin ${TITLE}`, exact: true }).click();
    },
  },

  read: readUiState,
  status: primary,
  // No session is driven here (the race is two file touches, not a model
  // turn), so nothing goes to the trace check.
  sessions: () => [],
});
