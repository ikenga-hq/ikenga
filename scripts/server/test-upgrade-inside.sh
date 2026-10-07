#!/bin/bash
# provision.sh upgrade / check-update / apply-request / install-update-units,
# run as root inside an ubuntu:24.04 container (test-upgrade-container.sh).
# No real systemd: a mock systemctl starts a mock server that answers
# /api/health with its version.
set -euo pipefail

TEST_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_DIR"' EXIT

export INSTALL_DIR="$TEST_DIR/opt/ikenga"
export IKENGA_UPDATE_STATE_DIR="$TEST_DIR/state"
export IKENGA_PROVISION_STABLE="$TEST_DIR/sbin/ikenga-provision"
export IKENGA_SYSTEMD_DIR="$TEST_DIR/systemd"
STATE="$IKENGA_UPDATE_STATE_DIR"
SYSTEMD_DIR="$IKENGA_SYSTEMD_DIR"
mkdir -p "$INSTALL_DIR/bin" "$INSTALL_DIR/data/operator" "$TEST_DIR/releases" "$SYSTEMD_DIR"
chmod 0700 "$INSTALL_DIR/data/operator"
PROVISION="/work/provision.sh"
REL="$TEST_DIR/releases"
FAKE_SECRET="FAKE_SECRET_123"

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "==> [Container] $* PASSED"; }

# A secret the update path must never read into any output.
printf 'IKENGA_HOST=127.0.0.1\nIKENGA_VAULT_KEY=%s\n' "$FAKE_SECRET" > "$INSTALL_DIR/.env"
chmod 0600 "$INSTALL_DIR/.env"

# The T0 service user (request files on T0 must be owned by it).
id ikenga >/dev/null 2>&1 || useradd --system --no-create-home ikenga

# make_mock <bin path> <--version string> <version /api/health reports> [broken]
make_mock() {
  local bin="$1" ver="$2" health="$3" broken="${4:-}"
  mkdir -p "$(dirname "$bin")"
  if [[ -n "$broken" ]]; then
    cat > "$bin" <<BIN
#!/bin/bash
if [[ "\${1:-}" == "--version" ]]; then echo "ikenga-server $ver"; exit 0; fi
echo "Crashing immediately as simulated broken binary..."
exit 1
BIN
  else
    cat > "$bin" <<BIN
#!/bin/bash
if [[ "\${1:-}" == "--version" ]]; then echo "ikenga-server $ver"; exit 0; fi
exec python3 -c '
import http.server, socketserver, json, sys
host, ver = sys.argv[1], sys.argv[2]
body = (json.dumps({"ok": True, "name": "ikenga-server", "version": ver, "status": "ready"}) + "\n").encode()
class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/api/health":
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(body)
        else:
            self.send_response(404)
            self.end_headers()
    def log_message(self, format, *args):
        pass
socketserver.TCPServer.allow_reuse_address = True
with socketserver.TCPServer((host, 4000), Handler) as httpd:
    httpd.serve_forever()
' "\${MOCK_HOST:-127.0.0.1}" "$health"
BIN
  fi
  chmod +x "$bin"
}

# make_release <version> <health version|broken> [min_upgrade_from] [with-unit]
make_release() {
  local ver="$1" health="$2" min="${3:-}" unit="${4:-}"
  local stage="$TEST_DIR/stage-$ver" members=(bin)
  if [[ "$health" == broken ]]; then
    make_mock "$stage/bin/ikenga-server" "$ver-broken" "" broken
  else
    make_mock "$stage/bin/ikenga-server" "$ver" "$health"
  fi
  if [[ -n "$unit" ]]; then
    echo "# unit version $ver" > "$stage/ikenga-server-t1.service"
    members+=(ikenga-server-t1.service)
  fi
  local tarball="$REL/ikenga-server_${ver}_linux_amd64.tar.gz"
  tar -czf "$tarball" -C "$stage" "${members[@]}"
  local sha size min_json=null
  sha="$(sha256sum "$tarball" | awk '{print $1}')"
  size="$(wc -c < "$tarball" | tr -d ' ')"
  [[ -n "$min" ]] && min_json="\"$min\""
  cat > "$REL/ikenga-server_${ver}_manifest.json" <<MANIFEST
{
  "schema": "ikenga-server-release/1",
  "version": "$ver",
  "tag": "v$ver",
  "commit": "0123456789abcdef0123456789abcdef01234567",
  "published_at": "2026-10-05T12:00:00Z",
  "channel": "stable",
  "glibc_floor": "2.31",
  "min_upgrade_from": $min_json,
  "artifacts": [
    { "kind": "tarball", "arch": "amd64", "name": "ikenga-server_${ver}_linux_amd64.tar.gz",
      "sha256": "$sha", "size": $size }
  ]
}
MANIFEST
}
manifest() { echo "$REL/ikenga-server_$1_manifest.json"; }
tarball() { echo "$REL/ikenga-server_$1_linux_amd64.tar.gz"; }

# Initial install: 0.18.2.
make_mock "$INSTALL_DIR/bin/ikenga-server" 0.18.2 0.18.2

# Mock systemctl. Only ikenga-server* units start the mock server; fds 8/9
# (provision.sh's locks) are closed so the server never holds them.
mkdir -p "$TEST_DIR/bin"
cat << 'SYSCTL' > "$TEST_DIR/bin/systemctl"
#!/bin/bash
PIDFILE="/tmp/mock-ikenga.pid"
case "$1" in
  restart|start)
    [[ "$*" == *ikenga-server* ]] || exit 0
    if [[ -f "$PIDFILE" ]]; then
      kill "$(cat "$PIDFILE")" 2>/dev/null || true
      rm -f "$PIDFILE"
    fi
    pkill -f "http.server" 2>/dev/null || true
    sleep 0.5
    "$INSTALL_DIR/bin/ikenga-server" >/dev/null 2>&1 8>&- 9>&- &
    echo $! > "$PIDFILE"
    sleep 0.5
    ;;
  is-active)
    if [[ -f "$PIDFILE" ]] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then exit 0; else exit 1; fi
    ;;
  *) exit 0 ;;
esac
SYSCTL
chmod +x "$TEST_DIR/bin/systemctl"
export PATH="$TEST_DIR/bin:$PATH"

systemctl start ikenga-server-t1
curl -fsS http://127.0.0.1:4000/api/health | grep -q '"ok": true'
echo "==> [Container] Initial 0.18.2 mock server is healthy"

installed() { "$INSTALL_DIR/bin/ikenga-server" --version | awk '{print $NF}'; }
health_version() { curl -fsS "http://${1:-127.0.0.1}:4000/api/health" | python3 -c 'import json,sys; print(json.load(sys.stdin)["version"])'; }
jget() { python3 -c 'import json,sys; v=json.load(open(sys.argv[1]));
for k in sys.argv[2].split("."): v=v[k]
print(json.dumps(v) if not isinstance(v,str) else v)' "$1" "$2"; }
no_secret() { if grep -rq "$FAKE_SECRET" "$@"; then fail "secret leaked into $*"; fi; }

make_release 0.18.3 0.18.3 0.18.0
make_release 0.18.4 broken 0.18.0 with-unit
make_release 0.18.5 0.18.5
# 0.18.7's binary says 0.18.7 but /api/health still reports 0.18.6.
make_release 0.18.7 0.18.6

# --- TEST 1: --dry-run ---
echo "==> [Container] TEST 1: Testing --dry-run..."
OUTPUT="$(RELEASE_MANIFEST_URL="$(manifest 0.18.3)" RELEASE_TARBALL_PATH="$(tarball 0.18.3)" \
  "$PROVISION" upgrade --to 0.18.3 --dry-run)"
echo "$OUTPUT"
echo "$OUTPUT" | grep -q "\[dry-run\]"
echo "$OUTPUT" | grep -q "would: ikenga-server upgraded 0.18.2 -> 0.18.3"
[[ "$(installed)" == "0.18.2" ]]
pass "TEST 1 (--dry-run)"

# --- TEST 2: min-upgrade-from refusal ---
echo "==> [Container] TEST 2: Testing min-upgrade-from refusal..."
cat << MANIFEST > "$(manifest 0.19.0)"
{
  "schema": "ikenga-server-release/1",
  "version": "0.19.0",
  "channel": "stable",
  "min_upgrade_from": "0.18.5",
  "artifacts": [
    { "kind": "tarball", "arch": "amd64", "name": "ikenga-server_0.19.0_linux_amd64.tar.gz",
      "sha256": "0000000000000000000000000000000000000000000000000000000000000000", "size": 100 }
  ]
}
MANIFEST
set +e
ERR_OUT="$(RELEASE_MANIFEST_URL="$(manifest 0.19.0)" "$PROVISION" upgrade --to 0.19.0 2>&1)"
RET=$?
set -e
[[ $RET -ne 0 ]] && echo "$ERR_OUT" | grep -q "below min-upgrade-from 0.18.5" \
  || fail "expected refusal below min-upgrade-from, got $RET: $ERR_OUT"
pass "TEST 2 (min-upgrade-from refusal)"

# --- TEST 3: Open terminals refusal unless --force ---
echo "==> [Container] TEST 3: Testing open terminals refusal unless --force..."
set +e
ERR_OUT="$(IKENGA_TEST_OPEN_TERMINALS=2 RELEASE_MANIFEST_URL="$(manifest 0.18.3)" \
  "$PROVISION" upgrade --to 0.18.3 2>&1)"
RET=$?
set -e
[[ $RET -ne 0 ]] && echo "$ERR_OUT" | grep -q "2 open terminal(s) detected" \
  || fail "expected terminal refusal, got $RET: $ERR_OUT"
OUTPUT="$(IKENGA_TEST_OPEN_TERMINALS=2 RELEASE_MANIFEST_URL="$(manifest 0.18.3)" \
  "$PROVISION" upgrade --to 0.18.3 --force --dry-run)"
echo "$OUTPUT" | grep -q "Warning: 2 open terminal(s) will be terminated"
pass "TEST 3 (open terminals need --force)"

# --- TEST 4: Automatic rollback on failing binary ---
echo "==> [Container] TEST 4: Testing automatic rollback with failing binary..."
echo "# unit version 0.18.2" > "$SYSTEMD_DIR/ikenga-server-t1.service"
set +e
ERR_OUT="$(HEALTH_TIMEOUT=3 RELEASE_MANIFEST_URL="$(manifest 0.18.4)" RELEASE_TARBALL_PATH="$(tarball 0.18.4)" \
  "$PROVISION" upgrade --to 0.18.4 2>&1)"
RET=$?
set -e
echo "$ERR_OUT"
[[ $RET -eq 3 ]] || fail "a clean rollback must exit 3, got $RET"
echo "$ERR_OUT" | grep -q "Initiating automatic rollback to 0.18.2"
echo "$ERR_OUT" | grep -q "automatically rolled back to 0.18.2 successfully"
[[ -f "$INSTALL_DIR/bin/ikenga-server.prev-0.18.2" ]]
[[ "$(installed)" == "0.18.2" ]]
[[ -f "$SYSTEMD_DIR/ikenga-server-t1.service.prev-0.18.2" ]]
grep -q "unit version 0.18.2" "$SYSTEMD_DIR/ikenga-server-t1.service"
if grep -q "unit version 0.18.4" "$SYSTEMD_DIR/ikenga-server-t1.service"; then
  fail "the broken unit is still installed after rollback"
fi
[[ "$(health_version)" == "0.18.2" ]]
pass "TEST 4 (automatic rollback, exit 3)"

# From here the box is T1 (the t1 unit exists, no profile): requests live in
# operator/ and must be root-owned.
REQ_T1="$INSTALL_DIR/data/operator/update-request.json"
REQ_T0="$INSTALL_DIR/data/update-request.json"
CLAIM="$INSTALL_DIR/.update-claim/request.json"

# --- TEST 5: check-update writes available.json ---
echo "==> [Container] TEST 5: check-update..."
RELEASE_MANIFEST_URL="$(manifest 0.18.3)" "$PROVISION" check-update
AV="$STATE/available.json"
[[ "$(stat -c '%u %a' "$AV")" == "0 644" ]] || fail "available.json must be root 0644, is $(stat -c '%u %a' "$AV")"
python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$AV"
[[ "$(jget "$AV" schema)" == "ikenga-update-available/1" ]]
[[ "$(jget "$AV" installed)" == "0.18.2" ]]
[[ "$(jget "$AV" latest)" == "0.18.3" ]]
[[ "$(jget "$AV" blocked)" == "false" ]]
[[ "$(jget "$AV" notes_url)" == "https://github.com/ikenga-hq/ikenga/releases/tag/v0.18.3" ]]
[[ "$(jget "$AV" last_error)" == "null" ]]
[[ "$(installed)" == "0.18.2" ]] || fail "check-update installed something"
no_secret "$AV"
pass "TEST 5 (check-update)"

# --- TEST 6: min_upgrade_from above installed => blocked; packaging default ---
echo "==> [Container] TEST 6: blocked + packaging default..."
RELEASE_MANIFEST_URL="$(manifest 0.19.0)" "$PROVISION" check-update
[[ "$(jget "$AV" latest)" == "0.19.0" ]]
[[ "$(jget "$AV" blocked)" == "true" ]]
[[ "$(jget "$AV" blocked_reason)" == "requires 0.18.5 first" ]]
# 0.18.5's manifest is shaped like package-server.sh's new default (no floor).
RELEASE_MANIFEST_URL="$(manifest 0.18.5)" "$PROVISION" check-update
[[ "$(jget "$AV" blocked)" == "false" ]]
[[ "$(jget "$AV" min_upgrade_from)" == "null" ]]
if grep -q 'min_upgrade_from="$version"' /work/package-server.sh; then
  fail "package-server.sh still defaults min_upgrade_from to the release's own version"
fi
pass "TEST 6 (blocked; no floor by default)"

# --- TEST 7: manifest text is never executed ---
echo "==> [Container] TEST 7: manifest injection..."
rm -f /tmp/pwned
cat > "$REL/evil_manifest.json" << 'MANIFEST'
{ "schema": "ikenga-server-release/1", "version": "0.18.3$(touch /tmp/pwned)", "channel": "stable",
  "min_upgrade_from": "$(touch /tmp/pwned)",
  "artifacts": [ { "kind": "tarball", "arch": "amd64", "name": "x$(touch /tmp/pwned)", "sha256": "`touch /tmp/pwned`", "size": 1 } ] }
MANIFEST
RELEASE_MANIFEST_URL="$REL/evil_manifest.json" "$PROVISION" check-update
[[ "$(jget "$AV" last_error)" == "the release manifest could not be read" ]]
[[ "$(jget "$AV" latest)" == "0.18.5" ]] || fail "a bad check must keep the last good latest"
set +e
ERR_OUT="$(RELEASE_MANIFEST_URL="$REL/evil_manifest.json" "$PROVISION" upgrade --latest 2>&1)"
RET=$?
set -e
[[ $RET -ne 0 ]] && echo "$ERR_OUT" | grep -q "invalid version" || fail "evil manifest not refused: $ERR_OUT"
[[ ! -e /tmp/pwned ]] || fail "manifest text was executed"
pass "TEST 7 (manifest is data, not code)"

# Advertise 0.18.3 again for the request tests.
RELEASE_MANIFEST_URL="$(manifest 0.18.3)" "$PROVISION" check-update

# write_request <path> <version> [owner] [requested_at] [request_id]
write_request() {
  local path="$1" ver="$2" owner="${3:-root}"
  local at="${4:-$(date -u +%Y-%m-%dT%H:%M:%SZ)}" id="${5:-$(cat /proc/sys/kernel/random/uuid)}"
  printf '{"schema":"ikenga-update-request/1","version":"%s","request_id":"%s","requested_by":"ada","requested_at":"%s","acknowledged_open_terminals":0}\n' \
    "$ver" "$id" "$at" > "$path"
  chown "$owner" "$path"
  LAST_ID="$id"
}
ST="$STATE/status.json"

# expect_refused <label> [message substring]
expect_refused() {
  local label="$1" msg="${2:-}"
  RELEASE_MANIFEST_URL="$(manifest 0.18.3)" RELEASE_TARBALL_PATH="$(tarball 0.18.3)" "$PROVISION" apply-request >/dev/null
  [[ "$(jget "$ST" state)" == "refused" ]] || fail "$label: expected refused, got $(jget "$ST" state)"
  [[ -z "$msg" ]] || jget "$ST" message | grep -q "$msg" || fail "$label: message was '$(jget "$ST" message)'"
  [[ ! -e "$CLAIM" && ! -L "$CLAIM" ]] || fail "$label: claimed file left behind"
  [[ ! -e "$REQ_T1" && ! -L "$REQ_T1" ]] || fail "$label: request file left behind"
  [[ "$(installed)" == "0.18.2" ]] || fail "$label: something was installed"
  no_secret "$ST"
  if grep -q "pwned" "$ST"; then fail "$label: request content echoed into status.json"; fi
  echo "    refused: $label ($(jget "$ST" message))"
}

# --- TEST 8: forged / malformed requests are refused ---
echo "==> [Container] TEST 8: apply-request refusals..."
cp /etc/passwd "$TEST_DIR/passwd.before"
ln -s /etc/passwd "$REQ_T1"; expect_refused "symlink" "invalid request"
cmp -s /etc/passwd "$TEST_DIR/passwd.before" || fail "symlink target was modified"
write_request "$TEST_DIR/linked.json" 0.18.3; ln "$TEST_DIR/linked.json" "$REQ_T1"
expect_refused "hardlink" "invalid request"
write_request "$REQ_T1" 0.18.3 ikenga; expect_refused "wrong owner" "invalid request"
write_request "$REQ_T1" 0.18.3; python3 -c 'import sys; p=sys.argv[1]; s=open(p).read(); open(p,"w").write(s+" "*5000)' "$REQ_T1"
expect_refused "over 4 KiB" "invalid request"
mkfifo "$REQ_T1"; expect_refused "fifo" "invalid request"
echo 'not json {' > "$REQ_T1"; expect_refused "invalid json" "invalid request"
write_request "$REQ_T1" 0.18; expect_refused "bad version" "invalid request"
rm -f /tmp/pwned2
write_request "$REQ_T1" '0.18.3$(touch /tmp/pwned2)'; expect_refused "injection" "invalid request"
[[ ! -e /tmp/pwned2 ]] || fail "request content was executed"
write_request "$REQ_T1" 0.18.9; expect_refused "not advertised" "not the advertised update"
write_request "$REQ_T1" 0.18.3 root "$(date -u -d '-1 hour' +%Y-%m-%dT%H:%M:%SZ)"; expect_refused "stale" "expired"
# A downgrade: root advertises something older than what is installed.
cp "$AV" "$TEST_DIR/av.bak"
sed -i 's/"latest":"0.18.3"/"latest":"0.18.1"/' "$AV"
write_request "$REQ_T1" 0.18.1; expect_refused "downgrade" "older"
cp "$TEST_DIR/av.bak" "$AV"
pass "TEST 8 (forged requests refused, nothing installed, nothing echoed)"

# --- TEST 9: a valid request upgrades (T0: owned by the service user) ---
echo "==> [Container] TEST 9: valid request (T0)..."
printf 'TIER=t0\nPERIMETER=public-https\nCHANNEL=stable\n' > "$INSTALL_DIR/.profile.env"
chmod 0600 "$INSTALL_DIR/.profile.env"
write_request "$REQ_T0" 0.18.3 root
RELEASE_MANIFEST_URL="$(manifest 0.18.3)" "$PROVISION" apply-request >/dev/null
[[ "$(jget "$ST" state)" == "refused" ]] || fail "T0: a root-owned request must be refused"
write_request "$REQ_T0" 0.18.3 ikenga
RELEASE_MANIFEST_URL="$(manifest 0.18.3)" RELEASE_TARBALL_PATH="$(tarball 0.18.3)" "$PROVISION" apply-request
[[ "$(jget "$ST" state)" == "succeeded" ]] || { cat "$ST"; cat "$STATE/last-run.log"; fail "expected succeeded"; }
[[ "$(jget "$ST" from)" == "0.18.2" && "$(jget "$ST" to)" == "0.18.3" ]]
[[ "$(jget "$ST" request_id)" == "$LAST_ID" && "$(jget "$ST" requested_by)" == "ada" ]]
[[ "$(jget "$ST" rolled_back)" == "false" && "$(jget "$ST" exit_code)" == "0" ]]
python3 -c 'import json,sys; t=json.load(open(sys.argv[1]))["log_tail"]; assert isinstance(t,list) and t and len(t)<=40, t' "$ST"
[[ "$(stat -c '%u %a' "$STATE/last-run.log")" == "0 600" ]] || fail "last-run.log must be root 0600"
no_secret "$ST" "$STATE/last-run.log" "$AV"
[[ "$(installed)" == "0.18.3" && "$(health_version)" == "0.18.3" ]]
[[ "$(jget "$AV" installed)" == "0.18.3" ]] || fail "available.json not refreshed after the apply"
PID_BEFORE="$(cat /tmp/mock-ikenga.pid)"
write_request "$REQ_T0" 0.18.3 ikenga
RELEASE_MANIFEST_URL="$(manifest 0.18.3)" "$PROVISION" apply-request >/dev/null
[[ "$(jget "$ST" state)" == "noop" ]] || fail "a second identical request must be a noop"
[[ "$(cat /tmp/mock-ikenga.pid)" == "$PID_BEFORE" ]] || fail "a noop restarted the server"
rm -f "$INSTALL_DIR/.profile.env"
pass "TEST 9 (valid request applied; repeat is a noop)"

# --- TEST 10: a failing release rolls back; the same version cools down ---
echo "==> [Container] TEST 10: rolled back + cooldown..."
RELEASE_MANIFEST_URL="$(manifest 0.18.4)" "$PROVISION" check-update
[[ "$(jget "$AV" latest)" == "0.18.4" ]]
write_request "$REQ_T1" 0.18.4
HEALTH_TIMEOUT=3 RELEASE_MANIFEST_URL="$(manifest 0.18.4)" RELEASE_TARBALL_PATH="$(tarball 0.18.4)" "$PROVISION" apply-request
[[ "$(jget "$ST" state)" == "rolled_back" ]] || { cat "$ST"; fail "expected rolled_back"; }
[[ "$(jget "$ST" rolled_back)" == "true" && "$(jget "$ST" exit_code)" == "3" ]]
[[ "$(installed)" == "0.18.3" && "$(health_version)" == "0.18.3" ]]
write_request "$REQ_T1" 0.18.4
RELEASE_MANIFEST_URL="$(manifest 0.18.4)" "$PROVISION" apply-request >/dev/null
[[ "$(jget "$ST" state)" == "refused" ]] && jget "$ST" message | grep -q cooldown || fail "expected cooldown, got $(cat "$ST")"
[[ "$(installed)" == "0.18.3" ]]
pass "TEST 10 (rollback reported; retry cooled down)"

# --- TEST 11: tailnet boxes health-check their IKENGA_HOST ---
echo "==> [Container] TEST 11: health URL follows IKENGA_HOST..."
sed -i 's/^IKENGA_HOST=.*/IKENGA_HOST=127.0.0.2/' "$INSTALL_DIR/.env"
printf 'TIER=t1\nPERIMETER=tailnet\n' > "$INSTALL_DIR/.profile.env"; chmod 0600 "$INSTALL_DIR/.profile.env"
OUT="$(MOCK_HOST=127.0.0.2 HEALTH_TIMEOUT=8 RELEASE_MANIFEST_URL="$(manifest 0.18.5)" RELEASE_TARBALL_PATH="$(tarball 0.18.5)" \
  "$PROVISION" upgrade --to 0.18.5 2>&1)" || { echo "$OUT"; fail "tailnet upgrade did not succeed"; }
echo "$OUT" | grep -q "healthy at http://127.0.0.2:4000" || fail "health was not checked on IKENGA_HOST: $OUT"
[[ "$(installed)" == "0.18.5" && "$(health_version 127.0.0.2)" == "0.18.5" ]]
no_secret <(printf '%s' "$OUT")
sed -i 's/^IKENGA_HOST=.*/IKENGA_HOST=127.0.0.1/' "$INSTALL_DIR/.env"
rm -f "$INSTALL_DIR/.profile.env"
systemctl restart ikenga-server-t1
[[ "$(health_version)" == "0.18.5" ]]
pass "TEST 11 (tailnet health URL)"

# --- TEST 12: the manifest cannot swap the version ---
echo "==> [Container] TEST 12: manifest/--to mismatch..."
set +e
ERR_OUT="$(RELEASE_MANIFEST_URL="$(manifest 0.18.7)" "$PROVISION" upgrade --to 0.18.6 2>&1)"
RET=$?
set -e
[[ $RET -ne 0 ]] && echo "$ERR_OUT" | grep -q "not the requested 0.18.6" || fail "mismatch not refused: $ERR_OUT"
[[ "$(installed)" == "0.18.5" ]]
pass "TEST 12 (manifest version must equal --to)"

# --- TEST 13: one upgrade at a time ---
echo "==> [Container] TEST 13: upgrade lock..."
exec 7>"$STATE/upgrade.lock"
flock 7
set +e
ERR_OUT="$(RELEASE_MANIFEST_URL="$(manifest 0.18.7)" "$PROVISION" upgrade --to 0.18.7 2>&1)"
RET=$?
set -e
[[ $RET -ne 0 ]] && echo "$ERR_OUT" | grep -q "another upgrade is running" || fail "lock not honoured: $ERR_OUT"
RELEASE_MANIFEST_URL="$(manifest 0.18.7)" "$PROVISION" check-update
write_request "$REQ_T1" 0.18.7
RELEASE_MANIFEST_URL="$(manifest 0.18.7)" "$PROVISION" apply-request >/dev/null
[[ "$(jget "$ST" state)" == "refused" ]] && jget "$ST" message | grep -q "another upgrade" || fail "apply-request ignored the lock: $(cat "$ST")"
exec 7>&-
[[ "$(installed)" == "0.18.5" ]]
pass "TEST 13 (upgrade lock)"

# --- TEST 14: install-update-units ---
echo "==> [Container] TEST 14: install-update-units..."
OUT="$("$PROVISION" install-update-units --dry-run)"
for u in ikenga-update-check.service ikenga-update-check.timer ikenga-update.path ikenga-update.service; do
  echo "$OUT" | grep -q "write $SYSTEMD_DIR/$u" || fail "dry run does not show $u"
  [[ ! -e "$SYSTEMD_DIR/$u" ]] || fail "dry run wrote $u"
done
echo "$OUT" | grep -q "$IKENGA_PROVISION_STABLE" || fail "dry run does not show the stable copy"
[[ ! -e "$IKENGA_PROVISION_STABLE" ]] || fail "dry run wrote the stable copy"
"$PROVISION" install-update-units >/dev/null
cmp -s /work/provision.sh "$IKENGA_PROVISION_STABLE" || fail "stable copy differs"
[[ "$(stat -c '%u %a' "$IKENGA_PROVISION_STABLE")" == "0 755" ]]
grep -qx "PathExists=$REQ_T1" "$SYSTEMD_DIR/ikenga-update.path" || fail "path unit watches the wrong file"
grep -qx "ExecStart=$IKENGA_PROVISION_STABLE apply-request" "$SYSTEMD_DIR/ikenga-update.service"
grep -qx "ExecStart=$IKENGA_PROVISION_STABLE check-update" "$SYSTEMD_DIR/ikenga-update-check.service"
OUT="$("$PROVISION" install-update-units)"
echo "$OUT" | grep -q "no changes" || fail "install-update-units is not idempotent: $OUT"
pass "TEST 14 (update units)"

# --- TEST 15: health must report the TARGET version ---
echo "==> [Container] TEST 15: health requires the target version..."
set +e
ERR_OUT="$(HEALTH_TIMEOUT=3 RELEASE_MANIFEST_URL="$(manifest 0.18.7)" RELEASE_TARBALL_PATH="$(tarball 0.18.7)" \
  "$PROVISION" upgrade --to 0.18.7 2>&1)"
RET=$?
set -e
[[ $RET -eq 3 ]] || { echo "$ERR_OUT"; fail "a server answering with the wrong version must roll back (exit 3), got $RET"; }
[[ "$(installed)" == "0.18.5" && "$(health_version)" == "0.18.5" ]]
pass "TEST 15 (health checks the version)"

echo "==> [Container] ALL UPGRADE TESTS PASSED!"
