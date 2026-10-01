// Same-profile tabs must leave HTTP/1.1 connections available for the
// transcript and prompt requests, even after the stream-owning tab closes.
import type { Page } from '@playwright/test';
import * as fs from 'fs';
import * as path from 'path';
import { freePort } from '../helpers/bough';
import { CONTROL_CONFIG, controlDir, queue, waitTaken } from '../helpers/control';
import { test, expect } from '../helpers/serve';

type ObservedWindow = Window & { __sessionStreams: EventSource[]; __streamLifecycle: string[] };

// Playwright normally keeps every page visible through focus emulation.
// That prevents Chromium from freezing a tab even when CDP asks it to.
// Connect to an isolated browser's default context without those overrides;
// a second CDP session cannot clear the first session's visibility capture.
const lifecycleTest = test.extend({
  context: async ({ playwright, launchOptions }, use, testInfo) => {
    const port = await freePort();
    const server = await playwright.chromium.launchServer({
      ...launchOptions, args: [...(launchOptions.args ?? []), `--remote-debugging-port=${port}`],
    });
    try {
      const browser = await playwright.chromium.connectOverCDP(`http://127.0.0.1:${port}`, { noDefaults: true });
      const context = browser.contexts()[0];
      // Screenshot tracing starts a screencast, another visibility capture.
      await context.tracing.start({ screenshots: false, snapshots: true, sources: true });
      try {
        await use(context);
      } finally {
        const trace = testInfo.outputPath('native-lifecycle-trace.zip');
        const failed = testInfo.status !== testInfo.expectedStatus;
        await context.tracing.stop(failed ? { path: trace } : {});
        if (failed) await testInfo.attach('native-lifecycle-trace', { path: trace, contentType: 'application/zip' });
        await browser.close();
      }
    } finally {
      await server.close();
    }
  },
});
lifecycleTest.use({ trace: 'off' });

// Keep native networking and event delivery: only observe which tab owns
// each connection, so this still exercises Chromium's six-socket limit.
function observeStreams(): void {
  const NativeEventSource = window.EventSource;
  const streams: EventSource[] = [];
  (window as unknown as ObservedWindow).__sessionStreams = streams;
  const lifecycle: string[] = [];
  (window as unknown as ObservedWindow).__streamLifecycle = lifecycle;
  for (const name of ['freeze', 'resume', 'visibilitychange']) {
    document.addEventListener(name, () => lifecycle.push(`${name}:${document.visibilityState}`));
  }
  window.EventSource = class extends NativeEventSource {
    constructor(url: string | URL, init?: EventSourceInit) {
      super(url, init);
      if (/\/api\/sessions\/[^/]+\/events$/.test(new URL(String(url), location.href).pathname)) {
        streams.push(this);
        lifecycle.push('stream:open');
      }
    }
    close() {
      lifecycle.push('stream:close');
      super.close();
    }
  };
}

async function connections(page: Page, id: string): Promise<number> {
  return page.evaluate((session) => (window as unknown as ObservedWindow).__sessionStreams.filter((stream) =>
    new URL(stream.url).pathname === `/api/sessions/${session}/events` &&
    stream.readyState !== EventSource.CLOSED,
  ).length, id);
}

async function totalConnections(pages: Page[], id: string): Promise<number> {
  return (await Promise.all(pages.map((page) => connections(page, id)))).reduce((a, b) => a + b, 0);
}

async function send(page: Page, text: string): Promise<void> {
  await page.locator('#composer').fill(text);
  await page.getByRole('button', { name: 'Send', exact: true }).click();
}

async function revealRunningTool(page: Page): Promise<void> {
  // Live tools start folded inside the turn's Working stretch. Assert
  // the real summary before opening it, just as a person would.
  const work = page.locator('.turn[data-turn] details.work-seg-live');
  const summary = work.locator(':scope > summary');
  await expect(summary.locator('.block-label')).toHaveText('Working');
  if (await work.getAttribute('open') === null) await summary.click();
  await expect(page.locator('.call-native .tool-running')).toBeVisible();
}

for (const sharedTabs of [5, 7]) {
  test(`seven tabs (${sharedTabs} sharing a session) keep updating after the stream owner closes`, async ({ serve, context }) => {
    test.setTimeout(60_000);
    await context.addInitScript(observeStreams);
    const id = await serve.newSession('same-session seed');
    const pages = await Promise.all(Array.from({ length: sharedTabs }, () => context.newPage()));
    // The reported layout was five tabs of one session plus two other
    // sessions; also cover seven copies of just one session.
    const otherIds = await Promise.all(Array.from({ length: 7 - sharedTabs }, (_, i) =>
      serve.newSession(`unrelated session ${i}`),
    ));
    const others = await Promise.all(otherIds.map(async (otherId, i) => {
      const page = await context.newPage();
      await page.goto(`${serve.url}/#/s/${otherId}`);
      await expect(page.locator('.thread')).toContainText(`echo: unrelated session ${i}`);
      return page;
    }));
    await Promise.all(pages.map(async (page) => {
      await page.goto(`${serve.url}/#/s/${id}`);
      await expect(page.locator('.thread')).toContainText('echo: same-session seed');
      await expect(page.locator('#composer')).toBeEnabled();
    }));
    await expect.poll(() => totalConnections(pages, id)).toBe(1);

    // A fresh prompt proves that loading from cached history did not merely
    // hide blocked requests, and that followers receive live stream events.
    await send(pages[pages.length - 1], 'all shared tabs receive this');
    await Promise.all(pages.map((page) =>
      expect(page.locator('.thread')).toContainText('echo: all shared tabs receive this'),
    ));

    for (let i = 0; i < others.length; i++) {
      await expect.poll(() => totalConnections([...pages, ...others], otherIds[i])).toBe(1);
      await send(others[i], `unrelated update ${i}`);
      await expect(others[i].locator('.thread')).toContainText(`echo: unrelated update ${i}`);
      await expect(others[i].locator('.thread')).not.toContainText('all shared tabs receive this');
    }

    const counts = await Promise.all(pages.map((page) => connections(page, id)));
    const owner = pages[counts.indexOf(1)];
    expect(owner).toBeDefined();
    await owner.close();
    const remaining = pages.filter((page) => page !== owner);
    await expect.poll(() => totalConnections(remaining, id)).toBe(1);
    await send(remaining[remaining.length - 1], 'after the owner closes');
    await Promise.all(remaining.map((page) =>
      expect(page.locator('.thread')).toContainText('echo: after the owner closes'),
    ));
    expect(await totalConnections(remaining, id)).toBe(1);
  });
}

test('navigating the stream owner releases its old session without mixing sessions', async ({ serve, context }) => {
  test.setTimeout(60_000);
  await context.addInitScript(observeStreams);
  const first = await serve.newSession('first session seed');
  const second = await serve.newSession('second session seed');
  const pages = await Promise.all([context.newPage(), context.newPage()]);
  await Promise.all(pages.map(async (page) => {
    await page.goto(`${serve.url}/#/s/${first}`);
    await expect(page.locator('.thread')).toContainText('echo: first session seed');
  }));
  await expect.poll(() => totalConnections(pages, first)).toBe(1);
  const counts = await Promise.all(pages.map((page) => connections(page, first)));
  const owner = pages[counts.indexOf(1)];
  const follower = pages.find((page) => page !== owner)!;

  // A hash navigation invokes unsubscribe without destroying the tab;
  // followers must take over while this tab joins a different channel.
  await owner.evaluate((id) => { location.hash = `/s/${id}`; }, second);
  await expect(owner.locator('.thread')).toContainText('echo: second session seed');
  await expect.poll(() => connections(owner, first)).toBe(0);
  await expect.poll(() => connections(follower, first)).toBe(1);
  await expect.poll(() => connections(owner, second)).toBe(1);

  await send(follower, 'only in the first session');
  await expect(follower.locator('.thread')).toContainText('echo: only in the first session');
  await send(owner, 'only in the second session');
  await expect(owner.locator('.thread')).toContainText('echo: only in the second session');
  await expect(owner.locator('.thread')).not.toContainText('only in the first session');
  await expect(follower.locator('.thread')).not.toContainText('only in the second session');
  expect(await totalConnections(pages, first)).toBe(1);
  expect(await totalConnections(pages, second)).toBe(1);
});

lifecycleTest('a frozen owner hands off its stream and resumes as a live follower', async ({ serve, context }) => {
  test.setTimeout(60_000);
  await context.addInitScript(observeStreams);
  const id = await serve.newSession('freeze seed');
  const pages = await Promise.all([context.newPage(), context.newPage(), context.newPage()]);
  await Promise.all(pages.map(async (page) => {
    await page.goto(`${serve.url}/#/s/${id}`);
    await expect(page.locator('.thread')).toContainText('echo: freeze seed');
  }));
  await expect.poll(() => totalConnections(pages, id)).toBe(1);
  const counts = await Promise.all(pages.map((page) => connections(page, id)));
  const owner = pages[counts.indexOf(1)];
  const followers = pages.filter((page) => page !== owner);
  await followers[0].bringToFront();
  const cdp = await context.newCDPSession(owner);
  try {
    // Chromium really suspends this document; a synthetic freeze event
    // would not expose an owner that retained a lock while unable to relay.
    await cdp.send('Page.setWebLifecycleState', { state: 'frozen' });
    await expect.poll(async () => {
      const state = await cdp.send('Runtime.evaluate', {
        expression: 'JSON.stringify(window.__streamLifecycle)', returnByValue: true,
      });
      return JSON.parse(state.result.value);
    }).toContain('freeze:hidden');
    try {
      await expect.poll(() => totalConnections(followers, id)).toBe(1);
    } catch (error) {
      console.log('frozen owner state', await cdp.send('Runtime.evaluate', {
        expression: 'JSON.stringify({ lifecycle: window.__streamLifecycle, visibility: document.visibilityState, streams: window.__sessionStreams.map(s => s.readyState) })',
        returnByValue: true,
      }));
      console.log('follower lock state', await followers[0].evaluate(async () => ({
        lifecycle: (window as unknown as ObservedWindow).__streamLifecycle,
        locks: await navigator.locks.query(),
      })));
      throw error;
    }
    const first = await serve.api.post(`/api/sessions/${id}/prompt`, { data: { text: 'while the owner is frozen' } });
    expect(first.ok()).toBe(true);
    await Promise.all(followers.map((page) =>
      expect(page.locator('.thread')).toContainText('echo: while the owner is frozen'),
    ));

    await cdp.send('Page.setWebLifecycleState', { state: 'active' });
    await expect(owner.locator('.thread')).toContainText('echo: while the owner is frozen');
    await expect.poll(() => connections(owner, id)).toBe(0);
    await expect.poll(() => totalConnections(pages, id)).toBe(1);
    // An external send cannot refresh any tab as a composer side effect.
    // The resumed document must receive a fresh broadcast to render it.
    const second = await serve.api.post(`/api/sessions/${id}/prompt`, { data: { text: 'after the owner resumes' } });
    expect(second.ok()).toBe(true);
    await Promise.all(pages.map((page) =>
      expect(page.locator('.thread')).toContainText('echo: after the owner resumes'),
    ));
    expect(await totalConnections(pages, id)).toBe(1);
  } finally {
    // A failed assertion must not leave a frozen document in fixture cleanup.
    await cdp.send('Page.setWebLifecycleState', { state: 'active' });
    await cdp.detach();
  }
});

test.describe('late subscribers', () => {
  test.use({ serveOpts: { config: CONTROL_CONFIG } });

  test('a late follower replays an in-flight native tool without opening another stream', async ({ serve, context }) => {
    test.setTimeout(60_000);
    await context.addInitScript(observeStreams);
    const id = await serve.newSession();
    const owner = await context.newPage();
    await owner.goto(`${serve.url}/#/s/${id}`);
    await expect.poll(() => connections(owner, id)).toBe(1);
    const dir = controlDir(serve.home);
    const gate = path.join(serve.work, 'release-shared-events-call');
    queue(dir, 'a-call', {
      mode: 'call', tool: 'bash',
      args: { command: `while [ ! -f ${JSON.stringify(gate)} ]; do sleep 0.05; done`, timeout: '60s' },
    });
    queue(dir, 'b-finish', { mode: 'ok', text: 'held command finished' });
    try {
      await send(owner, 'run the held command');
      await waitTaken(dir, 'a-call');
      await revealRunningTool(owner);

      // Native starts are live-only. A fresh transcript read alone cannot
      // recreate this indicator while the tool is still behind its gate.
      const snapshot = await (await serve.api.get(`/api/sessions/${id}`)).json();
      expect(snapshot.entries.some((entry: { kind: string; data?: { phase?: string } }) =>
        entry.kind === 'call' && entry.data?.phase === 'start',
      )).toBe(false);
      const follower = await context.newPage();
      await follower.goto(`${serve.url}/#/s/${id}`);
      await revealRunningTool(follower);
      expect(await connections(owner, id)).toBe(1);
      expect(await connections(follower, id)).toBe(0);
      await expect(owner.locator('.call-native .tool-running')).toHaveCount(1);

      fs.writeFileSync(gate, '');
      await Promise.all([owner, follower].map(async (page) => {
        await expect(page.locator('.thread')).toContainText('held command finished');
        await expect(page.locator('.call-native .tool-running')).toHaveCount(0);
      }));
    } finally {
      fs.writeFileSync(gate, '');
    }
  });
});
