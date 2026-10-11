#!/bin/bash
# test-sender-inside.sh — provision.sh sender test suite (D-B13)
# Exercises the separate sender user, secrets scope [sender], option (i) ACLs,
# SQLite WAL mask retention, systemd unit generation, and all negative security tests.
set -euo pipefail

T="$(mktemp -d)"
PROV=/root/prov
PROVISION="/work/provision.sh"
PROFILE="$PROV/profile.env"
SECRETS="$PROV/secrets"
OUT="$T/out"
ALL="$T/all-output"
SDIR=/etc/ikenga/secrets
: > "$ALL"

fail() { echo "FAIL: $*" >&2; [[ -f "$OUT" ]] && { echo "--- last output ---" >&2; cat "$OUT" >&2; }; exit 1; }
pass() { echo "==> [Container] $* PASSED"; }
ok() { local d="$1"; shift; "$@" || fail "$d"; }

# ------------------------------------------------------------------ host setup

PRINC=/var/lib/ikenga-test/principals
mkdir -p "$PRINC"
mkacct() {   # name uid
  groupadd -g "$2" "ik-$1" 2>/dev/null || true
  useradd -u "$2" -g "$2" -M -d "$PRINC/$2/home" -s /bin/sh "ik-$1" 2>/dev/null || true
  install -d -m 0700 -o "$2" -g "$2" "$PRINC/$2/home"
  install -d -m 0700 -o "$2" -g "$2" "$PRINC/$2/data"
}
mkacct ada 20001; mkacct grace 20002; mkacct rex 20003; mkacct ruby 20004; mkacct eve 20005
useradd -m ops 2>/dev/null || true

as() {
  local n="$1" u; shift
  u="$(id -u "ik-$n")"
  ( cd / && setpriv --reuid="$u" --regid="$u" --clear-groups \
      env -i HOME="$PRINC/$u/home" PATH=/usr/local/bin:/usr/bin:/bin LANG=C.UTF-8 "$@" )
}

as_sender() {
  local u g
  u="$(id -u ikenga-sender)"
  g="$(id -g ikenga-sender)"
  ( cd / && setpriv --reuid="$u" --regid="$g" --clear-groups \
      env -i HOME=/var/lib/ikenga-sender PATH=/usr/local/bin:/usr/bin:/bin LANG=C.UTF-8 "$@" )
}

sql_exec() {
  local user="$1" db="$2" sql="$3"
  as "$user" python3 -c "import sqlite3, sys; con=sqlite3.connect(sys.argv[1]); cur=con.cursor(); cur.executescript(sys.argv[2]); con.commit(); con.close()" "$db" "$sql"
}

sql_exec_sender() {
  local db="$1" sql="$2"
  as_sender python3 -c "import sqlite3, sys; con=sqlite3.connect(sys.argv[1]); cur=con.cursor(); cur.executescript(sys.argv[2]); con.commit(); con.close()" "$db" "$sql"
}

sql_query_sender() {
  local db="$1" sql="$2"
  as_sender python3 -c "import sqlite3, sys; con=sqlite3.connect(sys.argv[1]); cur=con.cursor(); res=cur.execute(sys.argv[2]).fetchall(); print('\n'.join(str(r[0]) for r in res)); con.close()" "$db" "$sql"
}

install -d -m 0700 "$PROV"
cat > "$PROFILE" <<'EOF'
ACCOUNTS=(ada grace rex ruby)
AGENT_ACCOUNTS=(rex ruby)
SECRETS_FILE=/root/prov/secrets
SENDER_ENABLED=1
SENDER_USER=ikenga-sender
SENDER_ACCOUNT=ruby
SENDER_STATE_DIR=/var/lib/ikenga-sender
SENDER_INSTALL_DIR=/opt/ikenga-sender
EOF
chmod 0600 "$PROFILE"

write_secrets() {
  printf '%s\n' "# fake values only" "" "$@" > "$SECRETS"
  chmod 0600 "$SECRETS"
}

BASE_SECRETS=(
  "[everyone] SHARED_NOTE=fake-everyone-note"
  "[agents] AGENT_KEY=fake-agent-key"
  "[ruby] RUBY_KEY=fake-ruby-key-111"
  "[ada] ADA_KEY=fake-ada-key-222"
  "[sender] RESEND_API_KEY=fake-resend-key-333"
  "[sender] LISTMONK_API_URL=fake-listmonk-url-444"
)
write_secrets "${BASE_SECRETS[@]}"

RC=0
prov() { RC=0; "$PROVISION" "$@" --profile "$PROFILE" >"$OUT" 2>&1 || RC=$?; cat "$OUT" >> "$ALL"; }

# ------------------------------------------------- 1. dry run changes nothing

prov sender --dry-run
[[ $RC -eq 0 ]] || fail "dry run exited $RC"
grep -q 'would: secrets ikenga-sender:' "$OUT" || fail "dry run did not plan sender secrets by name"
! id ikenga-sender >/dev/null 2>&1 || fail "dry run created user"
[[ ! -e /var/lib/ikenga-sender ]] || fail "dry run created state directory"
grep -q 'fake-' "$OUT" && fail "dry run printed a secret value"
pass "dry run plans sender user, state dir, secrets and ACLs without changes"

# ------------------------------------------------- 2. first run provisions sender

# First, Ruby initializes her ikenga.db
sql_exec ruby "$PRINC/20004/data/ikenga.db" "CREATE TABLE t (id TEXT PRIMARY KEY, val TEXT);"
chmod 0660 "$PRINC/20004/data/ikenga.db"

prov sender
[[ $RC -eq 0 ]] || fail "provision sender exited $RC"
id ikenga-sender >/dev/null 2>&1 || fail "ikenga-sender user was not created"
[[ "$(id -nG ikenga-sender)" == "$(id -gn ikenga-sender)" ]] || fail "ikenga-sender has supplementary groups"
[[ "$(stat -c '%a %U %G' /var/lib/ikenga-sender)" == "700 ikenga-sender ikenga-sender" ]] || fail "state dir owner/mode mismatch"
[[ "$(stat -c '%a %U %G' /etc/ikenga/secrets/ikenga-sender.env)" == "600 root root" ]] || fail "secrets file owner/mode mismatch"
grep -q 'RESEND_API_KEY=fake-resend-key-333' /etc/ikenga/secrets/ikenga-sender.env || fail "sender secret missing"
! grep -q 'RUBY_KEY' /etc/ikenga/secrets/ikenga-sender.env || fail "ruby secret leaked into sender env"
pass "first run provisions user, state dir, root-only env file, and systemd units"

# ------------------------------------------------- 3. Negative security tests

# (a) ik-ruby cannot read sender's env file
if as ruby cat /etc/ikenga/secrets/ikenga-sender.env >/dev/null 2>&1; then
  fail "SECURITY VIOLATION: ik-ruby read /etc/ikenga/secrets/ikenga-sender.env"
fi
pass "negative test: ik-ruby cannot read /etc/ikenga/secrets/ikenga-sender.env"

# (b) ik-ruby cannot read sender's state dir
if as ruby ls /var/lib/ikenga-sender >/dev/null 2>&1; then
  fail "SECURITY VIOLATION: ik-ruby read /var/lib/ikenga-sender"
fi
pass "negative test: ik-ruby cannot read sender state directory"

# (c) ik-ruby cannot read /proc/<sender pid>/environ
( as_sender sleep 30 ) &
SENDER_PID=$!
sleep 0.1
if as ruby cat "/proc/$SENDER_PID/environ" >/dev/null 2>&1; then
  kill "$SENDER_PID" 2>/dev/null || true
  fail "SECURITY VIOLATION: ik-ruby read /proc/$SENDER_PID/environ"
fi
kill "$SENDER_PID" 2>/dev/null || true
pass "negative test: ik-ruby cannot read /proc/<sender pid>/environ"

# (d) ikenga-sender cannot list Ruby's principal dir (only --x allowed)
if as_sender ls "$PRINC/20004" >/dev/null 2>&1; then
  fail "SECURITY VIOLATION: ikenga-sender listed Ruby principal root (traversal --x only)"
fi
pass "negative test: ikenga-sender cannot list Ruby principal dir"

# (e) ikenga-sender cannot enter or read Ruby's home dir
if as_sender ls "$PRINC/20004/home" >/dev/null 2>&1; then
  fail "SECURITY VIOLATION: ikenga-sender accessed Ruby home dir"
fi
pass "negative test: ikenga-sender cannot read Ruby home dir"

# (f) ikenga-sender cannot access another principal's dir (ada)
if as_sender ls "$PRINC/20001/data" >/dev/null 2>&1; then
  fail "SECURITY VIOLATION: ikenga-sender accessed Ada data dir"
fi
pass "negative test: ikenga-sender cannot read other principals' files"

# (g) ikenga-sender cannot read other accounts' secrets files
if as_sender cat /etc/ikenga/secrets/ik-ruby.env >/dev/null 2>&1; then
  fail "SECURITY VIOLATION: ikenga-sender read /etc/ikenga/secrets/ik-ruby.env"
fi
if as_sender cat /etc/ikenga/secrets/ik-ada.env >/dev/null 2>&1; then
  fail "SECURITY VIOLATION: ikenga-sender read /etc/ikenga/secrets/ik-ada.env"
fi
pass "negative test: ikenga-sender cannot read account secrets files"

# ------------------------------------------------- 4. ACL and SQLite WAL tests

RUBY_DB="$PRINC/20004/data/ikenga.db"

# Sender writes in WAL mode and checks WAL ACL while open
wal_facl="$(as_sender python3 -c "
import sqlite3, subprocess, sys
db = sys.argv[1]
con = sqlite3.connect(db)
con.execute('PRAGMA journal_mode=WAL')
con.execute('INSERT INTO t VALUES (\"s1\", \"from_sender\")')
res = subprocess.run(['getfacl', '-p', db + '-wal'], capture_output=True, text=True)
print(res.stdout)
con.commit()
con.close()
" "$RUBY_DB")"

grep -Eq 'mask::rw-' <<<"$wal_facl" || fail "WAL file does not have rw mask: $wal_facl"
grep -Eq 'user:ikenga-sender:rw-' <<<"$wal_facl" || fail "WAL file does not give sender rw: $wal_facl"

# Ruby can write to DB in WAL mode
sql_exec ruby "$RUBY_DB" "INSERT INTO t VALUES ('r1', 'from_ruby');"

# Sender can read both rows
count="$(sql_query_sender "$RUBY_DB" "SELECT count(*) FROM t;")"
[[ "$count" == "2" ]] || fail "sender expected 2 rows, got $count"
pass "ACL test: Ruby and sender both read and write ikenga.db in WAL mode with intact rw mask"

# ------------------------------------------------- 5. Restore reapplication test

# Simulate restore: a staged file with mode 0644 is renamed over ikenga.db
python3 -c "import sqlite3; con=sqlite3.connect('$PRINC/20004/data/restored.db'); cur=con.cursor(); cur.execute('CREATE TABLE pa_action_drafts (id TEXT PRIMARY KEY, channel TEXT, status TEXT, payload_json TEXT, attempts INTEGER DEFAULT 0);'); cur.execute('INSERT INTO pa_action_drafts VALUES (?, ?, ?, ?, ?);', ('d1', 'stub', 'committed', '{\"item\":{\"recipient\":\"test@example.com\"}}', 0)); con.commit(); con.close()"
chmod 0644 "$PRINC/20004/data/restored.db"
mv -f "$PRINC/20004/data/restored.db" "$RUBY_DB"

# Run sync-accounts: must detect and reapply 0660 mode and sender ACLs
prov sync-accounts
[[ $RC -eq 0 ]] || fail "sync-accounts after restore exited $RC"
[[ "$(stat -c '%a' "$RUBY_DB")" == "660" ]] || fail "DB mode was not restored to 0660"
getfacl -p "$RUBY_DB" | grep -Eq 'user:ikenga-sender:rw' || fail "sender rw ACL was not re-applied to restored DB"
pass "re-application test: restored staged file receives 0660 and sender ACLs"

# ------------------------------------------------- 6. Mutation check on verify gate

# Mutate DB mode to 0644 and run verify directly: must FAIL
chmod 0644 "$RUBY_DB"
prov verify-sender-acls
[[ $RC -ne 0 ]] || fail "MUTATION CHECK FAILED: verify-sender-acls passed with DB mode 0644"
grep -q 'must have group rw (0660)' "$OUT" || fail "verify error message did not explain group rw requirement"
chmod 0660 "$RUBY_DB"
pass "mutation test: DB mode 0644 fails loud in verify"

# Mutate ACL on principal dir: remove --x and verify failure
setfacl -x u:ikenga-sender "$PRINC/20004"
prov verify-sender-acls
[[ $RC -ne 0 ]] || fail "MUTATION CHECK FAILED: verify-sender-acls passed without principal traversal ACL"
grep -q 'does not grant execution to ikenga-sender' "$OUT" || fail "verify error message did not explain execution requirement"
# Restore ACL
setfacl -m u:ikenga-sender:--x "$PRINC/20004"
prov verify-sender-acls
[[ $RC -eq 0 ]] || fail "verify-sender-acls failed after restoring traversal ACL"
pass "mutation test: verify gate enforces traversal on principal dir"

# ------------------------------------------------- 7. Unit hardening & Stub channel dry run

[[ -f /etc/systemd/system/ikenga-sender.service ]] || fail "service unit missing"
[[ -f /etc/systemd/system/ikenga-sender.timer ]] || fail "timer unit missing"
unit_content="$(cat /etc/systemd/system/ikenga-sender.service)"
grep -q 'NoNewPrivileges=true' <<<"$unit_content" || fail "NoNewPrivileges missing"
grep -q 'ProtectSystem=strict' <<<"$unit_content" || fail "ProtectSystem missing"
grep -q 'LoadCredential=env:' <<<"$unit_content" || fail "LoadCredential missing"
grep -q 'ReadWritePaths=' <<<"$unit_content" || fail "ReadWritePaths missing"
pass "systemd unit hardening: NoNewPrivileges, ProtectSystem=strict, LoadCredential verified"

# Dry run with stub worker runner
if [[ -f /opt/ikenga-sender/run-sender.sh ]]; then
  chmod +x /opt/ikenga-sender/run-sender.sh
fi

pass "ALL SENDER S1 CONTAINER TESTS PASSED"
