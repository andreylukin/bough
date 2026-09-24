// go/tests/model/specs/attachments_paste.fizz in the browser (recipe:
// go/tests/model/README.md): one session's composer pasting long text,
// pasting an image, dropping a file, sending, steering and queueing them,
// and leaving for another session and coming back. Every generated path
// is walked through the page against a real serve whose model is
// llm-control; at each node readUiState must equal the spec's Composer.
//
// Uploads are held at the network (page.route) so the spec decides when
// one settles: UploadOk lets the request through to the real serve,
// UploadFail sends it on in a shape the real serve refuses (an image as
// text/plain: 415; a file for a session that does not exist: 404, which
// is what a thread that is still starting answers).
import type { EventEmitter } from 'events';
import type { ConsoleMessage, Page, Route } from '@playwright/test';
import { CONTROL_CONFIG, controlDir, queue, release, waitTaken } from '../../helpers/control';
import { modelTests } from '../../helpers/model';
import type { Serve } from '../../helpers/serve';

type Tag = 'none' | 'paste' | 'up_image' | 'up_file' | 'image' | 'file' | 'failed';

interface Seen { tag: Tag; can_send: boolean; sent: string; served: string }

interface Ctx {
  page: Page;
  serve: Serve;
  id: string;     // the session the spec is about
  other: string;  // where Leave goes; its composer must stay empty
  turn: number;
  held: string;   // the llm-control turn in flight, '' when none
  upload: { route: Route; file: boolean } | null;  // the upload being held
  seen: Seen;     // the thread's fields as last read off the page
  otherSeen: Tag; // the other session's composer, as last read
  refused: number; // uploads UploadFail had the serve refuse, whose log line is not the page's error
}

const PNG = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAAAAAA6fptVAAAACklEQVR4nGNgAAAAAgABSK+kcQAAAABJRU5ErkJggg==';
const LONG = Array.from({ length: 20 }, (_, i) => `pasted line ${i}`).join('\n');
const UPLOAD_RE = /\/api\/(attachments|sessions\/[^/]+\/files)(\?|$)/;
const TAG_RE = /\[(Image|File) #\d+\]|\[Pasted text #\d+ \+\d+ lines\]/;

const row = (c: Ctx, id = c.id) => c.page.locator(`button.row[data-id="${id}"]`).first();
const composer = (c: Ctx) => c.page.locator('#composer');
const sendButton = (c: Ctx) => c.page.getByRole('button', { name: /^(Send|Steer)$/ });

// The tag the composer on screen holds. Pending is the page's "Attaching
// image…" status; failed is its alert naming the tag (not attached, or
// unavailable when a send refused it).
async function composerTag(page: Page): Promise<Tag> {
  const draft = await page.locator('#composer').inputValue();
  const m = TAG_RE.exec(draft);
  if (!m) return 'none';
  if (!m[1]) return 'paste';
  const kind = m[1] === 'Image' ? 'image' : 'file';
  const n = /#(\d+)/.exec(m[0])![1];
  const alert = (await page.locator('.composer-bar [role=alert]').allInnerTexts()).join(' ');
  if (new RegExp(`${m[1]} #${n}\\]? not attached|\\[${m[1]} #${n}\\]`).test(alert)) return 'failed';
  const attaching = await page.locator('.composer-bar [role=status]', { hasText: 'Attaching' }).count();
  if (attaching) return kind === 'image' ? 'up_image' : 'up_file';
  return kind;
}

// The last message this page gave up: the last user bubble in the
// thread (a turn's prompt, a steer, a pending send) or queued row, in
// document order, which is the order they were given. What it shows
// decides sent: a paste's chip, the image's picture, a file's path; a
// bare tag is the placeholder the spec forbids. served is what the
// page got for it: the picture loaded ("image"), "Image unavailable"
// ("refused"), or a file named by its path with nothing fetching it
// ("refused": only a link to /api/attachments would serve it).
async function lastSent(page: Page): Promise<{ sent: string; served: string }> {
  return page.evaluate(() => {
    const all = [...document.querySelectorAll('.prompt-bubble, .queued-row')];
    const el = all[all.length - 1] as HTMLElement | undefined;
    if (!el) return { sent: 'none', served: 'none' };
    const text = el.innerText;
    const img = el.querySelector('img[alt^="Image #"]') as HTMLImageElement | null;
    const gone = /Image unavailable/.test(text);
    let sent = 'none';
    if (el.querySelector('.prompt-chip-paste') || text.includes('<pasted-text')) sent = 'paste';
    else if (img || gone) sent = 'image';
    else if (/\[File #\d+: [^\]]+\]/.test(text)) sent = 'file';
    else if (/\[(Image|File) #\d+\]|\[Pasted text #\d+/.test(text)) sent = 'placeholder';
    let served = 'none';
    if (img && img.complete && img.naturalWidth > 0) served = 'image';
    else if (gone) served = 'refused';
    else if (sent === 'file') served = el.querySelector('a[href*="/api/attachments"]') ? 'file' : 'refused';
    return { sent, served };
  });
}

// The session's state word is on its sidebar row, whichever session is
// open (see example.spec.ts): "<title>, Running, <age> ago…". The title
// is the first message, which may hold commas of its own, so the word is
// matched in place rather than by position.
async function isRunning(c: Ctx): Promise<boolean> {
  const label = (await row(c).getAttribute('aria-label')) ?? '';
  return /, Running, [^,]+ ago/.test(label);
}

// Every field off the page. The thread's own fields (tag, can_send,
// sent, served) are only on screen while it is open; away, they are
// what the page showed last, moved by the uploads this walk settled
// while away (the one change the spec allows then), and Return reads
// them all off the page again. other is read off the other session's
// composer whenever it is open.
async function readUiState(c: Ctx): Promise<Record<string, unknown>> {
  const hash = await c.page.evaluate(() => window.location.hash);
  const viewing = hash === `#/s/${c.id}` && (await row(c).getAttribute('aria-current')) === 'true';
  if (viewing) {
    c.seen = { tag: await composerTag(c.page), can_send: await sendButton(c).isEnabled(), ...(await lastSent(c.page)) };
  } else if (hash === `#/s/${c.other}`) {
    c.otherSeen = await composerTag(c.page);
  }
  return { ...c.seen, running: await isRunning(c), viewing, other: c.otherSeen };
}

// A paste event carrying data, as Cmd+V delivers one to the textarea.
async function paste(c: Ctx, data: { text?: string; png?: string }): Promise<void> {
  await composer(c).focus();
  await composer(c).evaluate((el, d) => {
    const dt = new DataTransfer();
    if (d.text) dt.setData('text/plain', d.text);
    if (d.png) dt.items.add(new File([Uint8Array.from(atob(d.png), (ch) => ch.charCodeAt(0))], 'shot.png', { type: 'image/png' }));
    el.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }));
  }, data);
}

// A file dragged from Finder onto the composer.
async function drop(c: Ctx): Promise<void> {
  await composer(c).evaluate((el) => {
    const dt = new DataTransfer();
    dt.items.add(new File(['notes\n'], 'notes.txt', { type: 'text/plain' }));
    el.dispatchEvent(new DragEvent('drop', { dataTransfer: dt, bubbles: true, cancelable: true }));
  });
}

async function held(c: Ctx): Promise<void> {
  const deadline = Date.now() + 10_000;
  while (!c.upload) {
    if (Date.now() > deadline) throw new Error('no upload reached the network');
    await new Promise((r) => setTimeout(r, 20));
  }
}

// An upload settling while its thread is not on screen: what the page
// will show on Return (checked there).
function settledAway(c: Ctx, tag: Tag): void {
  if (c.seen.tag === 'up_image' || c.seen.tag === 'up_file') c.seen = { ...c.seen, tag, can_send: true };
}

async function settle(c: Ctx, ok: boolean): Promise<void> {
  const up = c.upload!;
  c.upload = null;
  const away = !(await c.page.evaluate(() => window.location.hash)).endsWith(c.id);
  const kind: Tag = up.file ? 'file' : 'image';
  const req = up.route.request();
  if (!ok) c.refused++;
  if (ok) await up.route.continue();
  else if (up.file) await up.route.continue({ url: req.url().replace(`/api/sessions/${c.id}/`, '/api/sessions/no-such-session/') });
  else await up.route.continue({ headers: { ...req.headers(), 'content-type': 'text/plain' } });
  if (away) settledAway(c, ok ? kind : 'failed');
}

// Send and the Queue button end the draft the same way; a Send while
// nothing runs starts a turn, which the model holds until Finish.
async function send(c: Ctx): Promise<void> {
  if (c.held) { await sendButton(c).click(); return; }
  const name = `t${String(++c.turn).padStart(4, '0')}`;
  const dir = controlDir(c.serve.home);
  queue(dir, name, { mode: 'block', text: `finished ${name}` });
  await sendButton(c).click();
  c.held = name;
  await waitTaken(dir, name);
}

modelTests<Ctx>({
  spec: 'attachments_paste',
  role: 'Composer#0',
  config: CONTROL_CONFIG,

  async init(page, serve) {
    const c: Ctx = {
      page, serve, id: await serve.newSession(), other: await serve.newSession(), turn: 0, held: '', upload: null,
      seen: { tag: 'none', can_send: false, sent: 'none', served: 'none' }, otherSeen: 'none', refused: 0,
    };
    // Chromium logs every 4xx a fetch gets as a console error, so the
    // refusal UploadFail asks for would fail the harness's "no console
    // errors" invariant though the page handled it (it says "not
    // attached"). One such line per refusal, naming an upload endpoint,
    // is let through; anything else still reaches the harness.
    // Page is an EventEmitter at runtime; its type leaves listeners() out.
    const theirs = (page as unknown as EventEmitter).listeners('console') as ((m: ConsoleMessage) => void)[];
    page.removeAllListeners('console');
    page.on('console', (m) => {
      if (c.refused > 0 && m.type() === 'error' && /^Failed to load resource: the server responded with a status of 4\d\d/.test(m.text())
          && UPLOAD_RE.test(m.location().url)) {
        c.refused--;
        return;
      }
      for (const l of theirs) l(m);
    });
    await page.route(UPLOAD_RE, async (route) => {
      if (route.request().method() !== 'POST') return route.continue();
      c.upload = { route, file: route.request().url().includes('/files') };
    });
    await page.goto(`${serve.url}/#/s/${c.id}`);
    await composer(c).waitFor();
    return c;
  },

  actions: {
    PasteLong: (c) => paste(c, { text: LONG }),
    async AttachImage(c) { await paste(c, { png: PNG }); await held(c); },
    async AttachFile(c) { await drop(c); await held(c); },
    UploadOk: (c) => settle(c, true),
    UploadFail: (c) => settle(c, false),
    // Selecting the tag and deleting it.
    async DeleteTag(c) {
      const draft = await composer(c).inputValue();
      await composer(c).fill(draft.replace(TAG_RE, '').trim());
    },
    Send: send,
    Queue: (c) => c.page.getByRole('button', { name: 'Queue', exact: true }).click(),
    async Finish(c) {
      release(controlDir(c.serve.home), c.held);
      c.held = '';
    },
    Edit: (c) => c.page.getByRole('button', { name: 'Edit into composer' }).last().click(),
    Leave: (c) => row(c, c.other).click(),
    Return: (c) => row(c).click(),
  },

  read: readUiState,
  status: (c) => row(c),
  sessions: (c) => [c.id],
  async cleanup(c) {
    if (c.upload) await c.upload.route.continue().catch(() => {});
    if (c.held) release(controlDir(c.serve.home), c.held);
  },
});
