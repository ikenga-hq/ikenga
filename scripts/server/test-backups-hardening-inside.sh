#!/bin/bash
# Regression checks for the security fixes to `provision.sh backups`, run as root
# inside an ubuntu:24.04 container by test-backups-hardening-container.sh, either
# with a mock systemctl or with real systemd as PID 1 (detected below).
#
#   B1  root never follows or writes through a path the backup user controls
#         - the backup user cannot swap status/ or private/ for a symlink at all
#         - a symlink at the key / status.json / lock / errors / gcloud path is
#           REFUSED (not adopted) and the root-owned target is left untouched
#         - provisioning converges, rotates the key, disables and re-enables
#           with root's DAC bypass (CAP_DAC_OVERRIDE, CAP_DAC_READ_SEARCH)
#           removed, i.e. root needs no power over anything inside the user's dirs
#   (a) disable / key rotation remove the gcloud credentials (real google-cloud-cli,
#       a locally generated service-account key, a local token stub)
#   (b) schedules that start together do not share a gcloud config dir
#   (c) a database without "enabled": true is skipped
#   (d) BACKUP_PG_MAJOR below 17 is refused
#   (f) the README says what disable removes
#
# The checks are SOFT: every one runs, then a summary is printed, so a run against
# an older provision.sh lists every check that fails there. Every secret is fake.
set -uo pipefail

if [[ -n "${KEEP_APT_ARCHIVES:-}" ]]; then
  rm -f /etc/apt/apt.conf.d/docker-clean
  echo 'Binary::apt::APT::Keep-Downloaded-Packages "true"; APT::Keep-Downloaded-Packages "true";' > /etc/apt/apt.conf.d/99keep
fi
export DEBIAN_FRONTEND=noninteractive
T="$(mktemp -d)"
PROV=/root/prov
PROVISION=/work/provision.sh
PROFILE="$PROV/profile.env"
SECRETS="$PROV/secrets"
CONFIG_SRC="$PROV/backup-config.json"
OUT="$T/out"; ALL="$T/all-output"; : > "$ALL"
BU=ikenga-backup
ETC=/etc/ikenga-backup
STATE=/var/lib/ikenga-backup
PRIV=$STATE/private
UNITS=/etc/systemd/system
SVC=$UNITS/ikenga-backup@.service
FAKELOG=/srv/fake-gcs/calls.log
STUB_PORT=58123
REAL_SYSTEMD=0; [[ "$(cat /proc/1/comm 2>/dev/null)" == systemd ]] && REAL_SYSTEMD=1

PASSN=0; FAILN=0; FAILED=()
check() {   # name, then a function/command that returns 0 on pass and says why on stderr when not
  local name="$1" why; shift
  if why="$("$@" 2>&1)"; then
    echo "==> [Hardening] $name PASSED"; PASSN=$((PASSN + 1))
    [[ -z "$why" ]] || echo "$why" | sed 's/^/        /'
  else
    echo "==> [Hardening] $name FAILED"; FAILN=$((FAILN + 1)); FAILED+=("$name")
    echo "$why" | sed 's/^/        /'
  fi
}
die_setup() { echo "SETUP FAILED: $*" >&2; [[ -f "$OUT" ]] && tail -30 "$OUT" >&2; exit 2; }

echo "==> [Hardening] mode: $([[ $REAL_SYSTEMD -eq 1 ]] && echo 'real systemd as PID 1' || echo 'mock systemctl'); provision.sh = $PROVISION"

# ------------------------------------------------------------------ host

PRINC=/var/lib/ikenga-test/principals
mkacct() { groupadd -g "$2" "ik-$1"; useradd -u "$2" -g "$2" -M -d "$PRINC/$2/home" -s /bin/sh "ik-$1"; install -d -m 0700 -o "$2" -g "$2" "$PRINC/$2/home"; }
mkdir -p "$PRINC"; mkacct ada 20001; mkacct rex 20003; useradd -m ops
as_bu() { ( cd / && setpriv --reuid="$(id -u $BU)" --regid="$(id -g $BU)" --clear-groups env -i HOME=$PRIV PATH=/usr/local/bin:/usr/bin:/bin "$@" ); }
as_acct() { local u; u="$(id -u "ik-$1")"; shift; ( cd / && setpriv --reuid="$u" --regid="$u" --clear-groups env -i PATH=/usr/local/bin:/usr/bin:/bin "$@" ); }

apt-get update -qq >/dev/null 2>&1 || die_setup "apt-get update"
apt-get install -y -qq --no-install-recommends jq openssl python3 >/dev/null 2>&1 || die_setup "could not install jq/openssl/python3"

if [[ $REAL_SYSTEMD -eq 0 ]]; then
  # A mock systemctl: `start <unit>` runs the unit's own ExecStart as its User with its Environment=.
  mkdir -p /var/lib/mock-systemctl/enabled /var/lib/mock-systemctl/active /var/log/mock-journal
  cat > /usr/local/bin/systemctl <<'MOCK'
#!/bin/bash
D=/var/lib/mock-systemctl
cmd="${1:-}"; shift || true
units=(); for a in "$@"; do [[ "$a" == -* ]] || units+=("$a"); done
now=0; for a in "$@"; do [[ "$a" == --now ]] && now=1; done
case "$cmd" in
  enable) for u in "${units[@]}"; do touch "$D/enabled/$u"; [[ $now -eq 1 ]] && touch "$D/active/$u"; done ;;
  disable) for u in "${units[@]}"; do rm -f "$D/enabled/$u"; [[ $now -eq 1 ]] && rm -f "$D/active/$u"; done ;;
  restart) for u in "${units[@]}"; do touch "$D/active/$u"; done ;;
  is-enabled) [[ -e "$D/enabled/${units[0]}" ]]; exit $? ;;
  is-active) [[ -e "$D/active/${units[0]}" ]]; exit $? ;;
  start)
    u="${units[0]}"; base="${u%.service}"; inst="${base#*@}"; tmpl="/etc/systemd/system/${base%%@*}@.service"
    [[ -f "$tmpl" ]] || { echo "Unit $u not found." >&2; exit 5; }
    user="$(sed -n 's/^User=//p' "$tmpl")"; group="$(sed -n 's/^Group=//p' "$tmpl")"
    mapfile -t envs < <(sed -n 's/^Environment=//p' "$tmpl" | sed "s/%i/$inst/g")
    read -ra exe <<<"$(sed -n 's/^ExecStart=//p' "$tmpl" | sed "s/%i/$inst/g")"
    ( cd / && umask 0077 && exec setpriv --reuid="$(id -u "$user")" --regid="$(id -g "$group")" --clear-groups \
        env -i "${envs[@]}" "${exe[@]}" ) >> "/var/log/mock-journal/$u.log" 2>&1
    exit $? ;;
esac
exit 0
MOCK
  chmod +x /usr/local/bin/systemctl
fi
units_start() { systemctl start "$1" >"$T/start.out" 2>&1; }

# ---------------------------------------------------------------- fixtures

# Real RSA service-account keys, generated here. token_uri points at a local stub
# so the real gcloud can activate one with no Google endpoint involved.
mkkey() {   # key id, output: base64 -w0 of the key JSON
  openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out "$T/k.pem" 2>/dev/null || return 1
  jq -nc --arg kid "$1" --rawfile pk "$T/k.pem" --arg tu "http://127.0.0.1:$STUB_PORT/token" \
    '{type:"service_account",project_id:"fake-project",private_key_id:$kid,private_key:$pk,client_email:"backup@fake-project.iam.gserviceaccount.com",client_id:"1",token_uri:$tu}' | base64 -w0
}
KID1=HARDENKEYONE1111; KID2=HARDENKEYTWO2222
KEY1="$(mkkey $KID1)"; KEY2="$(mkkey $KID2)"
[[ -n "$KEY1" && -n "$KEY2" ]] || die_setup "could not generate keys"

cat > "$T/stub.py" <<PY
import http.server, json
class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        self.rfile.read(int(self.headers.get('Content-Length', 0)))
        b = json.dumps({"access_token": "ya29.FAKE-token", "expires_in": 3600, "token_type": "Bearer"}).encode()
        self.send_response(200); self.send_header('Content-Type', 'application/json'); self.send_header('Content-Length', str(len(b))); self.end_headers(); self.wfile.write(b)
    do_GET = do_POST
    def log_message(self, *a): pass
http.server.HTTPServer(('127.0.0.1', $STUB_PORT), H).serve_forever()
PY
python3 "$T/stub.py" & STUB_PID=$!
trap 'kill $STUB_PID 2>/dev/null || true' EXIT

install -d -m 0700 "$PROV"
cat > "$CONFIG_SRC" <<'JSON'
{
  "databases": [
    { "name": "h-daily",  "connection_secret": "H_ONE_URL", "schedule": "daily",   "gcs_bucket": "bucket-a", "enabled": true },
    { "name": "h-fast",   "connection_secret": "H_ONE_URL", "schedule": "4hourly", "gcs_bucket": "bucket-a", "enabled": true },
    { "name": "h-weekly", "connection_secret": "H_ONE_URL", "schedule": "weekly",  "gcs_bucket": "bucket-a", "enabled": true },
    { "name": "h-unset",  "connection_secret": "H_ONE_URL", "schedule": "daily",   "gcs_bucket": "bucket-a" }
  ]
}
JSON
chmod 0600 "$CONFIG_SRC"
ENABLED=1; PGMAJ=""
write_secrets() {   # key b64 (default KEY1)
  printf '%s\n' "[backup] H_ONE_URL=postgres://hu:FAKE-pw@127.0.0.1:55999/hdb?sslmode=disable&connect_timeout=2" "[root] GCS_KEY_B64=${1:-$KEY1}" > "$SECRETS"
  chmod 0600 "$SECRETS"
}
write_profile() {
  { echo 'ADMIN_USER=ops'; echo 'ACCOUNTS=(ada rex)'; echo 'AGENT_ACCOUNTS=(rex)'; echo "SECRETS_FILE=$SECRETS"
    echo "BACKUPS_ENABLED=$ENABLED"; echo "BACKUP_CONFIG=$CONFIG_SRC"; echo 'BACKUP_GCS_KEY_SECRET=GCS_KEY_B64'
    [[ -z "$PGMAJ" ]] || echo "BACKUP_PG_MAJOR=$PGMAJ"; } > "$PROFILE"
  chmod 0600 "$PROFILE"
}
write_secrets; write_profile
RC=0
prov() { RC=0; "$PROVISION" backups --profile "$PROFILE" "$@" >"$OUT" 2>&1 || RC=$?; cat "$OUT" >> "$ALL"; }
# Provisioning with root's DAC bypass removed: root is then an ordinary uid 0 that
# cannot open, create or traverse anything its owner/group/other bits deny. chown,
# chmod and friends keep working (CAP_CHOWN, CAP_FOWNER stay).
NODAC=(setpriv --bounding-set=-dac_override,-dac_read_search)
prov_nodac() { RC=0; "${NODAC[@]}" "$PROVISION" backups --profile "$PROFILE" "$@" >"$OUT" 2>&1 || RC=$?; cat "$OUT" >> "$ALL"; }

# ------------------------------------------------- first real provisioning

echo "==> [Hardening] first provisioning (real PGDG + Google Cloud SDK packages)..."
prov
[[ $RC -eq 0 ]] || die_setup "first provisioning exited $RC"
command -v gcloud >/dev/null && gcloud --version >/dev/null 2>&1 || die_setup "real gcloud missing"
REAL_GCLOUD="$(command -v gcloud)"

# The fake gcloud shadows the real one on the service's PATH (/usr/local/bin comes
# first). Its activation takes a lock in CLOUDSDK_CONFIG, like gcloud's sqlite
# files do: a second activation into the same dir while one runs is refused.
install -d -m 1777 /srv/fake-gcs
cat > /usr/local/bin/gcloud <<'SHIM'
#!/bin/bash
echo "gcloud $* | CLOUDSDK_CONFIG=${CLOUDSDK_CONFIG:-unset} uid=$(id -u)" >> /srv/fake-gcs/calls.log
[[ -n "${CLOUDSDK_CONFIG:-}" ]] || exit 1
if [[ "${1:-}" == auth && "${2:-}" == activate-service-account ]]; then
  mkdir -p "$CLOUDSDK_CONFIG"
  mkdir "$CLOUDSDK_CONFIG/.activate-lock" 2>/dev/null || { echo "ERROR: database is locked" >&2; exit 1; }
  sleep 1.5
  : > "$CLOUDSDK_CONFIG/active-account"
  rmdir "$CLOUDSDK_CONFIG/.activate-lock"
  exit 0
fi
[[ "${1:-}" == storage && "${2:-}" == cp ]] && exit 0
exit 2
SHIM
chmod +x /usr/local/bin/gcloud

# One unit run per schedule so errors/, work/, gcloud/<schedule>/ and the status
# file all exist for the checks that tamper with them. (No Postgres server here:
# each database ends in dump-failed, which is fine, activation comes first.)
for s in daily 4hourly weekly; do units_start ikenga-backup@$s.service || true; done
SW=$STATE/status/status.json; [[ -d $STATE/status ]] || SW=$STATE/status.json     # older layout: no status/ dir
SLOCK=$(dirname "$SW")/.status.lock

# ----------------------------------------------------------- B1: layout

t_layout() {
  local f=0
  [[ "$(stat -c '%U:%a' $STATE)" == "root:755" ]] || { echo "$STATE is $(stat -c '%U:%a' $STATE), not root:755"; f=1; }
  as_bu sh -c "touch $STATE/planted" 2>/dev/null && { echo "the backup user can create files directly in $STATE"; rm -f $STATE/planted; f=1; }
  as_bu sh -c "mv $PRIV $PRIV.x" 2>/dev/null && { echo "the backup user can rename $PRIV"; as_bu mv $PRIV.x $PRIV; f=1; }
  [[ "$(stat -c '%U:%a' $PRIV)" == "$BU:700" ]] || { echo "$PRIV is $(stat -c '%U:%a' $PRIV)"; f=1; }
  [[ "$(as_acct ada cat $STATE/status.json | jq -r '.enabled')" == true ]] || { echo "an account cannot read $STATE/status.json"; f=1; }
  return $f
}
check "B1 layout: $STATE is root's alone; the user cannot create or rename anything in it; status.json stays readable at its documented path" t_layout

# ------------------------------------- B1: symlink swaps and refusals

victim_reset() {
  rm -rf /opt/victim-dir /opt/victim-file; install -d -m 0755 /opt/victim-dir; echo sentinel > /opt/victim-dir/sentinel
  echo root-only-content > /opt/victim-file; chmod 0644 /opt/victim-file
}
victim_sig() { { stat -c '%n %u:%g %a' /opt/victim-dir /opt/victim-dir/sentinel /opt/victim-file; ls -A /opt/victim-dir; sha256sum /opt/victim-file /opt/victim-dir/sentinel; } 2>&1; }
# sym_case <path> <target> : as the backup user make <path> a symlink to a
# root-owned target; if the user cannot, the attack is blocked (pass). Otherwise
# both an enabled and a disabled run must refuse, and leave the target untouched.
sym_case() {
  local path="$1" target="$2" before after f=0 swap
  victim_reset; before="$(victim_sig)"
  if ! as_bu sh -c "{ [ ! -e '$path' ] || mv '$path' '$path.real'; } && ln -s '$target' '$path'" 2>"$T/swap.err"; then
    [[ ! -L "$path" ]] || { echo "swap failed but $path is a symlink"; return 1; }
    echo "blocked at the swap itself: $(head -1 "$T/swap.err")"
    return 0
  fi
  prov;  [[ $RC -ne 0 ]] || { echo "an enabled run accepted a symlink at $path (rc 0)"; f=1; }
  grep -q 'symbolic link' "$OUT" || { echo "the enabled run did not refuse on 'symbolic link'; output: $(tail -3 "$OUT" | tr '\n' ' ')"; f=1; }
  ENABLED=0 write_profile; prov; ENABLED=1 write_profile
  [[ $RC -ne 0 ]] || { echo "a disabled run accepted a symlink at $path (rc 0)"; f=1; }
  after="$(victim_sig)"
  [[ "$after" == "$before" ]] || { echo "the root-owned target was modified:"; diff <(echo "$before") <(echo "$after") | head -8; f=1; }
  # put things back (as the user: the path is its own)
  as_bu sh -c "rm -f '$path'; { [ ! -e '$path.real' ] || mv '$path.real' '$path'; }" 2>/dev/null || true
  [[ $f -eq 0 ]] && echo "refused; target untouched"
  return $f
}
check "B1 symlink: $PRIV -> root-owned dir (the verifier's escalation: chown of the target to the backup user)" sym_case $PRIV /opt/victim-dir
check "B1 symlink: $STATE/status -> root-owned dir" sym_case $STATE/status /opt/victim-dir
check "B1 symlink: the status file -> root-owned file" sym_case "$SW" /opt/victim-file
check "B1 symlink: the status lock -> root-owned file" sym_case "$SLOCK" /opt/victim-file
check "B1 symlink: $PRIV/gcs-key.json -> root-owned file" sym_case $PRIV/gcs-key.json /opt/victim-file
check "B1 symlink: $PRIV/errors -> root-owned dir" sym_case $PRIV/errors /opt/victim-dir
check "B1 symlink: $PRIV/gcloud -> root-owned dir" sym_case $PRIV/gcloud /opt/victim-dir
check "B1 symlink: $PRIV/work -> root-owned dir" sym_case $PRIV/work /opt/victim-dir

t_still_converges() {   # the tampering above left nothing behind that blocks a normal run
  prov; [[ $RC -eq 0 ]] || { echo "a clean run after the symlink cases exited $RC: $(tail -3 $OUT | tr '\n' ' ')"; return 1; }
  [[ "$(as_acct ada cat $STATE/status.json | jq -r '.enabled')" == true ]] || { echo "status.json not readable/enabled"; return 1; }
}
check "B1 after the symlink cases a normal run still converges" t_still_converges

# --------------------------- B1: provisioning without root's DAC bypass

t_nodac_control() {
  "${NODAC[@]}" cat $PRIV/gcs-key.json >/dev/null 2>&1 && { echo "control failed: root with the DAC bypass dropped can still read the user's key"; return 1; }
  "${NODAC[@]}" sh -c "echo x > $PRIV/probe" 2>/dev/null && { echo "control failed: root without the DAC bypass could write into $PRIV"; rm -f $PRIV/probe; return 1; }
  return 0
}
check "B1 control: with CAP_DAC_OVERRIDE/CAP_DAC_READ_SEARCH dropped, root cannot read or write inside $PRIV" t_nodac_control

t_nodac_flow() {
  local f=0 kid
  prov_nodac; [[ $RC -eq 0 ]] || { echo "converge without DAC bypass exited $RC: $(grep -iE 'denied|error' $OUT | head -3 | tr '\n' ' ')"; f=1; }
  # the key is rotated: written by the user, old gcloud credentials gone
  write_secrets "$KEY2"; prov_nodac
  [[ $RC -eq 0 ]] || { echo "key rotation without DAC bypass exited $RC: $(grep -iE 'denied|error' $OUT | head -3 | tr '\n' ' ')"; f=1; }
  kid="$(as_bu jq -r .private_key_id $PRIV/gcs-key.json 2>/dev/null)"
  [[ "$kid" == "$KID2" ]] || { echo "rotated key not installed (private_key_id is '$kid')"; f=1; }
  [[ "$(stat -c '%U:%a' $PRIV/gcs-key.json)" == "$BU:600" ]] || { echo "key file is $(stat -c '%U:%a' $PRIV/gcs-key.json)"; f=1; }
  ENABLED=0 write_profile; prov_nodac
  [[ $RC -eq 0 ]] || { echo "disable without DAC bypass exited $RC: $(grep -iE 'denied|error' $OUT | head -3 | tr '\n' ' ')"; f=1; }
  [[ "$(jq -r .enabled $STATE/status.json)" == false ]] || { echo "status.json not marked disabled"; f=1; }
  [[ ! -e $PRIV/gcs-key.json ]] || { echo "the key survived disabling"; f=1; }
  ENABLED=1 write_profile; write_secrets "$KEY1"; prov_nodac
  [[ $RC -eq 0 ]] || { echo "re-enable without DAC bypass exited $RC: $(grep -iE 'denied|error' $OUT | head -3 | tr '\n' ' ')"; f=1; }
  [[ "$(jq -r .enabled $STATE/status.json)" == true && -e $UNITS/ikenga-backup-daily.timer ]] || { echo "re-enable did not restore status/timers"; f=1; }
  return $f
}
check "B1 provisioning converges, rotates the key, disables and re-enables with root's DAC bypass removed (root needs no power inside the user's dirs)" t_nodac_flow
write_secrets "$KEY1"; ENABLED=1 write_profile; prov >/dev/null

# ----------------------------------------- (a) gcloud credentials removed

activate_real() {   # activate the current key with the REAL gcloud into the unit's CLOUDSDK_CONFIG for <schedule>
  local cfg; cfg="$(sed -n 's/^Environment=CLOUDSDK_CONFIG=//p' $SVC | sed "s/%i/$1/")"
  as_bu env CLOUDSDK_CONFIG="$cfg" CLOUDSDK_CORE_DISABLE_PROMPTS=1 CLOUDSDK_CORE_DISABLE_USAGE_REPORTING=true \
    CLOUDSDK_COMPONENT_MANAGER_DISABLE_UPDATE_CHECK=1 "$REAL_GCLOUD" auth activate-service-account --key-file=$PRIV/gcs-key.json -q >"$T/activate.out" 2>&1
}
creds_files() { find $PRIV -name credentials.db -o -name access_tokens.db -o -name legacy_credentials -o -name configurations 2>/dev/null; }
t_creds_removed_on_disable() {
  local f=0
  activate_real daily || { echo "fixture: the real gcloud could not activate the key against the local stub: $(head -c 300 $T/activate.out)"; return 1; }
  [[ -n "$(creds_files)" ]] || { echo "fixture: real gcloud left no credentials file under $PRIV, so this test would prove nothing"; return 1; }
  grep -rqF "$KID1" $PRIV/gcloud 2>/dev/null || echo "(note: the key id is not greppable in the gcloud dir; checking the files gcloud made)"
  ENABLED=0 write_profile; prov; ENABLED=1 write_profile
  [[ $RC -eq 0 ]] || { echo "disable exited $RC"; f=1; }
  [[ -z "$(creds_files)" ]] || { echo "credentials survived disabling: $(creds_files | tr '\n' ' ')"; f=1; }
  [[ ! -e $PRIV/gcloud ]] || { echo "$PRIV/gcloud survived disabling: $(ls -A $PRIV/gcloud | tr '\n' ' ')"; f=1; }
  grep -rqF "$KID1" $PRIV $STATE/status 2>/dev/null && { echo "the service-account key id is still on disk under $PRIV"; f=1; }
  [[ $f -eq 0 ]] && echo "real gcloud credentials created, then all removed by disable"
  return $f
}
check "(a) disable removes the gcloud credentials (real google-cloud-cli, generated key, local token stub)" t_creds_removed_on_disable
prov >/dev/null

t_creds_removed_on_rotation() {
  local f=0
  activate_real daily || { echo "fixture: real gcloud activation failed: $(head -c 300 $T/activate.out)"; return 1; }
  [[ -n "$(creds_files)" ]] || { echo "fixture: no credentials were created"; return 1; }
  write_secrets "$KEY2"; prov; write_secrets "$KEY1"
  [[ $RC -eq 0 ]] || { echo "rotation run exited $RC"; f=1; }
  [[ -z "$(creds_files)" ]] || { echo "the old key's gcloud credentials survived a key rotation: $(creds_files | tr '\n' ' ')"; f=1; }
  [[ "$(as_bu jq -r .private_key_id $PRIV/gcs-key.json)" == "$KID2" ]] || { echo "the new key is not installed"; f=1; }
  return $f
}
check "(a) a rotated key removes the old key's gcloud credentials" t_creds_removed_on_rotation
write_secrets "$KEY1"; prov >/dev/null

# ------------------------------------------------ (b) concurrent schedules

t_concurrent() {
  local s f=0 kinds n
  # (the sandboxed service can only write under its own dirs, so count the config
  # dirs the activations left a marker in, rather than a log outside them)
  rm -rf $PRIV/gcloud
  for s in daily 4hourly weekly; do ( systemctl start ikenga-backup@$s.service >/dev/null 2>&1; echo $? > "$T/rc.$s" ) & done
  wait
  kinds="$(jq -r '.databases[].last_error_kind' $STATE/status.json | sort -u | tr '\n' ' ')"
  [[ "$kinds" != *gcs-auth* ]] || { echo "a schedule failed to authenticate when started together with the others (kinds: $kinds)"; f=1; }
  n="$(find $PRIV/gcloud -name active-account 2>/dev/null | wc -l)"
  [[ "$n" -eq 3 ]] || { echo "the 3 schedules activated into $n distinct gcloud config dir(s), want 3: $(find $PRIV/gcloud -name active-account | tr '\n' ' ')"; f=1; }
  return $f
}
check "(b) daily, 4hourly and weekly started together all authenticate, each with its own gcloud config dir" t_concurrent

# ------------------------------------------ (c) missing "enabled" = skip

t_enabled_skip() {
  local f=0
  jq -e '.databases | has("h-unset")' $STATE/status.json >/dev/null && { echo "h-unset (no \"enabled\" field) is in status.json: it would be backed up"; f=1; }
  jq -e '.databases | has("h-daily")' $STATE/status.json >/dev/null || { echo "h-daily (enabled: true) is missing from status.json"; f=1; }
  prov --dry-run
  grep -q 'h-unset' "$OUT" && grep -qi 'not true' "$OUT" || { echo "the run does not name the database it skips (h-unset)"; f=1; }
  # a config where nothing says enabled: true backs up nothing, and says so
  cp $CONFIG_SRC $CONFIG_SRC.keep
  jq 'del(.databases[].enabled)' $CONFIG_SRC.keep > $CONFIG_SRC
  prov --dry-run
  [[ $RC -ne 0 ]] && grep -q 'no enabled database' "$OUT" || { echo "a config with no \"enabled\": true was accepted (rc $RC)"; f=1; }
  mv $CONFIG_SRC.keep $CONFIG_SRC
  return $f
}
check "(c) a database without \"enabled\": true is skipped (rex-vps semantics), by name; none enabled is refused" t_enabled_skip

# -------------------------------------------- (d) BACKUP_PG_MAJOR >= 17

t_pgmajor() {
  local f=0
  PGMAJ=16 write_profile; prov --dry-run
  [[ $RC -ne 0 ]] && grep -q 'BACKUP_PG_MAJOR' "$OUT" || { echo "BACKUP_PG_MAJOR=16 was accepted (rc $RC)"; f=1; }
  PGMAJ=09 write_profile; prov --dry-run
  [[ $RC -ne 0 ]] || { echo "BACKUP_PG_MAJOR=09 was accepted"; f=1; }
  PGMAJ=17 write_profile; prov --dry-run
  [[ $RC -eq 0 ]] || { echo "BACKUP_PG_MAJOR=17 was refused: $(tail -2 $OUT | tr '\n' ' ')"; f=1; }
  PGMAJ="" write_profile
  return $f
}
check "(d) BACKUP_PG_MAJOR below 17 is refused, 17 accepted" t_pgmajor

# --------------------------------------------------------------- (f) README

t_readme() {
  local r=/work/README.md f=0
  grep -q 'gcloud' <(sed -n '/^\*\*Disabling\*\*/p' $r) || { echo "README 'Disabling' paragraph does not mention the gcloud credentials"; f=1; }
  grep -q 'status/status.json' $r || { echo "README does not document the status/ directory"; f=1; }
  return $f
}
check "(f) README says what disable removes and documents the layout" t_readme

# ------------------------------------------- (g) the backup user cannot block or hang root

t_disable_not_blockable() {   # a planted symlink must not keep timers/units/connection strings alive
  local f=0
  ENABLED=1 write_profile; prov
  as_bu ln -s /etc/passwd $PRIV/gcloud/planted-link 2>/dev/null || as_bu sh -c "mkdir -p $PRIV/gcloud && ln -s /etc/passwd $PRIV/gcloud/planted-link"
  ENABLED=0 write_profile; prov
  ls $UNITS/ikenga-backup-*.timer >/dev/null 2>&1 && { echo "timers survived a disable blocked by a planted symlink"; f=1; }
  [[ -e $SVC ]] && { echo "service unit survived a blocked disable"; f=1; }
  [[ -e /etc/ikenga-backup/connections.env ]] && { echo "connections.env survived a blocked disable"; f=1; }
  [[ $RC -ne 0 ]] || { echo "disable with a planted symlink should still report the refusal (rc 0)"; f=1; }
  rm -f $PRIV/gcloud/planted-link; ENABLED=1 write_profile; prov
  [[ $RC -eq 0 ]] || { echo "re-enable after removing the link failed (rc $RC)"; f=1; }
  return $f
}
check "(g1) a planted symlink cannot block disabling: timers, unit and connection strings are still removed" t_disable_not_blockable

t_no_user_jq_or_hang() {   # ~/.jq is not loaded; FIFOs where root's tools read cannot hang provisioning
  local f=0 t0 t1
  as_bu sh -c "printf 'def from_entries: error(\"JQHOOK-RAN\");\n' > $PRIV/.jq"
  ENABLED=1 write_profile; prov
  grep -q 'JQHOOK-RAN' "$OUT" && { echo "the backup user's ~/.jq ran inside provisioning"; f=1; }
  as_bu rm -f $PRIV/.jq
  as_bu mkfifo $PRIV/.jq 2>/dev/null || true
  as_bu sh -c "rm -f $STATE/status/.status.lock; mkfifo $STATE/status/.status.lock" 2>/dev/null || true
  t0=$(date +%s); BACKUP_AS_USER_TIMEOUT=8 ENABLED=0 write_profile; BACKUP_AS_USER_TIMEOUT=8 prov; t1=$(date +%s)
  (( t1 - t0 < 120 )) || { echo "provisioning hung on a planted FIFO ($((t1 - t0)) s)"; f=1; }
  as_bu rm -f $PRIV/.jq $STATE/status/.status.lock 2>/dev/null || true
  ENABLED=1 write_profile; prov
  [[ $RC -eq 0 ]] || { echo "re-enable after removing the FIFOs failed (rc $RC)"; f=1; }
  return $f
}
check "(g2) the backup user's ~/.jq never runs, and a planted FIFO cannot hang provisioning" t_no_user_jq_or_hang

# ---------------------------------------------------------------- summary
echo
echo "==> [Hardening] $PASSN passed, $FAILN failed"
if [[ $FAILN -gt 0 ]]; then printf '    FAILED: %s\n' "${FAILED[@]}"; exit 1; fi
echo "==> [Hardening] ALL HARDENING CHECKS PASSED"
