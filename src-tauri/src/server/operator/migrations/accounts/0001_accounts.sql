-- G-PRINCIPAL §6.1 — operator/accounts.db, migration set 'accounts', version 1.
-- Applied by `operator::migrations::apply` inside one BEGIN IMMEDIATE together
-- with its `_operator_migrations` row. Never edit: add the next version.

CREATE TABLE accounts (
  principal_id        TEXT    PRIMARY KEY
                      CHECK (length(principal_id) = 36 AND principal_id = lower(principal_id)),
  username            TEXT    NOT NULL UNIQUE COLLATE NOCASE
                      CHECK (length(username) BETWEEN 1 AND 32),
  password_phc        TEXT,             -- argon2id PHC string ($argon2id$v=19$m=…,t=…,p=…$salt$hash).
                                        -- NULL = no password login (reserved for WP-22 OIDC-only);
                                        -- WP-20's CLI always sets it.
  unix_name           TEXT    NOT NULL UNIQUE,     -- immutable after provisioning (§7.2)
  unix_uid            INTEGER NOT NULL UNIQUE CHECK (unix_uid > 0),
  unix_gid            INTEGER NOT NULL CHECK (unix_gid > 0),
  home                TEXT    NOT NULL,            -- absolute
  shell               TEXT    NOT NULL DEFAULT '/bin/sh',
  is_admin            INTEGER NOT NULL DEFAULT 0 CHECK (is_admin IN (0, 1)),
  session_epoch       INTEGER NOT NULL DEFAULT 0,  -- bumped to revoke every session (§2.2)
  adopted             INTEGER NOT NULL DEFAULT 0 CHECK (adopted IN (0, 1)),  -- §11.2
  disabled_at         INTEGER,                     -- unix secs; NULL = active
  created_at          INTEGER NOT NULL,
  updated_at          INTEGER NOT NULL,
  password_changed_at INTEGER
);
-- Rows are never deleted (tombstones keep principal_id and unix_uid unreusable).

CREATE TABLE auth_events (                          -- append-only
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  at            INTEGER NOT NULL,
  principal_id  TEXT REFERENCES accounts(principal_id),  -- NULL for an unknown-username failure
  username_tried TEXT,
  kind          TEXT NOT NULL CHECK (kind IN (
                  'login_ok','login_fail','login_throttled','logout','password_changed',
                  'account_created','account_disabled','account_enabled','sessions_revoked',
                  'provision_failed','probe_failed')),
  remote_addr   TEXT,
  user_agent    TEXT,
  detail        TEXT                                -- JSON
);
CREATE INDEX auth_events_principal_at ON auth_events (principal_id, at);

-- I-4, enforced by the store rather than only by the absence of a DELETE in
-- code: a row is a tombstone that keeps its principal_id and unix_uid out of
-- circulation for good.
CREATE TRIGGER accounts_never_deleted BEFORE DELETE ON accounts
BEGIN
  SELECT RAISE(ABORT, 'accounts rows are never deleted (G-PRINCIPAL I-4)');
END;
