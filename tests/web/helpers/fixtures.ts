// Per-test fixture: launchBough spawns isolated bough instances and
// guarantees they are killed at test end; on failure the process
// output is attached so the report carries the full server log.
import { test as base } from '@playwright/test';
import { Bough, LaunchOpts, launch } from './bough';

interface Fixtures {
  launchBough: (opts?: LaunchOpts) => Promise<Bough>;
}

export const test = base.extend<Fixtures>({
  launchBough: async ({}, use, testInfo) => {
    const started: Bough[] = [];
    await use(async (opts?: LaunchOpts) => {
      const b = await launch(opts);
      started.push(b);
      return b;
    });
    for (const b of started) {
      if (testInfo.status !== testInfo.expectedStatus) {
        await testInfo.attach('bough-output', {
          body: b.output() || '(no output)',
          contentType: 'text/plain',
        });
      }
      await b.kill();
    }
  },
});

export { expect } from '@playwright/test';
