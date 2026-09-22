-- 0065_ngwa_usage_ownership.sql
-- WP-14b (DEC-29) — count a resumed or forked session only for what it did
-- itself.
--
-- When Claude Code resumes a session it copies the prior history into a new
-- file under a new `sessionId`. The copied assistant records keep their API
-- `message.id` (and their timestamps), so the same message is held by two
-- sessions. DEC-29: a message is owned by exactly one of the sessions holding
-- it - the one with the earliest start, ties broken by the smallest session
-- key - and a session counts an item only through messages it owns.
--
-- Ownership depends on other files, so it is resolved when the mirror is
-- aggregated, never when a file is scanned. That needs the mirror to keep
-- message-level rows (which session held which attributed message) and each
-- session's start, which 0064's per-(kind, name, session) rows cannot express.
--
-- Tables:
--   ngwa_usage_messages       - one row per (kind, name, session, message):
--                               the attributed messages each session holds.
--                               Replaces ngwa_usage_sessions as the source of
--                               session counts.
--   ngwa_usage_session_starts - the earliest record timestamp each file shows
--                               for a session. A session's start is the MIN
--                               over its rows.
--
-- Existing 0064 databases. Their ngwa_usage_sessions rows carry no message
-- key, so they cannot be re-attributed. This migration empties that retired
-- table and deletes every transcript watermark, so the next scan re-reads the
-- whole corpus from byte 0 into the new tables. ngwa_usage_turns is left as it
-- is: it is keyed by message already and a re-read upserts it idempotently.
-- ngwa_usage_sessions is kept (empty, no longer written or read) rather than
-- dropped, so 0064's schema and its migration test stay as they were.
--
-- Conventions (ikenga.db):
--   - No FK constraints (soft links).
--   - One statement per ';' (split by runner).
--   - Only '--' line comments.

CREATE TABLE IF NOT EXISTS ngwa_usage_messages (
  kind         TEXT NOT NULL,
  name         TEXT NOT NULL,
  session_key  TEXT NOT NULL,
  message_id   TEXT NOT NULL,
  timestamp_ms INTEGER NOT NULL,
  source_path  TEXT NOT NULL,
  UNIQUE (kind, name, session_key, message_id)
);

CREATE INDEX IF NOT EXISTS idx_ngwa_usage_messages_source
  ON ngwa_usage_messages (source_path);

CREATE TABLE IF NOT EXISTS ngwa_usage_session_starts (
  session_key TEXT NOT NULL,
  source_path TEXT NOT NULL,
  start_ms    INTEGER NOT NULL,
  UNIQUE (session_key, source_path)
);

CREATE INDEX IF NOT EXISTS idx_ngwa_usage_session_starts_source
  ON ngwa_usage_session_starts (source_path);

DELETE FROM ngwa_usage_sessions;

DELETE FROM ngwa_transcript_files;
