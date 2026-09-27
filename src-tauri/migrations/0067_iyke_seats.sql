-- 0067_iyke_seats.sql — Chi seats (G-SEATS §1.4, WP-65).
--
-- A seat is its own table, not an iyke_agents row (G-91): it needs a project,
-- a name unique per project, a session pointer and hold columns. Its inbox
-- reuses iyke_agents / iyke_agent_inbox additively, keyed by the seat id.
-- Status (vacant / live / idle / run) is derived at read time, never stored.

CREATE TABLE IF NOT EXISTS iyke_seats (
  id               TEXT PRIMARY KEY,               -- uuid v4, immutable
  project_id       TEXT NOT NULL,                  -- projects.id, checked at create, no FK (§3.3)
  name             TEXT NOT NULL,                  -- §1.2
  engine_id        TEXT NOT NULL,                  -- Chi engine id (§6.1)
  session_kind     TEXT CHECK (session_kind IN ('run', 'terminal')),
  session_ref      TEXT,                           -- run_id or terminal_id
  external_id      TEXT,                           -- engine-native resume id, when known
  session_cwd      TEXT,                           -- the directory the session ran in
  hold_client      TEXT,
  hold_since       INTEGER,
  hold_expires_at  INTEGER,
  displaced_client TEXT,                           -- §5.3, told once, on its next call
  displaced_by     TEXT,
  displaced_at     INTEGER,
  created_at       INTEGER NOT NULL,
  last_active_at   INTEGER NOT NULL,
  CHECK ((session_kind IS NULL) = (session_ref IS NULL)),
  CHECK ((hold_client IS NULL) = (hold_since IS NULL) AND (hold_client IS NULL) = (hold_expires_at IS NULL)),
  UNIQUE (project_id, name)
);

-- DEC-69c, enforced by the database: one session, at most one seat, across all projects.
CREATE UNIQUE INDEX IF NOT EXISTS iyke_seats_one_seat_per_session
  ON iyke_seats (session_kind, session_ref) WHERE session_ref IS NOT NULL;

-- The same engine conversation reached through two refs (a terminal and a run) is one session.
-- engine_id equals the session's engine: a move refuses a session on another engine.
CREATE UNIQUE INDEX IF NOT EXISTS iyke_seats_one_seat_per_conversation
  ON iyke_seats (engine_id, external_id) WHERE external_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS iyke_seats_project ON iyke_seats (project_id, name);
