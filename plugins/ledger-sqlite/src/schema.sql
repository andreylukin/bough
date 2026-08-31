-- The §3 ledger schema (phase plan §2.8). Append-only is enforced BELOW the Rust API, by the
-- triggers at the bottom of this file: a raw connection cannot get around them (V1).
-- `agents` carries no triggers at all, on purpose: §3 exempts it as mutable config.

CREATE TABLE IF NOT EXISTS steps (
  id       TEXT PRIMARY KEY,
  traj_id  TEXT NOT NULL,
  seq      INTEGER NOT NULL,
  at       TEXT NOT NULL,
  wake_id  TEXT NOT NULL,
  type     TEXT NOT NULL,
  class    TEXT NOT NULL CHECK (class IN ('evidence','thought')),
  body     TEXT NOT NULL,
  cites    TEXT NOT NULL,
  ignorable INTEGER NOT NULL DEFAULT 0,
  UNIQUE (traj_id, seq)
);
CREATE INDEX IF NOT EXISTS idx_steps_traj_seq ON steps(traj_id, seq);
CREATE INDEX IF NOT EXISTS idx_steps_type     ON steps(type, traj_id, seq);
CREATE INDEX IF NOT EXISTS idx_steps_wake     ON steps(wake_id, seq);

CREATE TABLE IF NOT EXISTS step_refs (
  step_id TEXT NOT NULL REFERENCES steps(id),
  ref     TEXT NOT NULL,
  PRIMARY KEY (step_id, ref)
);
CREATE INDEX IF NOT EXISTS idx_step_refs_ref ON step_refs(ref);

CREATE TABLE IF NOT EXISTS edges (
  child_traj  TEXT NOT NULL,
  parent_traj TEXT NOT NULL,
  at_seq      INTEGER NOT NULL,
  kind        TEXT NOT NULL CHECK (kind IN ('ancestor','merge')),
  at          TEXT NOT NULL,
  PRIMARY KEY (child_traj, parent_traj, kind)
);
CREATE INDEX IF NOT EXISTS idx_edges_parent ON edges(parent_traj);

CREATE TABLE IF NOT EXISTS rollups (
  id            TEXT PRIMARY KEY,
  traj_id       TEXT NOT NULL,
  kind          TEXT NOT NULL,
  tier          INTEGER NOT NULL,
  from_seq      INTEGER NOT NULL,
  to_seq        INTEGER NOT NULL,
  src_trajs     TEXT NOT NULL,
  body          TEXT NOT NULL,
  notable_refs  TEXT NOT NULL,
  prompt_ver    TEXT NOT NULL,
  sealed_at     TEXT NOT NULL,
  superseded_by TEXT
);
CREATE INDEX IF NOT EXISTS idx_rollups_traj_tier ON rollups(traj_id, tier, from_seq);

CREATE TABLE IF NOT EXISTS actions (
  id       TEXT PRIMARY KEY,
  wake_id  TEXT NOT NULL,
  idem_key TEXT NOT NULL UNIQUE,
  kind     TEXT NOT NULL,
  payload  TEXT NOT NULL,
  status   TEXT NOT NULL,
  result   TEXT,
  at       TEXT NOT NULL,
  done_at  TEXT
);

-- MUTABLE CONFIG, explicitly exempt from append-only (§3). No triggers here, on purpose.
CREATE TABLE IF NOT EXISTS agents (
  name             TEXT PRIMARY KEY,
  traj_id          TEXT NOT NULL,
  routing_refs     TEXT NOT NULL,
  wake_classes     TEXT NOT NULL,
  model_override   TEXT,
  tick_floor       INTEGER,
  digest_rollup_id TEXT
);

CREATE VIRTUAL TABLE IF NOT EXISTS steps_fts
  USING fts5(body, cites, content='steps', content_rowid='rowid');

-- External content, INSERT-ONLY: steps is append-only, so the index needs no delete/update hook.
CREATE TRIGGER IF NOT EXISTS steps_fts_ins AFTER INSERT ON steps BEGIN
  INSERT INTO steps_fts(rowid, body, cites) VALUES (new.rowid, new.body, new.cites);
END;

CREATE TRIGGER IF NOT EXISTS steps_no_update BEFORE UPDATE ON steps
  BEGIN SELECT RAISE(ABORT, 'ledger: steps is append-only'); END;
CREATE TRIGGER IF NOT EXISTS steps_no_delete BEFORE DELETE ON steps
  BEGIN SELECT RAISE(ABORT, 'ledger: steps is append-only'); END;

CREATE TRIGGER IF NOT EXISTS step_refs_no_update BEFORE UPDATE ON step_refs
  BEGIN SELECT RAISE(ABORT, 'ledger: step_refs is append-only'); END;
CREATE TRIGGER IF NOT EXISTS step_refs_no_delete BEFORE DELETE ON step_refs
  BEGIN SELECT RAISE(ABORT, 'ledger: step_refs is append-only'); END;

CREATE TRIGGER IF NOT EXISTS edges_no_update BEFORE UPDATE ON edges
  BEGIN SELECT RAISE(ABORT, 'ledger: edges is append-only'); END;
CREATE TRIGGER IF NOT EXISTS edges_no_delete BEFORE DELETE ON edges
  BEGIN SELECT RAISE(ABORT, 'ledger: edges is append-only'); END;

CREATE TRIGGER IF NOT EXISTS rollups_no_delete BEFORE DELETE ON rollups
  BEGIN SELECT RAISE(ABORT, 'ledger: rollups are sealed'); END;

-- The ONE permitted write to a sealed row: NULL -> non-NULL superseded_by, nothing else moving.
CREATE TRIGGER IF NOT EXISTS rollups_seal_once BEFORE UPDATE ON rollups WHEN
     OLD.superseded_by IS NOT NULL OR NEW.superseded_by IS NULL
  OR NEW.id <> OLD.id OR NEW.traj_id <> OLD.traj_id OR NEW.kind <> OLD.kind
  OR NEW.tier <> OLD.tier OR NEW.from_seq <> OLD.from_seq OR NEW.to_seq <> OLD.to_seq
  OR NEW.src_trajs <> OLD.src_trajs OR NEW.body <> OLD.body
  OR NEW.notable_refs <> OLD.notable_refs OR NEW.prompt_ver <> OLD.prompt_ver
  OR NEW.sealed_at <> OLD.sealed_at
  BEGIN SELECT RAISE(ABORT, 'ledger: superseded_by is the one set-once write to a sealed rollup'); END;
