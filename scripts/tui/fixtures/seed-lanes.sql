-- 14-forks.sh's seed, written the way a previous process left it (the `seed-mail.sql`
-- precedent). It held four claim seeds too until the claims demolition (2026-08-30) removed the
-- claims setup; `13-claims.sh` went with it.
--
--   * `traj/fork-of-sol` — a headless branch of `lane/sol` with an ancestor edge and no `agents`
--                          row: §4's fork, and the row the branch picker must label differently
--                          from a lane child (V8).

-- The fork: a trajectory with steps of its own and an ANCESTOR edge back to `lane/sol`, and
-- deliberately NO `agents` row. Promotable later by adding the row and nothing else (§4).
INSERT INTO steps (id, traj_id, seq, at, wake_id, type, class, body, cites, ignorable)
VALUES (
  'seed-fork-step',
  'traj/fork-of-sol',
  1,
  strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
  'wake:seed',
  'thought/text',
  'thought',
  json_object('text', 'SEEDED-FORK-CONTENT: what the other branch thought', 'step_index', 0),
  json_array(),
  0
);

INSERT INTO edges (child_traj, parent_traj, at_seq, kind, at)
SELECT
  'traj/fork-of-sol',
  'lane/sol',
  (SELECT COALESCE(MAX(seq), 1) FROM steps WHERE traj_id = 'lane/sol'),
  'ancestor',
  strftime('%Y-%m-%dT%H:%M:%SZ', 'now');
