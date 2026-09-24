import { defineConfig } from '@playwright/test';
import * as os from 'os';
import * as path from 'path';

// The longest spec files, longest first: the model walks, where one file
// is most of the run (orb_lifecycle alone has ~1,700 paths). Playwright
// queues tests project by project and file by file in name order, so
// each of these is a project ahead of the rest; a heavy file queued last
// is a tail with most workers idle.
const heavy = [
  'orb_lifecycle',
  'live-transcript-sync',
  'unseen-trouble-ack',
  'steer-queue',
  'project_main_threads',
  'turn-lifecycle',
  'model-effort-controls',
  'orb-image-build',
  'changes-review',
];
const file = (name: string) => new RegExp(`/specs/model/${name}\\.spec\\.ts$`);

// Every spec launches its own bough process (temp HOME, temp cwd, own
// port) or works only in sessions it made on its worker's serve, so full
// parallelism is safe and encouraged.
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
  projects: [
    ...heavy.map((name) => ({ name, testMatch: file(name) })),
    { name: 'rest', testIgnore: heavy.map(file) },
  ],
  // CI shards write blob reports; the web-e2e-report job merges them.
  reporter: process.env.CI
    ? [['list'], ['github'], ['blob']]
    : [['list']],
  globalSetup: path.join(__dirname, 'helpers', 'global-setup.ts'),
});
