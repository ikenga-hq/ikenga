-- G-ACCESS §8.2. Runs on <data-dir>/access.db (T0) and operator/accounts.db (T1).
-- No FKs: the same set runs on T0, which has no `accounts` table (OD-11). principal_id integrity is
-- enforced in code (A-5). One statement per ';'. Every principal_id / device_id / invite_id is UUIDv7 text.

CREATE TABLE store_meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
-- keys: store_id, tier ('t0'|'t1'), created_at, owner_principal_id (T0 only)

CREATE TABLE devices (
  device_id          TEXT    PRIMARY KEY CHECK (length(device_id) = 36 AND device_id = lower(device_id)),
  principal_id       TEXT    NOT NULL    CHECK (length(principal_id) = 36 AND principal_id = lower(principal_id)),
  kind               TEXT    NOT NULL    CHECK (kind IN ('host','paired')),
  name               TEXT    NOT NULL    CHECK (length(name) BETWEEN 1 AND 64),
  platform           TEXT,
  tier               TEXT    NOT NULL    CHECK (tier IN ('view','dispatch','approve','full')),
  secret_sha256      BLOB    CHECK (secret_sha256 IS NULL OR length(secret_sha256) = 32),
  prev_secret_sha256 BLOB    CHECK (prev_secret_sha256 IS NULL OR length(prev_secret_sha256) = 32),
  prev_valid_until   INTEGER,
  secret_rotated_at  INTEGER,
  grant_epoch        INTEGER NOT NULL DEFAULT 0,
  paired_at          INTEGER NOT NULL,
  paired_via_device  TEXT,
  pairing_id         TEXT,
  last_seen_at       INTEGER,
  last_seen_addr     TEXT,
  revoked_at         INTEGER,
  revoked_by         TEXT,
  revoked_reason     TEXT    CHECK (revoked_reason IS NULL OR revoked_reason IN ('user','idle','sessions_revoked')),
  CHECK (kind <> 'host' OR (tier = 'full' AND secret_sha256 IS NULL)),
  CHECK (revoked_at IS NULL OR secret_sha256 IS NULL)
);
CREATE UNIQUE INDEX devices_one_host ON devices(kind) WHERE kind = 'host';
CREATE INDEX devices_principal ON devices(principal_id, revoked_at);

CREATE TABLE routing_prefs (
  principal_id TEXT PRIMARY KEY,
  mode         TEXT NOT NULL CHECK (mode IN ('any_approve','this_device')),
  device_id    TEXT,
  updated_at   INTEGER NOT NULL,
  CHECK ((mode = 'this_device') = (device_id IS NOT NULL))
);

CREATE TABLE shared_projects (
  project_key             TEXT    PRIMARY KEY,               -- '<owner_principal_id>/<project_id>'
  owner_principal_id      TEXT    NOT NULL,
  project_id              TEXT    NOT NULL,                  -- the owner's ikenga.db projects.id slug
  display_name            TEXT    NOT NULL,                  -- cached via share_project_info (§4.5.4) at first invite
  owner_approval_required INTEGER NOT NULL DEFAULT 1 CHECK (owner_approval_required IN (0,1)),
  created_at              INTEGER NOT NULL,
  updated_at              INTEGER NOT NULL,
  UNIQUE (owner_principal_id, project_id)
);

CREATE TABLE project_role_caps (                             -- overrides of §4.1 defaults; absent = default
  project_key TEXT    NOT NULL,
  role        TEXT    NOT NULL CHECK (role IN ('operator','reviewer','guest')),
  cap         TEXT    NOT NULL CHECK (cap IN ('files','sessions','dispatch','approve','install','settings')),
  allowed     INTEGER NOT NULL CHECK (allowed IN (0,1)),
  updated_at  INTEGER NOT NULL,
  updated_by  TEXT    NOT NULL,
  PRIMARY KEY (project_key, role, cap)
);

CREATE TABLE project_members (
  id                     INTEGER PRIMARY KEY AUTOINCREMENT,
  project_key            TEXT    NOT NULL,
  member_principal_id    TEXT    NOT NULL,
  role                   TEXT    NOT NULL CHECK (role IN ('operator','reviewer','guest')),
  scope_kind             TEXT    NOT NULL CHECK (scope_kind IN ('project','artifact')),
  artifact_path          TEXT,
  expires_at             INTEGER,
  weekly_spend_cap_cents INTEGER,                           -- reserved, §15 N-3
  invite_id              TEXT,
  added_by               TEXT    NOT NULL,
  added_at               INTEGER NOT NULL,
  last_active_at         INTEGER,                           -- broker, ≤1 write/min/member
  removed_at             INTEGER,
  removed_by             TEXT,
  removed_reason         TEXT    CHECK (removed_reason IS NULL OR removed_reason IN ('removed','expired')),
  CHECK ((scope_kind = 'artifact') = (artifact_path IS NOT NULL)),
  CHECK (role <> 'guest' OR (scope_kind = 'artifact' AND expires_at IS NOT NULL))
);
CREATE UNIQUE INDEX project_members_active ON project_members(project_key, member_principal_id) WHERE removed_at IS NULL;
CREATE INDEX project_members_member ON project_members(member_principal_id) WHERE removed_at IS NULL;

CREATE TABLE invites (
  invite_id         TEXT    PRIMARY KEY,
  project_key       TEXT    NOT NULL,
  role              TEXT    NOT NULL CHECK (role IN ('operator','reviewer','guest')),
  scope_kind        TEXT    NOT NULL CHECK (scope_kind IN ('project','artifact')),
  artifact_path     TEXT,
  member_expires_at INTEGER,
  mode              TEXT    NOT NULL CHECK (mode IN ('email','link')),
  invitee_label     TEXT,
  allow_new_account INTEGER NOT NULL DEFAULT 0 CHECK (allow_new_account IN (0,1)),   -- §7.2, fixed at issue
  token_sha256      BLOB    NOT NULL UNIQUE CHECK (length(token_sha256) = 32),
  issued_by         TEXT    NOT NULL,
  issued_by_device  TEXT,
  issued_at         INTEGER NOT NULL,
  expires_at        INTEGER NOT NULL,
  accepted_at       INTEGER,
  accepted_by       TEXT,
  revoked_at        INTEGER,
  revoked_by        TEXT,
  revoked_reason    TEXT    CHECK (revoked_reason IS NULL OR revoked_reason IN ('revoked','dismissed')),
  CHECK ((scope_kind = 'artifact') = (artifact_path IS NOT NULL)),
  CHECK (role <> 'guest' OR (scope_kind = 'artifact' AND member_expires_at IS NOT NULL)),
  CHECK (accepted_at IS NULL OR revoked_at IS NULL)
);
CREATE INDEX invites_project ON invites(project_key, accepted_at, revoked_at);

CREATE TABLE ownership_transfers (                           -- reserved for §4.3 / §15 N-2; unused in v1
  id INTEGER PRIMARY KEY AUTOINCREMENT, project_key TEXT NOT NULL, from_principal_id TEXT NOT NULL,
  to_principal_id TEXT NOT NULL, offered_at INTEGER NOT NULL, accepted_at INTEGER, cancelled_at INTEGER
);

CREATE TABLE audit_events (
  seq                  INTEGER PRIMARY KEY,                  -- contiguous; assigned head+1 in-txn
  at_ms                INTEGER NOT NULL,
  kind                 TEXT    NOT NULL,
  category             TEXT    NOT NULL CHECK (category IN ('permission','dispatch','access','pairing','people')),
  principal_id         TEXT,
  device_id            TEXT,
  via                  TEXT    NOT NULL CHECK (via IN ('session','device','operator','cli','system')),
  subject_principal_id TEXT,
  subject_device_id    TEXT,
  project_key          TEXT,
  target               TEXT,
  remote_addr          TEXT,
  user_agent           TEXT,
  detail               TEXT    NOT NULL DEFAULT '{}',
  prev_hash            BLOB    NOT NULL CHECK (length(prev_hash) = 32),
  hash                 BLOB    NOT NULL UNIQUE CHECK (length(hash) = 32)
);
CREATE INDEX audit_principal_at ON audit_events(principal_id, at_ms);
CREATE INDEX audit_project_at   ON audit_events(project_key, at_ms);
CREATE INDEX audit_kind_at      ON audit_events(category, at_ms);
CREATE TRIGGER audit_events_no_update BEFORE UPDATE ON audit_events BEGIN SELECT RAISE(ABORT, 'audit_events is append-only'); END;
CREATE TRIGGER audit_events_no_delete BEFORE DELETE ON audit_events BEGIN SELECT RAISE(ABORT, 'audit_events is append-only'); END
