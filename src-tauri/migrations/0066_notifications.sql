-- 0066_notifications.sql
-- WP-40 (Phase 5b, D-07 `notifications`) - the aggregation table behind the
-- notification centre, the daily address (WP-39) and the toast bridge (WP-40b).
--
-- Before this, nothing aggregated permission / run / update / violation
-- events: toasts were transient and the banner slot held one notice. A toast
-- becomes a transient copy of a row that stays here.
--
-- Columns:
--   kind        permission | run_finished | run_failed | update | violation |
--               invite. Validated in Rust (`notifications::NotificationKind`),
--               deliberately NOT a CHECK constraint: adding a kind later would
--               otherwise need a table rebuild in SQLite.
--   title/body  display copy, written by the producer.
--   action      JSON object `{ "kind": "<action kind>", ...params }` or NULL.
--               The UI maps the action kind to its buttons (Allow / Deny,
--               Open log, Release notes, Review, ...).
--   source      producer id, for audit (`iyke.hooks`, `engine.claude-code`,
--               `pkg.permissions_check`, `chi`, `updater`).
--   dedupe_key  producer-chosen key. How a repeat is folded is the producer's
--               call (`notifications::Coalesce`): `once` drops a repeat of any
--               row with the key; `while_unread` folds it into the newest
--               UNREAD, UNRESOLVED row with the key (count + 1, updated_at
--               bumped).
--   count       occurrences folded into this row (violations: "3 denials").
--   created_at  first occurrence, unix ms.
--   updated_at  latest occurrence, unix ms. Lists sort on this.
--   read_at     unix ms, NULL = unread.
--   resolved_at unix ms, NULL = still open. Distinct from read: set when the
--               thing a row asks about is over (a permission decided, timed
--               out, or answered in its terminal). A resolved `permission`
--               row must not offer Allow / Deny and does not count as
--               pending; a row can be read but still pending, or resolved
--               before anyone read it.
--
-- Per-kind mute is NOT stored here: it lives in `~/.ikenga/settings.json` at
-- `workspace.notifications.mutedKinds` (WP-32 settings substrate). Muted
-- kinds are still recorded and filtered at read time, so a mute never loses a
-- permission ask (permission and violation cannot be muted at all).
--
-- Conventions (ikenga.db):
--   - No FK constraints (soft links).
--   - One statement per ';' (split by runner).
--   - Only '--' line comments.

CREATE TABLE IF NOT EXISTS notifications (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  kind        TEXT    NOT NULL,
  title       TEXT    NOT NULL,
  body        TEXT,
  action      TEXT,
  source      TEXT    NOT NULL DEFAULT '',
  dedupe_key  TEXT,
  count       INTEGER NOT NULL DEFAULT 1,
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL,
  read_at     INTEGER,
  resolved_at INTEGER
);

CREATE INDEX IF NOT EXISTS idx_notifications_updated
  ON notifications (updated_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS idx_notifications_unread_kind
  ON notifications (read_at, kind);

CREATE INDEX IF NOT EXISTS idx_notifications_dedupe
  ON notifications (dedupe_key, read_at);

CREATE INDEX IF NOT EXISTS idx_notifications_kind_resolved
  ON notifications (kind, resolved_at);

-- When each kind was muted (unix ms), so un-muting marks read only the rows
-- recorded WHILE muted (`updated_at >= muted_at`), not an older backlog that
-- was already unread before the mute. The mute itself still lives in
-- settings.json; this is bookkeeping written by `notifications_mute_kind` /
-- `notifications_unmute_kind` and, for hand edits of settings.json, by the
-- settings-change listener (`notifications::mute::spawn_settings_listener`).
-- A kind with no row here marks nothing read on un-mute.
CREATE TABLE IF NOT EXISTS notification_mutes (
  kind      TEXT    PRIMARY KEY,
  muted_at  INTEGER NOT NULL
);
