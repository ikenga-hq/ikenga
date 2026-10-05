#!/bin/bash
set -euo pipefail

TEST_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_DIR"' EXIT

export INSTALL_DIR="$TEST_DIR/opt/ikenga"
mkdir -p "$INSTALL_DIR/bin" "$INSTALL_DIR/data" "$TEST_DIR/releases"

# Create a mock initial ikenga-server binary (v0.18.2)
cat << 'BIN' > "$INSTALL_DIR/bin/ikenga-server"
#!/bin/bash
if [[ "${1:-}" == "--version" ]]; then
  echo "ikenga-server 0.18.2"
  exit 0
fi
echo "Starting mock server 0.18.2 on port 4000..."
python3 -c '
import http.server, socketserver, json

class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/api/health":
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(b"{\"ok\": true, \"name\": \"ikenga-server\", \"version\": \"0.18.2\", \"status\": \"ready\"}\n")
        else:
            self.send_response(404)
            self.end_headers()
    def log_message(self, format, *args):
        pass

socketserver.TCPServer.allow_reuse_address = True
with socketserver.TCPServer(("127.0.0.1", 4000), Handler) as httpd:
    httpd.serve_forever()
'
BIN
chmod +x "$INSTALL_DIR/bin/ikenga-server"

# Mock systemctl to control the server in the container
mkdir -p "$TEST_DIR/bin"
cat << 'SYSCTL' > "$TEST_DIR/bin/systemctl"
#!/bin/bash
PIDFILE="/tmp/mock-ikenga.pid"
case "$1" in
  restart|start)
    if [[ -f "$PIDFILE" ]]; then
      kill "$(cat "$PIDFILE")" 2>/dev/null || true
      rm -f "$PIDFILE"
    fi
    pkill -f "http.server" 2>/dev/null || true
    sleep 0.5
    "$INSTALL_DIR/bin/ikenga-server" >/dev/null 2>&1 &
    echo $! > "$PIDFILE"
    sleep 0.5
    ;;
  is-active)
    if [[ -f "$PIDFILE" ]] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
      exit 0
    else
      exit 1
    fi
    ;;
  daemon-reload)
    exit 0
    ;;
  *)
    exit 0
    ;;
esac
SYSCTL
chmod +x "$TEST_DIR/bin/systemctl"
export PATH="$TEST_DIR/bin:$PATH"

# Start the initial 0.18.2 mock server
systemctl start ikenga-server-t1

# Verify initial mock server responds to /api/health
curl -fsS http://127.0.0.1:4000/api/health | grep -q '"ok": true'
echo "==> [Container] Initial 0.18.2 mock server is healthy"

PROVISION="/work/provision.sh"

# Package a valid 0.18.3 release
STAGE_0183="$TEST_DIR/stage-0183"
mkdir -p "$STAGE_0183/bin"
cat << 'BIN' > "$STAGE_0183/bin/ikenga-server"
#!/bin/bash
if [[ "${1:-}" == "--version" ]]; then
  echo "ikenga-server 0.18.3"
  exit 0
fi
echo "Starting mock server 0.18.3 on port 4000..."
python3 -c '
import http.server, socketserver, json

class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/api/health":
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(b"{\"ok\": true, \"name\": \"ikenga-server\", \"version\": \"0.18.3\", \"status\": \"ready\"}\n")
        else:
            self.send_response(404)
            self.end_headers()
    def log_message(self, format, *args):
        pass

socketserver.TCPServer.allow_reuse_address = True
with socketserver.TCPServer(("127.0.0.1", 4000), Handler) as httpd:
    httpd.serve_forever()
'
BIN
chmod +x "$STAGE_0183/bin/ikenga-server"
tar -czf "$TEST_DIR/releases/ikenga-server_0.18.3_linux_amd64.tar.gz" -C "$STAGE_0183" bin
SHA_0183="$(sha256sum "$TEST_DIR/releases/ikenga-server_0.18.3_linux_amd64.tar.gz" | awk '{print $1}')"
SIZE_0183="$(wc -c < "$TEST_DIR/releases/ikenga-server_0.18.3_linux_amd64.tar.gz" | tr -d ' ')"

cat << MANIFEST > "$TEST_DIR/releases/ikenga-server_0.18.3_manifest.json"
{
  "schema": "ikenga-server-release/1",
  "version": "0.18.3",
  "tag": "v0.18.3",
  "commit": "0123456789abcdef0123456789abcdef01234567",
  "published_at": "2026-10-05T12:00:00Z",
  "channel": "stable",
  "glibc_floor": "2.31",
  "min_upgrade_from": "0.18.0",
  "artifacts": [
    {
      "kind": "tarball",
      "arch": "amd64",
      "name": "ikenga-server_0.18.3_linux_amd64.tar.gz",
      "sha256": "$SHA_0183",
      "size": $SIZE_0183
    }
  ]
}
MANIFEST

# --- TEST 1: --dry-run ---
echo "==> [Container] TEST 1: Testing --dry-run..."
OUTPUT="$(RELEASE_MANIFEST_URL="$TEST_DIR/releases/ikenga-server_0.18.3_manifest.json" \
  RELEASE_TARBALL_PATH="$TEST_DIR/releases/ikenga-server_0.18.3_linux_amd64.tar.gz" \
  "$PROVISION" upgrade --to 0.18.3 --dry-run)"
echo "$OUTPUT"
echo "$OUTPUT" | grep -q "\[dry-run\]"
echo "$OUTPUT" | grep -q "would: ikenga-server upgraded 0.18.2 -> 0.18.3"
# Verify binary was untouched
[[ "$("$INSTALL_DIR/bin/ikenga-server" --version)" == "ikenga-server 0.18.2" ]]
echo "==> [Container] TEST 1 PASSED: --dry-run verified cleanly"

# --- TEST 2: min-upgrade-from refusal ---
echo "==> [Container] TEST 2: Testing min-upgrade-from refusal..."
cat << MANIFEST > "$TEST_DIR/releases/ikenga-server_0.19.0_manifest.json"
{
  "schema": "ikenga-server-release/1",
  "version": "0.19.0",
  "tag": "v0.19.0",
  "commit": "0123456789abcdef0123456789abcdef01234567",
  "channel": "stable",
  "min_upgrade_from": "0.18.5",
  "artifacts": [
    {
      "kind": "tarball",
      "arch": "amd64",
      "name": "ikenga-server_0.19.0_linux_amd64.tar.gz",
      "sha256": "fake",
      "size": 100
    }
  ]
}
MANIFEST

set +e
ERR_OUT="$(RELEASE_MANIFEST_URL="$TEST_DIR/releases/ikenga-server_0.19.0_manifest.json" \
  "$PROVISION" upgrade --to 0.19.0 2>&1)"
RET=$?
set -e
if [[ $RET -ne 0 ]] && echo "$ERR_OUT" | grep -q "below min-upgrade-from 0.18.5"; then
  echo "==> [Container] TEST 2 PASSED: refused upgrade as expected ($ERR_OUT)"
else
  echo "ERROR: Expected refusal below min-upgrade-from, got code $RET, output: $ERR_OUT"
  exit 1
fi

# --- TEST 3: Open terminals refusal unless --force ---
echo "==> [Container] TEST 3: Testing open terminals refusal unless --force..."
set +e
ERR_OUT="$(IKENGA_TEST_OPEN_TERMINALS=2 \
  RELEASE_MANIFEST_URL="$TEST_DIR/releases/ikenga-server_0.18.3_manifest.json" \
  "$PROVISION" upgrade --to 0.18.3 2>&1)"
RET=$?
set -e
if [[ $RET -ne 0 ]] && echo "$ERR_OUT" | grep -q "2 open terminal(s) detected"; then
  echo "==> [Container] TEST 3a PASSED: refused due to open terminals without --force"
else
  echo "ERROR: Expected terminal refusal, got code $RET, output: $ERR_OUT"
  exit 1
fi

# With --force it should proceed in --dry-run
OUTPUT="$(IKENGA_TEST_OPEN_TERMINALS=2 \
  RELEASE_MANIFEST_URL="$TEST_DIR/releases/ikenga-server_0.18.3_manifest.json" \
  "$PROVISION" upgrade --to 0.18.3 --force --dry-run)"
echo "$OUTPUT" | grep -q "Warning: 2 open terminal(s) will be terminated"
echo "==> [Container] TEST 3b PASSED: --force accepted with open terminals"

# --- TEST 4: Automatic rollback on failing binary ---
echo "==> [Container] TEST 4: Testing automatic rollback with failing binary..."
STAGE_FAIL="$TEST_DIR/stage-fail"
mkdir -p "$STAGE_FAIL/bin"
# Failing binary: exits 1 or refuses health
cat << 'BIN' > "$STAGE_FAIL/bin/ikenga-server"
#!/bin/bash
if [[ "${1:-}" == "--version" ]]; then
  echo "ikenga-server 0.18.4-broken"
  exit 0
fi
echo "Crashing immediately as simulated broken binary..."
exit 1
BIN
chmod +x "$STAGE_FAIL/bin/ikenga-server"

tar -czf "$TEST_DIR/releases/ikenga-server_0.18.4_linux_amd64.tar.gz" -C "$STAGE_FAIL" bin
SHA_FAIL="$(sha256sum "$TEST_DIR/releases/ikenga-server_0.18.4_linux_amd64.tar.gz" | awk '{print $1}')"
SIZE_FAIL="$(wc -c < "$TEST_DIR/releases/ikenga-server_0.18.4_linux_amd64.tar.gz" | tr -d ' ')"

cat << MANIFEST > "$TEST_DIR/releases/ikenga-server_0.18.4_manifest.json"
{
  "schema": "ikenga-server-release/1",
  "version": "0.18.4",
  "tag": "v0.18.4",
  "commit": "0123456789abcdef0123456789abcdef01234567",
  "channel": "stable",
  "min_upgrade_from": "0.18.0",
  "artifacts": [
    {
      "kind": "tarball",
      "arch": "amd64",
      "name": "ikenga-server_0.18.4_linux_amd64.tar.gz",
      "sha256": "$SHA_FAIL",
      "size": $SIZE_FAIL
    }
  ]
}
MANIFEST

# Run the upgrade with HEALTH_TIMEOUT=3 to quickly trigger failure
set +e
ERR_OUT="$(HEALTH_TIMEOUT=3 \
  RELEASE_MANIFEST_URL="$TEST_DIR/releases/ikenga-server_0.18.4_manifest.json" \
  RELEASE_TARBALL_PATH="$TEST_DIR/releases/ikenga-server_0.18.4_linux_amd64.tar.gz" \
  "$PROVISION" upgrade --to 0.18.4 2>&1)"
RET=$?
set -e

echo "$ERR_OUT"
# Check that it detected health failure and rolled back
echo "$ERR_OUT" | grep -q "Initiating automatic rollback to 0.18.2"
echo "$ERR_OUT" | grep -q "automatically rolled back to 0.18.2 successfully"

# Verify that previous binary was kept
[[ -f "$INSTALL_DIR/bin/ikenga-server.prev-0.18.2" ]]

# Verify current binary is back to 0.18.2
CURRENT_VER="$("$INSTALL_DIR/bin/ikenga-server" --version)"
[[ "$CURRENT_VER" == "ikenga-server 0.18.2" ]]

# Verify service is running and healthy again
curl -fsS http://127.0.0.1:4000/api/health | grep -q '"version": "0.18.2"'
echo "==> [Container] TEST 4 PASSED: automatic rollback recovered 0.18.2 cleanly"

echo "==> [Container] ALL UPGRADE TESTS PASSED!"
