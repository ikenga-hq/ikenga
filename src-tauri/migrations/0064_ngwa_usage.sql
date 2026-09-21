-- 0064_ngwa_usage.sql
-- WP-14 (G-NGWA-ITEM / DEC-24) — Transcript JSONL usage mirror tables.
--
-- Tables:
--   ngwa_transcript_files  - Watermark table for incremental mtime + byte-offset scanning.
--   ngwa_usage_events      - Attributed usage events extracted from transcript records.
--
-- Conventions (ikenga.db):
--   - No FK constraints (soft links).
--   - One statement per ';' (split by runner).
--   - Only '--' line comments.

CREATE TABLE IF NOT EXISTS ngwa_transcript_files (
  path          TEXT PRIMARY KEY,
  mtime_ms      INTEGER NOT NULL,
  byte_offset   INTEGER NOT NULL,
  scanned_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS ngwa_usage_events (
  id                    INTEGER PRIMARY KEY AUTOINCREMENT,
  kind                  TEXT NOT NULL,
  name                  TEXT NOT NULL,
  timestamp_ms          INTEGER NOT NULL,
  input_tokens          INTEGER NOT NULL DEFAULT 0,
  output_tokens         INTEGER NOT NULL DEFAULT 0,
  cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
  cache_read_tokens     INTEGER NOT NULL DEFAULT 0,
  session_id            TEXT
);

CREATE INDEX IF NOT EXISTS idx_ngwa_usage_lookup
  ON ngwa_usage_events (kind, name, timestamp_ms);

CREATE INDEX IF NOT EXISTS idx_ngwa_usage_timestamp
  ON ngwa_usage_events (timestamp_ms);
