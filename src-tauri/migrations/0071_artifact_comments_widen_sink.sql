-- 0071_artifact_comments_widen_sink.sql
-- Widen artifact_comments.sink CHECK constraint to include 'clipboard' and 'chi' (WP-S).
--
-- SQLite requires a table rebuild to alter a CHECK constraint.
-- Preserves existing rows, columns (including 0070 author_principal_id), and indexes.

CREATE TABLE artifact_comments_new (
  id                   INTEGER PRIMARY KEY AUTOINCREMENT,
  artifact_path        TEXT    NOT NULL,
  selector             TEXT    NOT NULL,
  text                 TEXT    NOT NULL,
  screenshot_path      TEXT,
  status               TEXT    NOT NULL DEFAULT 'open'
                         CHECK (status IN ('open', 'in_progress', 'resolved', 'stale')),
  position_x           REAL,
  position_y           REAL,
  thread_id            TEXT,
  opening_session_id   TEXT,
  sink                 TEXT
                         CHECK (sink IS NULL OR sink IN ('terminal', 'sidepane', 'both', 'clipboard', 'chi')),
  created_at           INTEGER NOT NULL,
  acknowledged_at      INTEGER,
  resolved_at          INTEGER,
  author_principal_id  TEXT
);

INSERT INTO artifact_comments_new (
  id, artifact_path, selector, text, screenshot_path, status,
  position_x, position_y, thread_id, opening_session_id, sink,
  created_at, acknowledged_at, resolved_at, author_principal_id
)
SELECT
  id, artifact_path, selector, text, screenshot_path, status,
  position_x, position_y, thread_id, opening_session_id, sink,
  created_at, acknowledged_at, resolved_at, author_principal_id
FROM artifact_comments;

DROP TABLE artifact_comments;

ALTER TABLE artifact_comments_new RENAME TO artifact_comments;

CREATE INDEX IF NOT EXISTS idx_artifact_comments_path_status
  ON artifact_comments(artifact_path, status);

CREATE INDEX IF NOT EXISTS idx_artifact_comments_status_created
  ON artifact_comments(status, created_at DESC);
