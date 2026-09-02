// context-md: AGENTS.md / BOUGH.md preambles land at the head of the
// system prompt. llm-echo ignores the system prompt, so these specs use
// a JS provider that reflects the system head back as the reply.
// (slice + backtick-strip: reflecting the WHOLE system prompt would
// echo its ```js example and codemode would execute it.)
import { test, expect } from '../helpers/fixtures';
import { ask, boot } from '../helpers/term';

const sysHeadProvider = `
bough.provider("syshead", function (system, messages) {
  return "SYSHEAD::" + system.slice(0, 500).replace(/\\u0060/g, "'");
});
bough.setup({ provider: { default: "syshead" } });
`;

test('a project AGENTS.md marker reaches the system prompt', async ({ launchBough, page }) => {
  const b = await launchBough({
    cwd: {
      'AGENTS.md': 'AGENTS_MD_MARKER_31337 always be kind',
      '.bough/init.js': sysHeadProvider,
    },
  });
  await boot(page, b.url);
  await ask(page, 'anything', 'AGENTS_MD_MARKER_31337');
});

test('~/.bough/BOUGH.md also reaches the system prompt, labeled', async ({ launchBough, page }) => {
  const b = await launchBough({
    home: { '.bough/BOUGH.md': 'BOUGH_MD_MARKER_9922' },
    cwd: { '.bough/init.js': sysHeadProvider },
  });
  await boot(page, b.url);
  await ask(page, 'anything', 'BOUGH_MD_MARKER_9922');
  // The preamble labels each file with its path.
  await ask(page, 'again', '# Context:');
});
