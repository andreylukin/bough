// go/tests/model/specs/skills_mentions.fizz walked in the browser
// (recipe: go/tests/model/README.md): the composer's Skills button and
// its / and @ mentions, for one open session. Every generated path is
// one test against a real serve; at each node readUiState must equal the
// spec's Composer#0 state.
//
// The page decides nothing about when its reads answer, so the walk
// does: every GET /api/skills and GET /api/files is held at the network
// until the spec's *Loaded or *Failed lets it go. That makes "loading" a
// state the page sits in, and a failed read one the walk chose.
import * as fs from 'fs';
import * as path from 'path';
import type { Page, Route } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import type { Serve } from '../../helpers/serve';

// One skill per pool the catalogue must merge or drop (the Go adapter's
// fixture, go/tests/model/mbt/skills_mentions_test.go): a catalogue
// read against serve's cwd lacks the repo skill, and one that ignores
// off.yml offers the off one.
const HOME_SKILL = 'alpha-home';   // ~/.claude/skills
const OFF_SKILL = 'beta-off';      // ~/.claude/skills, off in off.yml
const REPO_SKILL = 'gamma-repo';   // <session cwd>/.claude/skills
const FILE = 'notes.txt';          // in the session's cwd, for @
const PATH = '/tmp/x';             // the leading path TypePath types
const PASTE = ' see @foo';         // pasted text ending in a would-be token

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;
  skills: Route[];  // held GET /api/skills
  files: Route[];   // held GET /api/files
  turn: number;
  // What the page last showed of the catalogue, carried between nodes
  // where no list is on screen: the page holds it (mention.tsx
  // `catalogue`) but draws it only inside an open picker.
  cached: boolean;
  listed: string;
  known: string[];
  typed: number;     // letters of the mention token typed so far
  typedPath: boolean; // the spec's own client record, like example's viewing
  // A paste leaves a token at the caret that the page keeps shut
  // (app.tsx `pasted`, a ref no element shows) until the next real key.
  pasted: boolean;
}

const composer = (c: Ctx) => c.page.locator('#composer');
const skillsBtn = (c: Ctx) => c.page.getByRole('button', { name: 'Skills', exact: true });
const pop = (c: Ctx) => c.page.locator('.skills-pop');
const mention = (c: Ctx) => c.page.locator('.composer .mention');

const skill = (name: string) => `---\ndescription: The ${name} fixture skill.\n---\nfixture body of ${name}\n`;

function write(file: string, body: string): void {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, body);
}

// A failed read the page cannot tell from any other. A non-2xx would do
// the same, but Chromium logs every one as a console error, which the
// per-node invariant forbids; a 200 whose body is not JSON fails the
// same r.json() without that noise.
const failWith = (r: Route) => r.fulfill({ status: 200, contentType: 'application/json', body: 'not json: read failed' });

// Every held request of a kind, waited for: the @ read goes out 140 ms
// after the last keystroke on the page's clock, so the clock is moved
// until it has.
async function held(c: Ctx, q: Route[]): Promise<Route[]> {
  const deadline = Date.now() + 10_000;
  while (q.length === 0) {
    if (Date.now() > deadline) throw new Error('skills-mentions: no request to answer');
    await c.page.clock.fastForward(200);
    await new Promise((r) => setTimeout(r, 25));
  }
  return q.splice(0);
}

// Answers the newest held read; older ones answer too (continue), and
// the page drops them, as it does a slow reply to an older token.
async function answer(c: Ctx, q: Route[], ok: boolean): Promise<void> {
  const rs = await held(c, q);
  const last = rs.pop()!;
  for (const r of rs) await r.continue();
  await (ok ? last.continue() : failWith(last));
}

// The page's own rule (mention.tsx triggerAt): the / or @ token at the
// caret, if the sigil starts a word.
function tokenAt(text: string, caret: number): { kind: string; token: string } | null {
  for (let i = caret - 1; i >= 0; i--) {
    const ch = text[i];
    if (ch === ' ' || ch === '\n' || ch === '\t') return null;
    if (ch === '/' || ch === '@') {
      const before = i === 0 ? '' : text[i - 1];
      if (before && !/\s/.test(before)) { if (ch === '/') continue; return null; }
      return { kind: ch, token: text.slice(i + 1, caret) };
    }
  }
  return null;
}

// Skill rows on screen; the token or filter they are filtered by.
function noteList(c: Ctx, names: string[], filter: string): void {
  c.cached = true;
  for (const n of names) if (!c.known.includes(n)) c.known.push(n);
  const whole = !filter && names.includes(HOME_SKILL);
  const ok = names.includes(REPO_SKILL) && !names.includes(OFF_SKILL) && (whole || filter !== '');
  c.listed = ok ? 'session' : `other: ${names.join(',')}`;
}

function noteFailed(c: Ctx): void {
  c.cached = false;
  c.listed = '';
}

async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const { page } = c;

  let picker = 'closed';
  if (await pop(c).count()) {
    const rows = await pop(c).locator('.skill .skill-name').allTextContents();
    const note = (await pop(c).locator('.skills-empty').allTextContents())[0] ?? '';
    if (rows.length) {
      picker = 'loaded';
      noteList(c, rows.map((r) => r.replace(/^\//, '')), await pop(c).locator('.skills-filter').inputValue());
    } else if (note.startsWith('Couldn’t load')) {
      picker = 'error';
      noteFailed(c);
    } else if (note.startsWith('No skills')) picker = 'loaded';
    else picker = 'loading';
  }

  const { value, caret, focused } = await composer(c).evaluate((el: HTMLTextAreaElement) => ({
    value: el.value, caret: el.selectionStart ?? 0, focused: document.activeElement === el,
  }));
  const at = focused ? tokenAt(value, caret) : null;

  let m = 'none';
  let kind = '';
  if (await mention(c).count()) {
    const list = mention(c).locator('.mention-list');
    const note = (await mention(c).locator('.mention-state').allTextContents())[0] ?? '';
    if (await list.count()) {
      m = 'open';
      kind = (await list.getAttribute('aria-label')) === 'Files' ? '@' : '/';
      if (kind === '/') {
        const rows = await list.locator('.mention-name').allTextContents();
        noteList(c, rows.map((r) => r.replace(/^\//, '')), at?.token ?? '');
      }
    } else {
      kind = /files/.test(note) ? '@' : '/';
      if (note.startsWith('Couldn’t load')) {
        m = 'error';
        if (kind === '/') noteFailed(c);
      } else if (note.startsWith('No ')) m = 'open';
      else m = 'loading';
    }
  } else if (at && !c.pasted) {
    // A token at the caret with no picker over it: Escape shut it.
    m = 'dismissed';
    kind = at.kind;
  }

  const lead = value.trimStart().split(/\s/)[0];
  // The transcript: a turn the session took (a "/" line the child runs
  // as a command is a turn with no prompt bubble; one still on its way
  // is .turn-sending), and a skill that came in with it (prompt.tsx
  // folds the "[skill: name]" block the child injects into a chip).
  const tr = page.locator('.transcript');
  const sent = (await tr.locator('section.turn:not(.turn-sending)').count()) > 0;
  const ran = (await tr.locator('.prompt-chip-skill').count()) > 0;
  return {
    picker,
    mention: m,
    kind,
    cached: c.cached,
    listed: c.listed,
    lead: lead.startsWith(PATH) ? 'path' : lead.startsWith('/') && c.known.includes(lead.slice(1)) ? 'skill' : 'none',
    hasPath: value.includes(PATH),
    typedPath: c.typedPath,
    sent,
    ran,
  };
}

// Keys land in the composer where the caret is, without a click: a
// click is itself a caret move the page answers (caretTrigger).
async function focusAt(c: Ctx, where: 'start' | 'end' | 'keep'): Promise<void> {
  await composer(c).evaluate((el: HTMLTextAreaElement, w) => {
    el.focus();
    if (w === 'start') el.setSelectionRange(0, 0);
    else if (w === 'end') el.setSelectionRange(el.value.length, el.value.length);
  }, where);
}

async function type(c: Ctx, s: string): Promise<void> {
  c.pasted = false;
  await c.page.keyboard.type(s);
}

modelTests<Ctx>({
  spec: 'skills_mentions',
  role: 'Composer#0',
  config: CONTROL_CONFIG,

  async init(page, serve) {
    write(path.join(serve.home, '.claude', 'skills', HOME_SKILL, 'SKILL.md'), skill(HOME_SKILL));
    write(path.join(serve.home, '.claude', 'skills', OFF_SKILL, 'SKILL.md'), skill(OFF_SKILL));
    write(path.join(serve.home, '.bough', 'off.yml'), `disabled:\n  - skill:${OFF_SKILL}\n`);
    write(path.join(serve.work, '.claude', 'skills', REPO_SKILL, 'SKILL.md'), skill(REPO_SKILL));
    write(path.join(serve.work, FILE), 'plain words\n');

    const c: Ctx = {
      page, serve, id: await serve.newSession(), skills: [], files: [], turn: 0,
      cached: false, listed: '', known: [], typed: 0, typedPath: false, pasted: false,
    };
    // The page bounds each read at 15 s (AbortSignal.timeout) and shows
    // a failure: the spec's fair *Failed, which the walk fires itself.
    // The walk moves the page's clock 5 s per read, so a read held over
    // three nodes would time out where the spec still says loading.
    await page.addInitScript(() => { AbortSignal.timeout = () => new AbortController().signal; });
    await page.route((u) => u.pathname === '/api/skills', (r) => { c.skills.push(r); });
    await page.route((u) => u.pathname === '/api/files', (r) => { c.files.push(r); });
    await page.goto(`${serve.url}/#/s/${c.id}`);
    await composer(c).waitFor();
    await skillsBtn(c).waitFor();
    return c;
  },

  actions: {
    // --- the Skills button ---
    OpenPicker: (c) => skillsBtn(c).click(),
    ClosePicker: (c) => pop(c).locator('.skills-filter').press('Escape'),
    RetryPicker: (c) => pop(c).getByRole('button', { name: 'Retry' }).click(),
    // The repo skill, the one only the session's catalogue has; a list
    // without it gets its last row, so a wrong list still picks.
    async PickSkill(c) {
      const want = pop(c).locator('.skill', { hasText: `/${REPO_SKILL}` });
      await ((await want.count()) ? want : pop(c).locator('.skill').last()).click();
    },
    PickerLoaded: (c) => answer(c, c.skills, true),
    PickerFailed: (c) => answer(c, c.skills, false),

    // --- the composer mentions ---
    async TypeSlash(c) {
      c.typed = 0;
      await focusAt(c, 'start');
      await type(c, '/');
    },
    async TypeAt(c) {
      c.typed = 0;
      await focusAt(c, 'end');
      const v = await composer(c).inputValue();
      await type(c, v && !v.endsWith(' ') ? ' @' : '@');
    },
    // The next letter of what the walk would pick, so the rows it
    // filters to still hold it.
    async TypeToken(c) {
      const at = tokenAt(await composer(c).inputValue(), (await composer(c).evaluate((el: HTMLTextAreaElement) => el.selectionStart)) ?? 0);
      const target = at?.kind === '@' ? FILE : REPO_SKILL;
      await focusAt(c, 'keep');
      await type(c, target[c.typed++] ?? 'z');
    },
    Dismiss: async (c) => { await focusAt(c, 'keep'); await c.page.keyboard.press('Escape'); },
    RetryMention: (c) => mention(c).getByRole('button', { name: 'Retry' }).click(),
    async PickMention(c) {
      const row = mention(c).locator('.mention-item', { hasText: new RegExp(`^/?(${REPO_SKILL}|${FILE.replace('.', '\\.')})`) });
      if (await row.count()) await row.first().click();
      else { await focusAt(c, 'keep'); await c.page.keyboard.press('Enter'); }
    },
    MentionLoaded: async (c) => {
      const kind = (await mention(c).locator('.mention-state').allTextContents())[0] ?? '';
      await answer(c, /files/.test(kind) || c.files.length ? c.files : c.skills, true);
    },
    MentionFailed: async (c) => {
      const kind = (await mention(c).locator('.mention-state').allTextContents())[0] ?? '';
      await answer(c, /files/.test(kind) || c.files.length ? c.files : c.skills, false);
    },

    // A real paste: the clipboard, then the paste chord, at the end of
    // the draft.
    async Paste(c) {
      await c.page.context().grantPermissions(['clipboard-read', 'clipboard-write']);
      await c.page.evaluate((t) => navigator.clipboard.writeText(t), PASTE);
      await focusAt(c, 'end');
      c.pasted = true;
      await c.page.keyboard.press('ControlOrMeta+V');
    },

    // --- the draft ---
    async TypePath(c) {
      await focusAt(c, 'start');
      await type(c, PATH + ' ');
      c.typedPath = true;
    },
    // Enter in the composer. A skill's turn is answered by llm-control;
    // a leading path is a "/" line the child answers itself.
    async Send(c) {
      const lead = (await composer(c).inputValue()).trimStart();
      if (!lead.startsWith(PATH)) {
        queue(controlDir(c.serve.home), `sm${String(++c.turn).padStart(4, '0')}`, { mode: 'ok', text: 'ran it' });
      }
      await focusAt(c, 'keep');
      c.pasted = false;
      await c.page.keyboard.press('Enter');
      c.typedPath = false;
    },
  },

  read: readUiState,
  status: composer,
  sessions: (c) => [c.id],
  async cleanup(c) {
    await c.page.unrouteAll({ behavior: 'ignoreErrors' });
  },
});
