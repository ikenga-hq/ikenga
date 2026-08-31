-- 0063_meetings_domain.sql
-- WP-05 (G-STORAGE) — Meetings domain schema for com.ikenga.meetings and mcp-meetings.
--
-- Tables:
--   meetings              - Top-level meeting session records (both local_recording and bot joins)
--   meeting_speakers      - Participant roster with speaker_source attribution discriminator
--   meeting_transcripts   - Word- and segment-level timestamped transcripts
--   meeting_action_items  - Action items extracted from call, syncable to tasks
--   meeting_summaries     - Executive summaries and key decisions
--
-- Conventions (ikenga.db):
--   - No FK constraints (soft links).
--   - One statement per ';' (split by runner).
--   - Only '--' line comments.

CREATE TABLE IF NOT EXISTS meetings (
  id               TEXT PRIMARY KEY,
  title            TEXT NOT NULL,
  platform         TEXT NOT NULL,
  url              TEXT,
  status           TEXT NOT NULL,
  start_time       TEXT NOT NULL,
  end_time         TEXT,
  duration_seconds INTEGER NOT NULL DEFAULT 0,
  video_path       TEXT,
  audio_path       TEXT,
  created_at       TEXT NOT NULL,
  updated_at       TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS meeting_speakers (
  id             TEXT PRIMARY KEY,
  meeting_id     TEXT NOT NULL,
  name           TEXT NOT NULL,
  avatar_url     TEXT,
  contact_id     TEXT,
  speaker_source TEXT NOT NULL DEFAULT 'dom_cue'
);

CREATE TABLE IF NOT EXISTS meeting_transcripts (
  id             TEXT PRIMARY KEY,
  meeting_id     TEXT NOT NULL,
  speaker_id     TEXT,
  speaker_name   TEXT,
  speaker_source TEXT,
  start_ms       INTEGER NOT NULL,
  end_ms         INTEGER NOT NULL,
  text           TEXT NOT NULL,
  confidence     REAL NOT NULL DEFAULT 1.0,
  words_json     TEXT
);

CREATE TABLE IF NOT EXISTS meeting_action_items (
  id         TEXT PRIMARY KEY,
  meeting_id TEXT NOT NULL,
  title      TEXT NOT NULL,
  assignee   TEXT,
  due_date   TEXT,
  status     TEXT NOT NULL DEFAULT 'pending',
  task_id    TEXT
);

CREATE TABLE IF NOT EXISTS meeting_summaries (
  id                 TEXT PRIMARY KEY,
  meeting_id         TEXT NOT NULL,
  executive_summary  TEXT NOT NULL,
  key_decisions_json TEXT NOT NULL DEFAULT '[]',
  topics_json        TEXT NOT NULL DEFAULT '[]',
  created_at         TEXT
);

CREATE INDEX IF NOT EXISTS idx_meetings_status ON meetings(status);

CREATE INDEX IF NOT EXISTS idx_meetings_created_at ON meetings(created_at);

CREATE INDEX IF NOT EXISTS idx_meeting_speakers_meeting_id ON meeting_speakers(meeting_id);

CREATE INDEX IF NOT EXISTS idx_meeting_transcripts_meeting_id ON meeting_transcripts(meeting_id);

CREATE INDEX IF NOT EXISTS idx_meeting_transcripts_start_ms ON meeting_transcripts(meeting_id, start_ms);

CREATE INDEX IF NOT EXISTS idx_meeting_action_items_meeting_id ON meeting_action_items(meeting_id);

CREATE INDEX IF NOT EXISTS idx_meeting_summaries_meeting_id ON meeting_summaries(meeting_id);
