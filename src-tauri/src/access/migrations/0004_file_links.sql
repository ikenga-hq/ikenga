-- 0004_file_links (box-files WP-15, 06-file-links-security §3): outside links
-- to one file of one account. Lives in the access store; never opened by a
-- principal child. The token is never stored, only SHA-256(token). No row is
-- ever deleted in v1. Timestamps are Unix milliseconds. Under T0 the table
-- exists and stays empty (outside links need a T1 server).

CREATE TABLE file_links (
  link_id               TEXT    PRIMARY KEY
                                CHECK (length(link_id) = 36 AND link_id = lower(link_id)),
  token_sha256          BLOB    NOT NULL UNIQUE CHECK (length(token_sha256) = 32),
  owner_principal_id    TEXT    NOT NULL
                                CHECK (length(owner_principal_id) = 36
                                       AND owner_principal_id = lower(owner_principal_id)),
  path                  TEXT    NOT NULL
                                CHECK (length(path) BETWEEN 2 AND 4096 AND substr(path, 1, 1) = '/'),
  name                  TEXT    NOT NULL CHECK (length(name) BETWEEN 1 AND 255),
  preview_kind          TEXT    NOT NULL CHECK (preview_kind IN ('pdf','image','text','none')),
  mime                  TEXT    NOT NULL CHECK (mime IN ('application/pdf','image/png','image/jpeg',
                                                         'image/gif','image/webp',
                                                         'text/plain; charset=utf-8',
                                                         'application/octet-stream')),
  size_at_create        INTEGER NOT NULL CHECK (size_at_create >= 0),
  created_at            INTEGER NOT NULL,
  created_via           TEXT    NOT NULL CHECK (created_via IN ('session','device')),
  created_device_id     TEXT,
  expires_at            INTEGER,                         -- NULL = never (D-R1-8)
  passcode_phc          TEXT    CHECK (passcode_phc IS NULL OR passcode_phc LIKE '$argon2id$%'),
  passcode_failures     INTEGER NOT NULL DEFAULT 0,      -- rolling 24 h count, reset on lock
  passcode_window_start INTEGER,
  passcode_locked_until INTEGER,
  revoked_at            INTEGER,
  revoked_by            TEXT,
  revoked_reason        TEXT    CHECK (revoked_reason IS NULL OR revoked_reason IN ('owner','admin')),
  open_count            INTEGER NOT NULL DEFAULT 0,      -- coalesced link.opened rows
  download_count        INTEGER NOT NULL DEFAULT 0,      -- coalesced link.downloaded rows
  first_opened_at       INTEGER,                         -- first /view or /download (§8)
  last_opened_at        INTEGER,
  CHECK (expires_at IS NULL OR expires_at > created_at),
  CHECK ((revoked_at IS NULL) = (revoked_reason IS NULL)),
  CHECK (created_via = 'device' OR created_device_id IS NULL)
);
CREATE INDEX file_links_owner ON file_links (owner_principal_id, revoked_at);
CREATE INDEX file_links_live  ON file_links (expires_at) WHERE revoked_at IS NULL
