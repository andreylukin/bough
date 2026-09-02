import { defineConfig } from '@playwright/test';
import * as os from 'os';
import * as path from 'path';

// Every spec launches its own bough process (temp HOME, temp cwd, own
// port), so full parallelism is safe and encouraged.
export default defineConfig({
  testDir: './specs',
  fullyParallel: true,
  workers: os.cpus().length,
  retries: process.env.CI ? 1 : 0,
  forbidOnly: !!process.env.CI,
  timeout: 30_000,
  expect: { timeout: 10_000 },
  use: {
    trace: 'on-first-retry',
    viewport: { width: 1100, height: 700 },
  },
  reporter: process.env.CI
    ? [['list'], ['github']]
    : [['list']],
  globalSetup: path.join(__dirname, 'helpers', 'global-setup.ts'),
});
