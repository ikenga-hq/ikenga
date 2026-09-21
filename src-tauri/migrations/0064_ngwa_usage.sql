-- 0064_ngwa_usage.sql
-- WP-14 (G-NGWA-ITEM / DEC-24), amended in place by WP-14a (DEC-27, DEC-28) —
-- transcript JSONL usage mirror.
--
-- Tables:
--   ngwa_transcript_files - per-file watermark for the incremental tail scan.
--   ngwa_usage_sessions   - one row per (kind, name, session): DEC-27 counts
--                           distinct sessions, so the session is the unit.
--   ngwa_usage_turns      - one row per (kind, name, assistant message): the
--                           token ledger. Claude Code writes one record per
--                           content block, repeating the message's usage, so
--                           tokens are keyed by message id and upserted with
--                           MAX() - a message split across two scans is still
--                           counted exactly once.
--
-- A session key is a main transcript's `sessionId`, or a subagent run's
-- `agentId`. `source_path` records which transcript file contributed a row so
-- a truncated or rewritten file can have its contributions deleted and be
-- rescanned from byte 0.
--
-- DEC-28: this file was amended in place before it ever merged. A dev profile
-- that applied the earlier 0064 must discard that database: the runner in
-- db.rs records applied migrations by id only, so it never re-runs this
-- amended file there, and the scanner would find none of the tables above.
--
-- Conventions (ikenga.db):
--   - No FK constraints (soft links).
--   - One statement per ';' (split by runner).
--   - Only '--' line comments.

CREATE TABLE IF NOT EXISTS ngwa_transcript_files (
  path          TEXT PRIMARY KEY,
  mtime_ms      INTEGER NOT NULL,
  byte_offset   INTEGER NOT NULL,
  head_len      INTEGER NOT NULL,
  head_hash     TEXT NOT NULL,
  scanned_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS ngwa_usage_sessions (
  kind          TEXT NOT NULL,
  name          TEXT NOT NULL,
  session_key   TEXT NOT NULL,
  first_used_ms INTEGER NOT NULL,
  last_used_ms  INTEGER NOT NULL,
  source_path   TEXT NOT NULL,
  UNIQUE (kind, name, session_key)
);

CREATE INDEX IF NOT EXISTS idx_ngwa_usage_sessions_source
  ON ngwa_usage_sessions (source_path);

CREATE TABLE IF NOT EXISTS ngwa_usage_turns (
  kind         TEXT NOT NULL,
  name         TEXT NOT NULL,
  message_id   TEXT NOT NULL,
  session_key  TEXT NOT NULL,
  timestamp_ms INTEGER NOT NULL,
  tokens       INTEGER NOT NULL,
  source_path  TEXT NOT NULL,
  UNIQUE (kind, name, message_id)
);

CREATE INDEX IF NOT EXISTS idx_ngwa_usage_turns_source
  ON ngwa_usage_turns (source_path);

CREATE INDEX IF NOT EXISTS idx_ngwa_usage_turns_time
  ON ngwa_usage_turns (timestamp_ms);
