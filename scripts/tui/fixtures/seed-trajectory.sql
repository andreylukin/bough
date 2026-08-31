-- 10-memory.sh's seed: a LIVED DAY on `lane/sol`, written the way a previous process left it.
--
-- Raw SQL on purpose (the `seed-mail.sql` precedent): Phase 4's commands govern a trajectory that
-- already exists, and driving 84 real steps through the model to create one would make the suite
-- about the loop instead of about `/seal`, `/reconsolidate`, `/drift` and `/reset`.
--
-- The shape is what the governance rows need to have anything to say:
--   * 60 `thought/text` steps, one minute apart, with DELIBERATELY uneven lengths — drift-watch's
--     thought-length variance is a statistic, and a seed of identical thoughts would report a
--     variance of zero and prove nothing;
--   * 24 `tool/call` steps over three tool names in an uneven 12/8/4 split, so the tool-use
--     distribution and its entropy are neither empty nor uniform;
--   * one minute between steps, which is far below the row's `gap_minutes: 45`, so the whole day
--     is ONE episode and the windowing is the `max_window_steps` arithmetic alone.
--
-- 84 steps with `seal_lag_steps: 20` leaves 64 sealable ones: enough for several tier-1 blocks.

-- The lane already carries the resident's own boot steps, so every seeded seq is an OFFSET from
-- the current head — a fixed 1..84 would collide on `UNIQUE (traj_id, seq)` and seed nothing.
WITH RECURSIVE base(b) AS (SELECT COALESCE(MAX(seq), 0) FROM steps WHERE traj_id = 'lane/sol'),
     n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 59)
INSERT INTO steps (id, traj_id, seq, at, wake_id, type, class, body, cites, ignorable)
SELECT
  'seed-thought-' || i,
  'lane/sol',
  (SELECT b FROM base) + i + 1,
  strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-' || (84 - i) || ' minutes'),
  'wake:seed-' || (i / 10),
  'thought/text',
  'thought',
  json_object(
    'text', 'SEEDED-DAY step ' || i || ': ' || substr(
      'the collector reported a change and I read the diff before deciding what to do about it',
      1, 12 + ((i * 7) % 60)),
    'step_index', i
  ),
  json_array(),
  0
FROM n;

WITH RECURSIVE base(b) AS (SELECT COALESCE(MAX(seq), 0) FROM steps WHERE traj_id = 'lane/sol'),
     n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 23)
INSERT INTO steps (id, traj_id, seq, at, wake_id, type, class, body, cites, ignorable)
SELECT
  'seed-toolcall-' || i,
  'lane/sol',
  (SELECT b FROM base) + i + 1,
  strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-' || (24 - i) || ' minutes'),
  'wake:seed-6',
  'tool/call',
  'thought',
  json_object(
    'call', 'seed-call-' || i,
    'name', CASE WHEN i < 12 THEN 'read_file' WHEN i < 20 THEN 'write_file' ELSE 'run' END,
    'args', json_object('path', 'notes/seed-' || i || '.md'),
    'render', 'generic',
    'step_index', i
  ),
  json_array(),
  0
FROM n;
