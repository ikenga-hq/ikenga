-- 0070_access_attribution.sql
-- G-ACCESS §5.7, §8.4 (WP-74a, schema-first): the attribution columns the
-- remote permission routing (WP-75) and member comments (WP-76) fill.
-- WP-74a adds them so `db.rs` stays single-owner in every wave.
--
-- shell_notifications:
--   requested_by    principal_id of the member whose dispatch raised the ask;
--                   NULL for the Owner's own work.
--   project_id      the project the ask belongs to (a share sees only its
--                   project's `permission` rows, §5.7).
--   sensitive       0 = no, 1 = sensitive, 2 = secret material (§5.3),
--                   written by the producer when the row is created.
--   decided_by      principal_id of whoever decided the ask.
--   decided_via     session | device | operator.
--   decided_device  device_id the decision came from, if any.
-- artifact_comments:
--   author_principal_id  the comment's author (a non-Owner may change only
--                        their own comments in a share, §4.5.4).
--
-- Conventions (ikenga.db, as 0066): no FK constraints (soft links), one
-- statement per ';', only '--' line comments.

ALTER TABLE shell_notifications ADD COLUMN requested_by TEXT;
ALTER TABLE shell_notifications ADD COLUMN project_id TEXT;
ALTER TABLE shell_notifications ADD COLUMN sensitive INTEGER NOT NULL DEFAULT 0;
ALTER TABLE shell_notifications ADD COLUMN decided_by TEXT;
ALTER TABLE shell_notifications ADD COLUMN decided_via TEXT;
ALTER TABLE shell_notifications ADD COLUMN decided_device TEXT;
ALTER TABLE artifact_comments ADD COLUMN author_principal_id TEXT;
CREATE INDEX IF NOT EXISTS idx_shell_notifications_project ON shell_notifications(project_id, kind, resolved_at);
