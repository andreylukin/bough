import { defineConfig } from '@playwright/test';
import * as os from 'os';
import * as path from 'path';

// The spec files whose walks run longest, the longest single walk first
// (measured on a full --workers=3 run, 2026-09-24): session_create's one
// walk takes ~50 s, a ui_changes walk ~40 s, while orb_lifecycle is the
// most walk-seconds in all (23 walks, ~400 s). Playwright queues tests
// project by project and file by file in name order, so each of these is
// a project ahead of the rest; a long walk queued last is a tail with
// the other workers idle. Re-measure when a flow's walks change.
const heavy = [
  'session_create',
  'ui_changes',
  'ui_hooks',
  'ui_projects',
  'orb_lifecycle',
  'ui_wiki',
  'ui_palette',
  'live-transcript-sync',
  'project_main_threads',
  'unseen-trouble-ack',
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
