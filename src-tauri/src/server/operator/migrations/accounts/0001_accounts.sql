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

-- I-4 and §7.2 ("unix_name is immutable after provisioning"), for UPDATE too:
-- the identity columns of a row never change, so neither an id nor a uid can
-- be recycled by rewriting a tombstone. (username, password, shell, flags and
-- the epoch stay mutable; so does unix_gid, which an adopted host user's
-- primary group can legitimately change.)
CREATE TRIGGER accounts_identity_immutable
BEFORE UPDATE OF principal_id, unix_uid, unix_name ON accounts
WHEN NEW.principal_id IS NOT OLD.principal_id
  OR NEW.unix_uid IS NOT OLD.unix_uid
  OR NEW.unix_name IS NOT OLD.unix_name
BEGIN
  SELECT RAISE(ABORT, 'accounts identity columns are immutable (G-PRINCIPAL I-4, §7.2)');
END;

-- Operator-wide settings that the broker and the root CLI (separate
-- invocations) must agree on. `uid_range` ('START-END') is pinned by the first
-- provisioning (`Provisioner::pin_uid_range`) and a different --uid-range is
-- refused afterwards, so neither can allocate the other's §8 probe uid.
CREATE TABLE operator_meta (
  key    TEXT PRIMARY KEY,
  value  TEXT NOT NULL
);
