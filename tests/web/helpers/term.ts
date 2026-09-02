// Terminal-screen helpers for the sip web client.
//
// sip's client exposes window.sipTerm (see sip's static/terminal.js):
//   sipTerm.connected            — websocket/webtransport is up
//   sipTerm.term.buffer.active   — xterm.js-compatible buffer API
//   sipTerm.sendInput(str)       — raw input into the PTY
// termText reads the BUFFER (translateToString per row), not the DOM:
// the renderer draws to canvas/WebGL, so there are no DOM rows to read.
import { Page, expect } from '@playwright/test';

declare global {
  interface Window {
    sipTerm: any;
  }
}

/** Navigate and wait until the terminal client is connected. */
export async function boot(page: Page, url: string): Promise<void> {
  await page.goto(url);
  await page.waitForFunction(() => (window as any).sipTerm?.connected, null, {
    timeout: 15_000,
  });
}

/** Everything currently in the terminal buffer, as text (rows joined by \n). */
export function termText(page: Page): Promise<string> {
  return page.evaluate(() => {
    const b = (window as any).sipTerm.term.buffer.active;
    const rows: string[] = [];
    for (let i = 0; i < b.length; i++) rows.push(b.getLine(i).translateToString(true));
    return rows.join('\n');
  });
}

/** Wait until substr appears somewhere on the terminal screen. */
export async function waitForTermText(
  page: Page,
  substr: string,
  timeoutMs = 10_000,
): Promise<void> {
  try {
    await page.waitForFunction(
      (m: string) => {
        const b = (window as any).sipTerm.term.buffer.active;
        for (let i = 0; i < b.length; i++) {
          if (b.getLine(i).translateToString(true).includes(m)) return true;
        }
        return false;
      },
      substr,
      { timeout: timeoutMs },
    );
  } catch (e) {
    const screen = await termText(page).catch(() => '(unreadable)');
    throw new Error(`"${substr}" never appeared on the terminal screen.\n--- screen ---\n${screen}\n--------------\n${e}`);
  }
}

/** Focus the terminal and type s through real key events. Focus goes
 * through the client's own focus() — the app has mouse reporting on
 * (clicks toggle transcript blocks), so a synthetic focus-click could
 * toggle whatever block happens to sit under the cursor. */
export async function typeInTerm(page: Page, s: string): Promise<void> {
  await page.evaluate(() => (window as any).sipTerm.webterm.focus());
  await page.keyboard.type(s);
}

/** Type a prompt line and press Enter. */
export async function say(page: Page, line: string): Promise<void> {
  await typeInTerm(page, line);
  await page.keyboard.press('Enter');
}

/** Type a prompt and wait for the reply marker on screen. */
export async function ask(page: Page, line: string, expectOnScreen: string): Promise<void> {
  await say(page, line);
  await waitForTermText(page, expectOnScreen);
}

/** Assert substr is nowhere on the screen right now. */
export async function expectNotOnScreen(page: Page, substr: string): Promise<void> {
  expect(await termText(page)).not.toContain(substr);
}

/** First buffer row whose text contains substr, or -1. */
export async function findRow(page: Page, substr: string): Promise<number> {
  return page.evaluate((m: string) => {
    const b = (window as any).sipTerm.term.buffer.active;
    for (let i = 0; i < b.length; i++) {
      if (b.getLine(i).translateToString(true).includes(m)) return i;
    }
    return -1;
  }, substr);
}

/** The terminal's cell geometry in CSS pixels, from xterm's own
 * measured dimensions plus the on-page screen rect. */
export async function cellMetrics(page: Page): Promise<{
  left: number; top: number; cellWidth: number; cellHeight: number; cols: number; rows: number;
}> {
  return page.evaluate(() => {
    const term = (window as any).sipTerm.term;
    const dims = term._core._renderService.dimensions.css.cell;
    const screen = term.element.querySelector('.xterm-screen');
    const r = screen.getBoundingClientRect();
    return {
      left: r.left, top: r.top,
      cellWidth: dims.width, cellHeight: dims.height,
      cols: term.cols, rows: term.rows,
    };
  });
}

/** Real-mouse click on cell (row, col), coords from xterm metrics. */
export async function clickCell(page: Page, row: number, col: number): Promise<void> {
  const m = await cellMetrics(page);
  await page.mouse.click(
    m.left + (col + 0.5) * m.cellWidth,
    m.top + (row + 0.5) * m.cellHeight,
  );
}
