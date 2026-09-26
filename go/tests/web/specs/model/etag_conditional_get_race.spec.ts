// go/tests/model/specs/etag_conditional_get_race.fizz in the browser
// (recipe: go/tests/model/README.md): one poller's conditional GET
// against writeJSONTagged (internal/serve/etag.go), racing a write to
// the content it serves. The Go twin is
// go/tests/model/mbt/etag_conditional_get_race_test.go, which plays the
// same real round trip with net/http and keeps version/inflight/snapshot/
// client_tag as its own bookkeeping — nothing on screen shows the
// server's true revision counter or the tag a disk cache holds, so here
// too they are the walk's bookkeeping, not a DOM read. `result` is not:
// it comes off the sidebar's real list poll (GET /api/sessions) against
// the real server, so a server or frontend bug in the fresh/304 split
// still shows up here.
//
// StartPoll and Respond split one browser fetch in two, exactly as the
// Go adapter splits one net/http round trip: the interval is ticked with
// the page's clock to make the sidebar send its GET, which is let out to
// the real server right away (route.fetch(), a genuine round trip whose
// status is already decided) carrying the walk's own cached ETag —
// Chromium does not reliably cache a response this suite delivered
// through route.fulfill, so the walk holds that tag itself, exactly as
// the Go adapter holds `lastETag`. The answer's *delivery* to the page is
// held until Respond: a 304 is turned back into the cached 200 a real
// browser's disk cache would have handed the page (the app's own fetch
// wrapper treats any non-2xx as a failure, so a raw 304 would be a
// spurious list-load error, not what a real tab ever sees). A Write in
// between still lands strictly before Respond, which is the race: the
// answer StartPoll already got can be stale by the time the page sees
// it.
import type { Page, Route } from '@playwright/test';
import { modelTests } from '../../helpers/model';
import type { Serve } from '../../helpers/serve';

// The list poll while a thread is open (app.tsx POLL_MS * 3): the walk
// opens a decoy thread so a fixed fast-forward always crosses exactly
// one tick.
const TICK_MS = 12_001;

interface Held { route: Route; res: Awaited<ReturnType<Route['fetch']>> }

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;     // the session Write changes, to move the list's ETag
  decoy: string;  // kept open so the list poll's interval is fixed
  holding: boolean;
  captured: Route[]; // list GETs seen once holding, oldest first
  held?: Held;        // StartPoll's real answer, not yet delivered
  lastETag: string;   // the walk's own cached tag, sent as If-None-Match
  lastBody: string;   // the body that tag was issued for, for a 304's synthesized cache hit
  version: number;
  inflight: boolean;
  snapshot: number;
  clientTag: number;
  result: string;
}

async function until(what: string, ok: () => boolean | Promise<boolean>, ms = 10_000): Promise<void> {
  const deadline = Date.now() + ms;
  while (!(await ok())) {
    if (Date.now() > deadline) throw new Error(`etag_conditional_get_race: waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 25));
  }
}

const row = (c: Ctx) => c.page.locator(`button.row[data-id="${c.id}"]`).first();

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  return {
    version: c.version,
    inflight: c.inflight,
    snapshot: c.snapshot,
    client_tag: c.clientTag,
    result: c.result,
  };
}

modelTests<Ctx>({
  spec: 'etag_conditional_get_race',
  role: 'Poll#0',
  // The spec models the poll itself as StartPoll/Respond: an automatic
  // clock step per read would fire it where the path says it has not.
  pollStepMs: 0,

  async init(page, serve) {
    const c: Ctx = {
      page, serve, id: await serve.newSession(), decoy: await serve.newSession(),
      holding: false, captured: [], lastETag: '', lastBody: '',
      version: 0, inflight: false, snapshot: -1, clientTag: -1, result: '',
    };
    await page.route((u) => u.pathname === '/api/sessions', async (route) => {
      if (route.request().method() !== 'GET') return route.fallback();
      if (!c.holding) { await route.fulfill({ response: await route.fetch() }); return; }
      c.captured.push(route);
    });
    // The decoy, not the session under test, is open: fixes the list
    // poll's interval.
    await page.goto(`${serve.url}/#/s/${c.decoy}`);
    await row(c).waitFor();
    c.holding = true;
    return c;
  },

  actions: {
    // The server's read of the current content and its ETag decision: a
    // real round trip, right away, carrying the tag this walk is holding
    // — its answer just isn't delivered to the page yet.
    async StartPoll(c) {
      const before = c.captured.length;
      await c.page.clock.fastForward(TICK_MS);
      await until('the list poll to reach the server', () => c.captured.length > before);
      const route = c.captured[c.captured.length - 1];
      c.snapshot = c.version;
      c.inflight = true;
      const headers = { ...route.request().headers() };
      if (c.lastETag) headers['if-none-match'] = c.lastETag;
      else delete headers['if-none-match'];
      c.held = { route, res: await route.fetch({ headers }) };
    },

    // A turn landing a new message: nothing in the UI stands for "the
    // content changed", so this goes straight at the API, as the Go
    // adapter's Write does with servetest.Rename — any distinct title
    // moves the list's hash, which is all a Write needs to do.
    async Write(c) {
      c.version++;
      const res = await c.serve.api.post(`/api/sessions/${c.id}/rename`, { data: { title: `v${c.version}` } });
      if (!res.ok()) throw new Error(`rename: ${res.status()} ${await res.text()}`);
    },

    // The already-decided answer reaches the page now.
    async Respond(c) {
      const held = c.held!;
      if (held.res.status() === 304) {
        c.result = 'not_modified';
        // What a real browser's disk cache hands the page on a 304: the
        // body it already held, not the empty 304 itself (the app's own
        // fetch wrapper treats any non-2xx as a load failure).
        await held.route.fulfill({ status: 200, contentType: 'application/json', body: c.lastBody });
      } else {
        c.result = 'fresh';
        c.clientTag = c.snapshot;
        c.lastETag = held.res.headers()['etag'] ?? '';
        c.lastBody = await held.res.text();
        await held.route.fulfill({ response: held.res });
      }
      c.held = undefined;
      c.inflight = false;
    },
  },

  read: readUiState,
  status: row,
  // Write only renames a session; there is no turn transcript for this
  // flow to replay (no historyProjections entry in the Go mbt package,
  // same as a ui_* flow that is the page's own state).
  sessions: () => [],
  async cleanup(c) {
    c.holding = false;
    if (c.held) await c.held.route.fulfill({ response: c.held.res }).catch(() => {});
    await c.page.unrouteAll({ behavior: 'ignoreErrors' });
  },
});
