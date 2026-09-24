import { defineConfig } from '@playwright/test';
import * as os from 'os';
import * as path from 'path';

// Every spec launches its own bough process (temp HOME, temp cwd, own
// port), so full parallelism is safe and encouraged.
export default defineConfig({
  testDir: './specs',
  fullyParallel: true,
  // Two per CI runner: each worker drives a bough process and a browser,
  // and a 4-vCPU runner starves at more.
  workers: process.env.CI ? 2 : os.cpus().length,
  retries: process.env.CI ? 1 : 0,
  forbidOnly: !!process.env.CI,
  timeout: 30_000,
  expect: { timeout: 10_000 },
  use: {
    trace: 'on-first-retry',
    video: 'off',
    viewport: { width: 1100, height: 700 },
  },
  // CI shards write blob reports; the web-e2e-report job merges them.
  reporter: process.env.CI
    ? [['list'], ['github'], ['blob']]
    : [['list']],
  globalSetup: path.join(__dirname, 'helpers', 'global-setup.ts'),
});
