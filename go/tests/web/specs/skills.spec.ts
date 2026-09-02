// skills: mention-triggered SKILL.md injection from ~/.claude/skills
// and ./.claude/skills. With llm-echo the injected block comes back in
// the reply, so injection is directly observable.
import { test, expect } from '../helpers/fixtures';
import { ask, boot, termText } from '../helpers/term';

test('mentioning a HOME-pool skill injects its SKILL.md into the turn', async ({ launchBough, page }) => {
  const b = await launchBough({
    home: { '.claude/skills/frobnicate/SKILL.md': 'FROBNICATE_MARKER_777 use with care' },
  });
  await boot(page, b.url);
  await ask(page, 'please use frobnicate now', 'FROBNICATE_MARKER_777');
  const screen = await termText(page);
  expect(screen).toContain('[skill: frobnicate]');
});

test('mentioning a project-pool skill injects it too', async ({ launchBough, page }) => {
  const b = await launchBough({
    cwd: { '.claude/skills/zorp/SKILL.md': 'ZORP_MARKER_888' },
  });
  await boot(page, b.url);
  await ask(page, 'run zorp for me', 'ZORP_MARKER_888');
});

test('an unmentioned skill is not injected', async ({ launchBough, page }) => {
  const b = await launchBough({
    home: { '.claude/skills/frobnicate/SKILL.md': 'FROBNICATE_MARKER_777' },
  });
  await boot(page, b.url);
  await ask(page, 'nothing relevant here', 'echo: nothing relevant here');
  const screen = await termText(page);
  expect(screen).not.toContain('FROBNICATE_MARKER_777');
  expect(screen).not.toContain('[skill:');
});
