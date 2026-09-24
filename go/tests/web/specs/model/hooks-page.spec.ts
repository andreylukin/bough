// go/tests/model/specs/hooks_page.fizz walked in the browser (recipe:
// go/tests/model/README.md): the Hooks page's list poll, one hook's file
// opened, edited, saved and dry-run, its off switch, and the API callers
// serve must refuse. Every generated path is one test against a real
// serve; at each node readUiState must equal the spec's Page#0 state.
//
// The page decides nothing about when its reads answer, so the walk
// does: every GET /api/hooks and GET /api/hooks/file is held at the
// network until the spec's Poll/PollFail or Loaded/LoadFail lets it go.
// That is what makes "loading" a state the page sits in rather than a
// blink, and a failed read one the walk chose.
import * as fs from 'fs';
import * as path from 'path';
import type { Page, Route } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import type { Serve } from '../../helpers/serve';

// The hook the page opens. pre-code-exec never fires here (llm-control
// replies with text, no code runs), so editing it or switching it off
// cannot change what a Fire turn records.
const EVENT = 'pre-code-exec';
const NAME = 'guard.js';
const OK = '// guard: returns\nreturn {};\n';
const BROKEN = '// guard: throws\nthrow new Error("guard is broken");\n';
// The hook a Fire turn trips: always on, never edited.
const FIRE_EVENT = 'user-prompt-submit';
const FIRE_NAME = 'fire.js';
const RULE_BODY = 'Write tests first.\n';

// Same as the page's list poll (hooks.tsx POLL_MS): moving the page's
// clock this far makes the interval ask again.
const POLL_MS = 5_000;

interface Ctx {
  page: Page;
  serve: Serve;
  hook: string;        // guard.js on disk
  rule: string;        // a rule the page lists read-only
  outside: string[];   // every file a refused PUT would have created
  offYml: string;
  lists: Route[];      // held GET /api/hooks
  files: Route[];      // held GET /api/hooks/file
  fired: string[];     // one session per Fire
  turn: number;
  // What the refused callers saw; no page shows another client's
  // request, so these are the test's own record (see PutOutside).
  wroteOutside: boolean;
  wroteBadKind: boolean;
}

const version = (body: string) => (body.includes('throw') ? 'broken' : 'ok');

// guard.js's row in the Hooks section; "Recent decisions" rows use
// .hk-name, not .hk2-name, so a fire of it cannot be mistaken for it.
const guardRow = (c: Ctx) =>
  c.page.locator('.hk2-row').filter({ has: c.page.locator('.hk2-name', { hasText: new RegExp(`^${NAME}$`) }) });

// A failed read the page cannot tell from any other. A 500 would do the
// same to the page but Chromium logs every non-2xx fetch as a console
// error, which the per-node invariant forbids; a 200 whose body is not
// JSON fails the same req() call without that noise.
const failWith = (r: Route) => r.fulfill({ status: 200, contentType: 'application/json', body: 'not json: read failed' });

// The next held request of a kind. A list read only goes out on the
// poll's interval, so the page's clock is moved until one is held; a
// file read goes out on the click that asked for it.
async function next(c: Ctx, held: Route[], tick: boolean): Promise<Route> {
  const deadline = Date.now() + 10_000;
  while (held.length === 0) {
    if (Date.now() > deadline) throw new Error('hooks-page: no request to answer');
    if (tick) await c.page.clock.fastForward(POLL_MS);
    await new Promise((r) => setTimeout(r, 25));
  }
  return held.shift()!;
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const { page } = c;
  const hasData = (await page.locator('.hk-chips').count()) > 0;
  let list = 'loading';
  if (await page.locator('.hk-stale').count()) list = 'stale';
  else if (hasData) list = 'loaded';
  else if (await page.locator('.error-note-title', { hasText: /^Couldn’t load/ }).count()) list = 'error';

  const row = guardRow(c);
  const hasRow = (await row.count()) > 0;
  let file = 'closed';
  let buffer = '';
  let note = '';
  if (hasRow && (await row.locator('.hk-toggle').getAttribute('aria-expanded')) === 'true') {
    const text = row.locator('textarea.hk-edit');
    if (await text.count()) {
      file = 'open';
      buffer = version(await text.inputValue());
      // allTextContents does not wait for a note that is not there.
      const said = (await row.locator('.hk-note[role=status]').allTextContents())[0] ?? '';
      if (said === 'Saved.') note = 'saved';
      else if (said.startsWith('Dry run finished')) note = 'ran';
      else if (await row.locator('.hk-panel > p.err[role=alert]').count()) note = 'failed';
    } else if (await row.locator('.hk-loaderr').count()) file = 'error';
    else file = 'loading';
  }
  // The page never shows the saved bytes apart from the buffer, so disk
  // is read where Save put them: what the page's Save actually wrote.
  const disk = version(fs.readFileSync(c.hook, 'utf8'));
  if (file !== 'open') buffer = disk;

  let unshown = false;
  for (const id of c.fired) {
    if ((await page.locator(`.hk-decisions a.hk-sess[href="#/s/${id}"]`).count()) === 0) unshown = true;
  }
  return {
    list,
    unshown,
    file,
    disk,
    buffer,
    note,
    hook_off: hasRow && (await row.evaluate((el) => el.classList.contains('hk2-off'))),
    wrote_outside: c.wroteOutside,
    wrote_bad_kind: c.wroteBadKind,
  };
}

modelTests<Ctx>({
  spec: 'hooks_page',
  role: 'Page#0',
  config: CONTROL_CONFIG,

  async init(page, serve) {
    const b = path.join(serve.home, '.bough');
    const files: Record<string, string> = {
      [`hooks/${EVENT}/${NAME}`]: OK,
      [`hooks/${FIRE_EVENT}/${FIRE_NAME}`]: '// fire: records a fire\nreturn {};\n',
    };
    for (const [rel, text] of Object.entries(files)) {
      fs.mkdirSync(path.dirname(path.join(b, rel)), { recursive: true });
      fs.writeFileSync(path.join(b, rel), text);
    }
    const rule = path.join(serve.home, '.claude', 'rules', 'style.md');
    fs.mkdirSync(path.dirname(rule), { recursive: true });
    fs.writeFileSync(rule, RULE_BODY);
    // A symlink in the pool pointing out of it: writing through it
    // would land outside the pools.
    const elsewhere = path.join(serve.home, 'elsewhere');
    fs.mkdirSync(elsewhere);
    fs.symlinkSync(elsewhere, path.join(b, 'hooks', EVENT, 'out'));

    const c: Ctx = {
      page, serve, rule, offYml: path.join(b, 'off.yml'),
      hook: path.join(b, 'hooks', EVENT, NAME),
      outside: [path.join(serve.home, 'outside.js'), path.join(elsewhere, 'evil.js'), path.join(b, 'hooks', EVENT, 'notes.md')],
      lists: [], files: [], fired: [], turn: 0, wroteOutside: false, wroteBadKind: false,
    };
    await page.route((u) => u.pathname === '/api/hooks', (r) => { c.lists.push(r); });
    await page.route((u) => u.pathname === '/api/hooks/file' && u.searchParams.has('path'), (r) => {
      if (r.request().method() === 'GET') c.files.push(r);
      else void r.continue();
    });
    await page.goto(`${serve.url}/#/hooks`);
    return c;
  },

  actions: {
    Poll: async (c) => (await next(c, c.lists, true)).continue(),
    PollFail: async (c) => failWith(await next(c, c.lists, true)),

    // A hook fires in another session, as it would in any session serve
    // runs: the turn is started through the API, and the step ends once
    // the ledger has it (the spec's claim is that the next poll shows it).
    async Fire(c) {
      const name = `h${String(++c.turn).padStart(4, '0')}`;
      queue(controlDir(c.serve.home), name, { mode: 'ok', text: `fired ${name}` });
      const id = await c.serve.newSession(`turn ${name}`);
      c.fired.push(id);
      const deadline = Date.now() + 20_000;
      for (;;) {
        const d = (await (await c.serve.api.get('/api/hooks')).json()) as { fires: { session: string }[] };
        if (d.fires.some((f) => f.session === id)) return;
        if (Date.now() > deadline) throw new Error(`the fire of session ${id} never reached /api/hooks`);
        await new Promise((r) => setTimeout(r, 50));
      }
    },

    Open: (c) => guardRow(c).getByRole('button', { name: 'Open definition' }).click(),
    Loaded: async (c) => (await next(c, c.files, false)).continue(),
    LoadFail: async (c) => failWith(await next(c, c.files, false)),
    FileRetry: (c) => guardRow(c).locator('.hk-loaderr').getByRole('button', { name: 'Retry' }).click(),

    async Edit(c) {
      const text = guardRow(c).locator('textarea.hk-edit');
      await text.fill(version(await text.inputValue()) === 'ok' ? BROKEN : OK);
    },
    Save: (c) => guardRow(c).getByRole('button', { name: 'Save', exact: true }).click(),
    DryRun: (c) => guardRow(c).getByRole('button', { name: 'Dry run', exact: true }).click(),

    Toggle: (c) => guardRow(c).locator('.hk-offbtn').click(),

    // No page offers these: they are another client trying the three
    // ways out of the pools (and a rule file), then an unknown off kind.
    // Anything but a 400, or a file left behind, is what the spec calls
    // wrote_outside / wrote_bad_kind.
    async PutOutside(c) {
      const pool = path.join(c.serve.home, '.bough', 'hooks', EVENT);
      for (const p of [`${pool}/../../../outside.js`, path.join(pool, 'out', 'evil.js'), path.join(pool, 'notes.md'), c.rule]) {
        const res = await c.serve.api.put('/api/hooks/file', { data: { path: p, body: 'written from outside\n' } });
        if (res.status() !== 400) c.wroteOutside = true;
      }
      if (c.outside.some((p) => fs.existsSync(p)) || fs.readFileSync(c.rule, 'utf8') !== RULE_BODY) c.wroteOutside = true;
    },
    async OffBadKind(c) {
      const res = await c.serve.api.post('/api/off', { data: { id: 'bogus:x', off: true } });
      const off = fs.existsSync(c.offYml) ? fs.readFileSync(c.offYml, 'utf8') : '';
      if (res.status() !== 400 || off.includes('bogus')) c.wroteBadKind = true;
    },
  },

  read: readUiState,
  // With data, guard.js's row carries the state; before, the page's
  // skeleton or its error note does.
  status: (c) => c.page.locator(`.hk2-row:has(.hk2-name:text-is("${NAME}")), .proj-body > .skeleton, .proj-body > .error-note`).first(),
  sessions: (c) => c.fired,
  async cleanup(c) {
    await c.page.unrouteAll({ behavior: 'ignoreErrors' });
  },
});
