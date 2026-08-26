-- 06-catch-up.sh's seed: one piece of ORDINARY mail queued on `sol`'s lane, written the way
-- `Agent::deliver` writes it (P3-D15) — the `mail/delivered` EVIDENCE step first, then the
-- `inbox/spliced { op: insert }` that carries the message payload `Inbox::rebuild` folds.
--
-- It is raw SQL on purpose: the point of V6 is that the mail was queued by a PREVIOUS process and
-- survived the shutdown, which is exactly what a restart-time seed is.
INSERT INTO steps (id, traj_id, seq, at, wake_id, type, class, body, cites, ignorable)
SELECT
  'seed-mail-delivered',
  'lane/sol',
  (SELECT COALESCE(MAX(seq), 0) + 1 FROM steps WHERE traj_id = 'lane/sol'),
  strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
  'wake:seed',
  'mail/delivered',
  'evidence',
  json_object(
    'class', 'ordinary',
    'from',  'collector:seed',
    'subject', 'SEEDED-CATCH-UP-MAIL',
    'summary', 'one piece of mail queued before the process started',
    'refs', json_array('seed:mail:1')
  ),
  json_array(json_object('ref', 'seed:mail:1')),
  0;

INSERT INTO steps (id, traj_id, seq, at, wake_id, type, class, body, cites, ignorable)
SELECT
  'seed-inbox-spliced',
  'lane/sol',
  (SELECT COALESCE(MAX(seq), 0) + 1 FROM steps WHERE traj_id = 'lane/sol'),
  strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
  'wake:seed',
  'inbox/spliced',
  'evidence',
  json_object(
    'message', 'seed-message-1',
    'op', 'insert',
    'target', 'next_wake',
    -- json('false') and not 0: `InboxSpliced.wake` is a BOOL, and serde refuses the integer, so
    -- `Inbox::rebuild` skipped this splice entirely and the catch-up wake never had anything
    -- queued to wake for.
    'wake', json('false'),
    'payload', json_object(
      'id', 'seed-message-1',
      'from', json_object('kind', 'collector', 'name', 'seed'),
      'class', 'ordinary',
      'text', 'SEEDED-CATCH-UP-MAIL: a planted piece of queued mail',
      'subject', 'SEEDED-CATCH-UP-MAIL',
      'cites', json_array(),
      'refs', json_array(),
      'mail_seq', NULL,
      'at', strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    )
  ),
  json_array(),
  0;
