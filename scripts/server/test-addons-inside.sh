#!/bin/bash
# provision.sh ADDONS hook, run as root inside test-addons-container.sh's container.
# No real systemd: a mock systemctl starts a mock server that answers
# /api/health with its version (same pattern as test-upgrade-inside.sh).
set -euo pipefail

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
PROVISION=/work/provision.sh
ADD=/opt/addons          # root-owned, 755: the one place add-ons are allowed
MARKS=/run/addon-marks   # each add-on appends "<its path> <args>" here when it runs
OUT="$T/out"
ERR="$T/err"
CHECKS=0

fail() { echo "FAIL: $*" >&2; echo "--- stdout (tail) ---" >&2; tail -25 "$OUT" >&2 2>/dev/null || true; echo "--- stderr ---" >&2; cat "$ERR" >&2 2>/dev/null || true; exit 1; }
pass() { echo "==> [Container] $* PASSED"; }
ok() { local d="$1"; shift; CHECKS=$((CHECKS + 1)); "$@" || fail "$d"; }
no() { local d="$1"; shift; CHECKS=$((CHECKS + 1)); if "$@"; then fail "$d"; fi; }
out_has() { grep -qE -- "$1" "$OUT" "$ERR"; }
ran() { [[ -s "$MARKS/$1" ]]; }
marked_args() { cut -d' ' -f2- "$MARKS/$1" | tr -d '\n'; }

mkdir -p "$ADD" "$MARKS" "$T/keys"
chmod 0755 "$ADD"
echo "ssh-ed25519 AAAA test" > "$T/keys/k"
echo "tskey-fake" > "$T/keys/ts"; chmod 0600 "$T/keys/ts"

# mkaddon <name> <exit code> [mode] [dir]: a root-owned add-on that records its run.
mkaddon() {
  local name="$1" rc="$2" mode="${3:-755}" dir="${4:-$ADD}"
  mkdir -p "$dir"
  printf '#!/bin/sh\necho "$0 $*" >> "%s/%s"\nexit %s\n' "$MARKS" "$name" "$rc" > "$dir/$name.sh"
  chown root:root "$dir/$name.sh"; chmod "$mode" "$dir/$name.sh"
}
reset_marks() { rm -f "$MARKS"/*; }

# profile <name> [extra line...]: a minimal tailnet profile, root-owned 0600.
profile() {
  local f="$T/p-$1.env"; shift
  cat > "$f" <<PROFILE
HOSTNAME_WANT=t
TIER=t1
PERIMETER=tailnet
VERSION=0.19.4
ADMIN_USER=ops
ADMIN_SSH_KEYS_FILE=$T/keys/k
TS_AUTHKEY_FILE=$T/keys/ts
TS_HOSTNAME=t
PROFILE
  [[ $# -eq 0 ]] || printf '%s\n' "$@" >> "$f"
  chown root:root "$f"; chmod 0600 "$f"
  PF="$f"
}

RC=0
# prov [args]: the full provision run (dry), against $PF; sets RC, $OUT, $ERR.
prov() {
  if env -u ADDONS bash "$PROVISION" --profile "$PF" --dry-run --yes "$@" > "$OUT" 2> "$ERR"; then RC=0; else RC=$?; fi
}

# ============================================================ provision (dry run)

echo "==> [Container] TEST 1: a good add-on runs, gets --dry-run, and is in the summary"
reset_marks; mkaddon good 0
profile good "ADDONS=($ADD/good.sh)"
prov
ok "1: exit 0" test "$RC" -eq 0
ok "1: add-on ran" ran good
ok "1: add-on was passed --dry-run" test "$(marked_args good)" = "--dry-run"
ok "1: listed in the summary" out_has "would: add-on executed: $ADD/good.sh \(provision\)"
pass "TEST 1 (pass, --dry-run passed through)"

echo "==> [Container] TEST 2: a failing add-on is reported with its exit code, the run still summarises, and exits non-zero"
reset_marks; mkaddon fail7 7; mkaddon after 0
profile fail "ADDONS=($ADD/fail7.sh $ADD/after.sh)"
prov
ok "2: exit non-zero" test "$RC" -ne 0
ok "2: exit code is not 127 (an undefined helper)" test "$RC" -ne 127
ok "2: names the add-on and its real exit code 7" out_has "add-on failed \(provision\): $ADD/fail7.sh: exit code 7"
ok "2: no 'command not found'" bash -c "! grep -q 'command not found' '$OUT' '$ERR'"
ok "2: the Summary was still printed" out_has '^==> Summary'
ok "2: the final error line was printed" out_has 'add-on steps failed'
no "2: the next add-on did not run after a failure" ran after
pass "TEST 2 (failing exit code surfaced, rest stopped)"

echo "==> [Container] TEST 3: a missing add-on is reported, not a crash"
profile missing "ADDONS=($ADD/does-not-exist.sh)"
prov
ok "3: exit non-zero but not 127" bash -c "[[ $RC -ne 0 && $RC -ne 127 ]]"
ok "3: says not found" out_has "add-on refused \(provision\): $ADD/does-not-exist.sh: not found"
ok "3: Summary printed" out_has '^==> Summary'
pass "TEST 3 (missing file)"

echo "==> [Container] TEST 4: scripts root must not run are refused and do not run"
refused() {   # <label> <name> <why substring>   (profile already built for $2)
  reset_marks; prov
  ok "$1: exit non-zero" test "$RC" -ne 0
  ok "$1: refused with the reason" out_has "add-on refused \(provision\): .*$3"
  no "$1: did not run" ran "$2"
}
mkaddon ww 0 777;                   profile ww "ADDONS=($ADD/ww.sh)";     refused "777 root-owned" ww "group- or world-writable"
mkaddon gw 0 775;                   profile gw "ADDONS=($ADD/gw.sh)";     refused "group-writable" gw "group- or world-writable"
mkaddon other 0 755; chown 1000:1000 "$ADD/other.sh"
                                    profile other "ADDONS=($ADD/other.sh)"; refused "non-root owner" other "owned by uid 1000, not root"
mkaddon noexec 0 644;               profile noexec "ADDONS=($ADD/noexec.sh)"; refused "not executable" noexec "not executable"
mkaddon target 0; ln -sf "$ADD/target.sh" "$ADD/link.sh"
                                    profile link "ADDONS=($ADD/link.sh)"; refused "symlink" target "is a symlink"
mkdir -p "$ADD/dir.sh"
                                    profile isdir "ADDONS=($ADD/dir.sh)"; refused "directory" isdir "not a regular file"
mkaddon inww 0 755 /opt/wwdir; chmod 777 /opt/wwdir
                                    profile inww "ADDONS=(/opt/wwdir/inww.sh)"; refused "world-writable parent directory" inww "directory /opt/wwdir is group- or world-writable"
mkdir -p /opt/userdir; chown 1000:1000 /opt/userdir; chmod 755 /opt/userdir; mkaddon inuser 0 755 /opt/userdir
                                    profile inuser "ADDONS=(/opt/userdir/inuser.sh)"; refused "non-root parent directory" inuser "directory /opt/userdir is owned by uid 1000"
ln -sfn /opt/userdir /opt/linkdir
                                    profile linkdir "ADDONS=(/opt/linkdir/inuser.sh)"; refused "parent directory symlinked to a user's directory" inuser "directory /opt/linkdir is owned by uid 1000|directory /opt/userdir is owned by uid 1000"
pass "TEST 4 (777, group-writable, non-root owner, not executable, symlink, directory, bad parents)"

echo "==> [Container] TEST 5: a relative path is refused, even when a script by that name sits in the current directory"
reset_marks; mkaddon rel 0
profile rel "ADDONS=(rel.sh)"
( cd "$ADD" && env -u ADDONS bash "$PROVISION" --profile "$PF" --dry-run --yes > "$OUT" 2> "$ERR" ) && RC=0 || RC=$?
ok "5: exit non-zero" test "$RC" -ne 0
ok "5: says not an absolute path" out_has "add-on refused \(provision\): rel.sh: not an absolute path"
no "5: did not run" ran rel
profile dotrel "ADDONS=(./rel.sh)"
( cd "$ADD" && env -u ADDONS bash "$PROVISION" --profile "$PF" --dry-run --yes > "$OUT" 2> "$ERR" ) && RC=0 || RC=$?
ok "5b: ./rel.sh refused too" out_has "not an absolute path"
no "5b: did not run" ran rel
pass "TEST 5 (relative path)"

echo "==> [Container] TEST 6: a dry run never changes a script's mode, and never makes one executable"
reset_marks; mkaddon m644 0 644; mkaddon m755 0 755; mkaddon m700 0 700
for n in m644 m755 m700; do
  profile "mode-$n" "ADDONS=($ADD/$n.sh)"
  before="$(stat -c '%a %U' "$ADD/$n.sh")"
  prov
  ok "6: $n mode and owner unchanged by a dry run" test "$(stat -c '%a %U' "$ADD/$n.sh")" = "$before"
done
ok "6: the 644 script stays 644" test "$(stat -c '%a' "$ADD/m644.sh")" = "644"
ok "6: and did not run" bash -c "! [[ -s $MARKS/m644 ]]"
pass "TEST 6 (no chmod)"

echo "==> [Container] TEST 7: ADDONS unset behaves as origin/main would, and a caller's env ADDONS is ignored"
reset_marks; mkaddon envone 0
profile none
prov
ok "7: exit 0" test "$RC" -eq 0
no "7: no add-on text in the output" out_has 'add-on|Add-on'
cp "$OUT" "$T/out-none"
ADDONS="$ADD/envone.sh" bash "$PROVISION" --profile "$PF" --dry-run --yes > "$OUT" 2> "$ERR" && RC=0 || RC=$?
ok "7: env ADDONS exit 0" test "$RC" -eq 0
no "7: env ADDONS did not run" ran envone
ok "7: output identical with and without the env var" diff -q "$OUT" "$T/out-none"
pass "TEST 7 (unset = no-op; env ignored)"

echo "==> [Container] TEST 8: the space-separated string form, and a path with a space in array form"
reset_marks; mkaddon s1 0; mkaddon s2 0; mkaddon "with space" 0 755 "$ADD/sp dir"
profile str "ADDONS=\"$ADD/s1.sh $ADD/s2.sh\""
prov
ok "8: string form exit 0" test "$RC" -eq 0
ok "8: both ran" bash -c "[[ -s $MARKS/s1 && -s $MARKS/s2 ]]"
profile arrsp "ADDONS=(\"$ADD/sp dir/with space.sh\")"
prov
ok "8: array form with a space exit 0" test "$RC" -eq 0
ok "8: it ran" ran "with space"
pass "TEST 8 (string form; path with a space)"

# ============================================================ sync-accounts (dry run)

echo "==> [Container] TEST 9: sync-accounts runs the add-ons too (dry run)"
reset_marks; mkaddon sa 0
profile sa "ADDONS=($ADD/sa.sh)"
if env -u ADDONS bash "$PROVISION" sync-accounts --profile "$PF" --dry-run > "$OUT" 2> "$ERR"; then RC=0; else RC=$?; fi
ok "9: exit 0" test "$RC" -eq 0
ok "9: add-on ran with --dry-run" test "$(marked_args sa)" = "--dry-run"
mkaddon sa 3
if env -u ADDONS bash "$PROVISION" sync-accounts --profile "$PF" --dry-run > "$OUT" 2> "$ERR"; then RC=0; else RC=$?; fi
ok "9: a failing add-on exits non-zero" test "$RC" -ne 0
ok "9: with its exit code named" out_has "add-on failed \(sync-accounts\): $ADD/sa.sh: exit code 3"
pass "TEST 9 (sync-accounts)"

# ============================================================ upgrade

export INSTALL_DIR="$T/opt/ikenga"
export IKENGA_UPDATE_STATE_DIR="$T/state"
export IKENGA_PROVISION_STABLE="$T/sbin/ikenga-provision"
export IKENGA_SYSTEMD_DIR="$T/systemd"
STATE="$IKENGA_UPDATE_STATE_DIR"
REL="$T/releases"
mkdir -p "$INSTALL_DIR/bin" "$INSTALL_DIR/data" "$REL" "$IKENGA_SYSTEMD_DIR" "$T/bin"
id ikenga >/dev/null 2>&1 || useradd --system --no-create-home ikenga
printf 'IKENGA_HOST=127.0.0.1\n' > "$INSTALL_DIR/.env"; chmod 0600 "$INSTALL_DIR/.env"

make_mock() {   # <bin> <--version> <health version>
  mkdir -p "$(dirname "$1")"
  cat > "$1" <<BIN
#!/bin/bash
if [[ "\${1:-}" == "--version" ]]; then echo "ikenga-server $2"; exit 0; fi
exec python3 -c '
import http.server, socketserver, json, sys
body = (json.dumps({"ok": True, "name": "ikenga-server", "version": sys.argv[1], "status": "ready"}) + "\n").encode()
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200 if self.path == "/api/health" else 404)
        self.send_header("Content-Type", "application/json"); self.end_headers()
        if self.path == "/api/health": self.wfile.write(body)
    def log_message(self, *a): pass
socketserver.TCPServer.allow_reuse_address = True
with socketserver.TCPServer(("127.0.0.1", 4000), H) as s: s.serve_forever()
' "$3"
BIN
  chmod +x "$1"
}
make_release() {   # <version>
  local ver="$1" stage="$T/stage-$1"
  make_mock "$stage/bin/ikenga-server" "$ver" "$ver"
  local tb="$REL/ikenga-server_${ver}_linux_amd64.tar.gz"
  tar -czf "$tb" -C "$stage" bin
  cat > "$REL/ikenga-server_${ver}_manifest.json" <<MANIFEST
{ "schema": "ikenga-server-release/1", "version": "$ver", "tag": "v$ver",
  "commit": "0123456789abcdef0123456789abcdef01234567", "published_at": "2026-10-05T12:00:00Z",
  "channel": "stable", "glibc_floor": "2.31", "min_upgrade_from": null,
  "artifacts": [ { "kind": "tarball", "arch": "amd64", "name": "ikenga-server_${ver}_linux_amd64.tar.gz",
                   "sha256": "$(sha256sum "$tb" | awk '{print $1}')", "size": $(wc -c < "$tb" | tr -d ' ') } ] }
MANIFEST
}
cat > "$T/bin/systemctl" <<'SYSCTL'
#!/bin/bash
PIDFILE="/tmp/mock-ikenga.pid"
case "$1" in
  restart|start)
    [[ "$*" == *ikenga-server* ]] || exit 0
    if [[ -f "$PIDFILE" ]]; then kill "$(cat "$PIDFILE")" 2>/dev/null || true; rm -f "$PIDFILE"; fi
    sleep 0.5
    "$INSTALL_DIR/bin/ikenga-server" >/dev/null 2>&1 8>&- 9>&- &
    echo $! > "$PIDFILE"; sleep 0.5 ;;
  *) exit 0 ;;
esac
SYSCTL
chmod +x "$T/bin/systemctl"; export PATH="$T/bin:$PATH"
make_mock "$INSTALL_DIR/bin/ikenga-server" 0.18.2 0.18.2
make_release 0.18.3; make_release 0.18.4
systemctl start ikenga-server
installed() { "$INSTALL_DIR/bin/ikenga-server" --version | awk '{print $NF}'; }
up() {   # <version> [args]: provision.sh upgrade against the local release; sets RC
  local v="$1"; shift
  if RELEASE_MANIFEST_URL="$REL/ikenga-server_${v}_manifest.json" RELEASE_TARBALL_PATH="$REL/ikenga-server_${v}_linux_amd64.tar.gz" \
       bash "$PROVISION" upgrade --to "$v" "$@" > "$OUT" 2> "$ERR"; then RC=0; else RC=$?; fi
}
set_profile() { printf 'TIER=t0\nPERIMETER=public-https\nCHANNEL=stable\n%s\n' "$@" > "$INSTALL_DIR/.profile.env"; chown root:root "$INSTALL_DIR/.profile.env"; chmod 0600 "$INSTALL_DIR/.profile.env"; }

echo "==> [Container] TEST 10: upgrade --dry-run runs the add-ons with --dry-run (and changes nothing)"
reset_marks; mkaddon up 0
set_profile "ADDONS=($ADD/up.sh)"
up 0.18.3 --dry-run
ok "10: exit 0" test "$RC" -eq 0
ok "10: the add-on ran on the upgrade dry run" ran up
ok "10: with --dry-run" test "$(marked_args up)" = "--dry-run"
ok "10: listed as would:" out_has "would: add-on executed: $ADD/up.sh \(upgrade\)"
ok "10: nothing was installed" test "$(installed)" = "0.18.2"
mkaddon up 9
up 0.18.3 --dry-run
ok "10: a failing add-on on the dry run exits 5" test "$RC" -eq 5
ok "10: names the exit code" out_has "add-on failed \(upgrade\): $ADD/up.sh: exit code 9"
ok "10: still nothing installed" test "$(installed)" = "0.18.2"
pass "TEST 10 (upgrade dry run)"

echo "==> [Container] TEST 11: a real upgrade runs the add-on after the new version is healthy; a failure exits 5 and says so"
reset_marks; mkaddon up 0
set_profile "ADDONS=($ADD/up.sh)"
up 0.18.3
ok "11: exit 0" test "$RC" -eq 0
ok "11: upgraded" test "$(installed)" = "0.18.3"
ok "11: add-on ran WITHOUT --dry-run" test "$(marked_args up)" = ""
reset_marks; mkaddon up 4
up 0.18.4
ok "11: add-on failure after a good upgrade exits 5 (not 1, not 127, not 3/4)" test "$RC" -eq 5
ok "11: the new version is live" test "$(installed)" = "0.18.4"
ok "11: says so" out_has "ikenga-server 0.18.4 is installed and healthy, but an add-on script failed"
ok "11: names the exit code" out_has "add-on failed \(upgrade\): $ADD/up.sh: exit code 4"
pass "TEST 11 (real upgrade; exit 5)"

echo "==> [Container] TEST 12: apply-request reports an add-on failure as itself, not as 'did not start'"
# Back to 0.18.2 so the advertised update (0.18.3) is applicable.
make_mock "$INSTALL_DIR/bin/ikenga-server" 0.18.2 0.18.2; systemctl restart ikenga-server
mkaddon up 6; reset_marks
set_profile "ADDONS=($ADD/up.sh)"
RELEASE_MANIFEST_URL="$REL/ikenga-server_0.18.3_manifest.json" bash "$PROVISION" check-update
printf '{"schema":"ikenga-update-request/1","version":"0.18.3","request_id":"%s","requested_by":"ada","requested_at":"%s","acknowledged_open_terminals":0}\n' \
  "$(cat /proc/sys/kernel/random/uuid)" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$INSTALL_DIR/data/update-request.json"
chown ikenga "$INSTALL_DIR/data/update-request.json"
RELEASE_MANIFEST_URL="$REL/ikenga-server_0.18.3_manifest.json" RELEASE_TARBALL_PATH="$REL/ikenga-server_0.18.3_linux_amd64.tar.gz" \
  bash "$PROVISION" apply-request > "$OUT" 2> "$ERR" || true
ST="$STATE/status.json"
jget() { python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' "$ST" "$1"; }
ok "12: the new version is installed" test "$(installed)" = "0.18.3"
ok "12: state is failed" test "$(jget state)" = "failed"
ok "12: exit_code is 5" test "$(jget exit_code)" = "5"
ok "12: message is not the false 'did not start'" bash -c "! python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[\"message\"])' '$ST' | grep -q 'did not start'"
ok "12: message names the add-on failure" bash -c "python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[\"message\"])' '$ST' | grep -q 'add-on script failed'"
ok "12: rolled_back is false" test "$(jget rolled_back)" = "False"
pass "TEST 12 (apply-request status)"

echo "==> [Container] All $CHECKS checks passed."
