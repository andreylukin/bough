// history: JSONL session files under $HOME/.bough/history, the
// `bough log` CLI over them, and the in-UI inspector overlay.
import * as fs from 'fs';
import * as path from 'path';
import { test, expect } from '../helpers/fixtures';
import { ask, boot, termText, waitForTermText } from '../helpers/term';

function historyFiles(home: string): string[] {
  const dir = path.join(home, '.bough', 'history');
  try {
    return fs.readdirSync(dir).filter((f) => f.endsWith('.jsonl')).map((f) => path.join(dir, f));
  } catch {
    return [];
  }
}

test('a turn lands in the JSONL history file', async ({ launchBough, page }) => {
  const b = await launchBough();
  await boot(page, b.url);
  await ask(page, 'remember this turn', 'echo: remember this turn');

  const files = historyFiles(b.home);
  expect(files.length).toBe(1);
  const lines = fs.readFileSync(files[0], 'utf8').trim().split('\n').map((l) => JSON.parse(l));
  const kinds = lines.map((l) => l.kind);
  expect(kinds).toContain('input');
  expect(kinds).toContain('assistant');
  expect(kinds).toContain('done');
  const input = lines.find((l) => l.kind === 'input');
  expect(input.data.text).toBe('remember this turn');
  const assistant = lines.find((l) => l.kind === 'assistant');
  expect(assistant.data.text).toBe('echo: remember this turn');
});

test("'bough log' lists the turn (exec in the temp HOME)", async ({ launchBough, page }) => {
  const b = await launchBough();
  await boot(page, b.url);
  await ask(page, 'log me please', 'echo: log me please');

  const out = b.cli(['log']);
  expect(out).toContain('input');
  expect(out).toContain('log me please');
  expect(out).toContain('assistant');
  expect(out).toContain('echo: log me please');
});

// The inspector overlay. NOTE: the task brief says ctrl+h, but the
// shipped default keymap binds history_inspect to ctrl+o (ctrl+h is
// backspace in legacy terminals — documented in theme.go). This spec
// asserts the actual default.
test('ctrl+o toggles the history inspector overlay', async ({ launchBough, page }) => {
  const b = await launchBough();
  await boot(page, b.url);
  await ask(page, 'inspect me', 'echo: inspect me');

  await page.keyboard.press('Control+o');
  await waitForTermText(page, 'inspecting');
  const screen = await termText(page);
  expect(screen).toContain('history'); // overlay header
  expect(screen).toContain('.bough/history'); // session file path
  expect(screen).toContain('input');
  expect(screen).toContain('assistant');

  await page.keyboard.press('Control+o');
  await page.waitForFunction(() => {
    const buf = (window as any).sipTerm.term.buffer.active;
    for (let i = 0; i < buf.length; i++) {
      if (buf.getLine(i).translateToString(true).includes('inspecting')) return false;
    }
    return true;
  }, null, { timeout: 5_000 });
  // Transcript is back.
  expect(await termText(page)).toContain('echo: inspect me');
});
