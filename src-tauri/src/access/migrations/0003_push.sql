-- 0003_push (plans/pwa S2): Web Push subscriptions, per principal.
--
-- Lives in the access store (T0 `<data-dir>/access.db`, T1
-- `operator/accounts.db`) so a subscription dies in the same transaction as
-- the device grant that made it (`devices::mark_revoked`). Never opened by a
-- principal child.
--
-- `endpoint` is a bearer capability at the push service; `p256dh` / `auth`
-- are the browser's message keys. None of the three is ever returned by an
-- RPC or logged.

CREATE TABLE push_subscriptions (
  sub_id           TEXT    PRIMARY KEY,
  principal_id     TEXT    NOT NULL,
  device_id        TEXT    NULL REFERENCES devices(device_id),
  via              TEXT    NOT NULL CHECK (via IN ('device', 'session', 'operator')),
  session_epoch    INTEGER NULL,
  session_ref      TEXT    NULL,
  endpoint         TEXT    NOT NULL UNIQUE,
  endpoint_origin  TEXT    NOT NULL,
  p256dh           BLOB    NOT NULL CHECK (length(p256dh) = 65),
  auth             BLOB    NOT NULL CHECK (length(auth) = 16),
  vapid_key_id     TEXT    NOT NULL,
  kinds            TEXT    NOT NULL DEFAULT '[]',
  label            TEXT    NULL CHECK (label IS NULL OR length(label) <= 64),
  user_agent       TEXT    NULL,
  created_at       INTEGER NOT NULL,
  updated_at       INTEGER NOT NULL,
  last_success_at  INTEGER NULL,
  last_failure_at  INTEGER NULL,
  failure_count    INTEGER NOT NULL DEFAULT 0,
  last_status      INTEGER NULL
);

CREATE INDEX push_subscriptions_principal ON push_subscriptions (principal_id);
CREATE INDEX push_subscriptions_device ON push_subscriptions (device_id);

-- Small push state that must survive a restart (the last announced server
-- update version, so a restart doesn't announce it again).
CREATE TABLE push_meta (
  k TEXT PRIMARY KEY,
  v TEXT NOT NULL
);
