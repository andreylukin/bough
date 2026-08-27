-- 13-claims.sh's and 14-forks.sh's seed, written the way a previous process left it (the
-- `seed-mail.sql` precedent).
--
-- Two claims and one fork, planted directly:
--
--   * `SEED-CLAIM-REQ`  — a `claim/proposed` of kind `requirement`. Accepting it must append a
--                         `pin/set` (§3: "accepted requirements are pins").
--   * `SEED-CLAIM-LANE` — a `claim/proposed` of kind `lane`. Accepting it must BIRTH an `agents`
--                         row and a rail beside the others (V2).
--   * `SEED-CLAIM-REJ`  — a third, for the rejection bullet, so rejecting does not consume one of
--                         the two above.
--   * `SEED-CLAIM-EDIT` — a fourth, for the edit bullet: editing a claim that was already
--                         ACCEPTED is refused, so the edit needs an open card of its own.
--   * `traj/fork-of-sol` — a headless branch of `lane/sol` with an ancestor edge and no `agents`
--                         row: §4's fork, and the row the branch picker must label differently
--                         from a lane child (V8).
--
-- Raw SQL on purpose: a claim reaches the ledger through a MODEL turn calling `propose_claim`,
-- and driving one would make these scripts about the loop instead of about the cards, the pins
-- and the picker.
--
-- Every body carries `by`: `claims::query::as_claim` reads the PROPOSER from it, and a claim
-- without one is `<unknown>` — which the graph seam then refuses with "no agent named
-- `<unknown>`" the moment a lane claim is accepted.

INSERT INTO steps (id, traj_id, seq, at, wake_id, type, class, body, cites, ignorable)
SELECT
  'seed-claim-req',
  'lane/sol',
  (SELECT COALESCE(MAX(seq), 0) + 1 FROM steps WHERE traj_id = 'lane/sol'),
  strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
  'wake:seed',
  'claim/proposed',
  'thought',
  json_object(
    'by', 'sol',
    'claim', 'SEED-CLAIM-REQ',
    'kind', 'requirement',
    'title', 'SEEDED-REQUIREMENT',
    'body', 'the ledger is the only authority on what happened',
    'detail', json_object('kind', 'requirement', 'supersedes', json_array())
  ),
  json_array(),
  0;

INSERT INTO steps (id, traj_id, seq, at, wake_id, type, class, body, cites, ignorable)
SELECT
  'seed-claim-lane',
  'lane/sol',
  (SELECT COALESCE(MAX(seq), 0) + 1 FROM steps WHERE traj_id = 'lane/sol'),
  strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
  'wake:seed',
  'claim/proposed',
  'thought',
  json_object(
    'by', 'sol',
    'claim', 'SEED-CLAIM-LANE',
    'kind', 'lane',
    'title', 'SEEDED-LANE',
    'body', 'the release work deserves a lane of its own',
    'detail', json_object(
      'kind', 'lane',
      'name', 'vega',
      'from_seq', NULL,
      'routing_refs', json_array('repo:vega'),
      'wake_classes', json_array()
    )
  ),
  json_array(),
  0;

INSERT INTO steps (id, traj_id, seq, at, wake_id, type, class, body, cites, ignorable)
SELECT
  'seed-claim-rej',
  'lane/sol',
  (SELECT COALESCE(MAX(seq), 0) + 1 FROM steps WHERE traj_id = 'lane/sol'),
  strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
  'wake:seed',
  'claim/proposed',
  'thought',
  json_object(
    'by', 'sol',
    'claim', 'SEED-CLAIM-REJ',
    'kind', 'other',
    'title', 'SEEDED-DOUBTFUL',
    'body', 'every tool call should be approved by hand',
    'detail', json_object('kind', 'other')
  ),
  json_array(),
  0;

INSERT INTO steps (id, traj_id, seq, at, wake_id, type, class, body, cites, ignorable)
SELECT
  'seed-claim-edit',
  'lane/sol',
  (SELECT COALESCE(MAX(seq), 0) + 1 FROM steps WHERE traj_id = 'lane/sol'),
  strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
  'wake:seed',
  'claim/proposed',
  'thought',
  json_object(
    'by', 'sol',
    'claim', 'SEED-CLAIM-EDIT',
    'kind', 'requirement',
    'title', 'SEEDED-EDITABLE',
    'body', 'citations are optional',
    'detail', json_object('kind', 'requirement', 'supersedes', json_array())
  ),
  json_array(),
  0;

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
