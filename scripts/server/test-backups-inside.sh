#!/bin/bash
# provision.sh backups (D-B10): Postgres backups to GCS as system jobs, run as
# root inside an ubuntu:24.04 container (test-backups-container.sh).
#
# What is real: the apt repositories and signing-key checks (PGDG, Google
# Cloud SDK), postgresql-client-17 and the google-cloud-cli package, a local
# PostgreSQL 17 server with two fake databases and password auth, the backup
# user, every file and mode, the unit files (checked by systemd-analyze), and
# the scripts. What is faked: there is no PID 1 systemd, so a mock `systemctl`
# records enable/disable/restart and `start <unit>` runs the unit's own
# ExecStart as the unit's User/Group with the unit's Environment= (the
# sandbox directives are NOT enforced here); and GCS, which a fake `gcloud`
# on the backup user's PATH replaces with a local directory.
#
# Every secret value below is a FAKE-... canary. The suite asserts none of
# them reaches argv, the journal, status.json, or any provisioner output.
set -euo pipefail

if [[ -n "${KEEP_APT_ARCHIVES:-}" ]]; then   # the container script mounted a .deb cache
  rm -f /etc/apt/apt.conf.d/docker-clean
  echo 'Binary::apt::APT::Keep-Downloaded-Packages "true"; APT::Keep-Downloaded-Packages "true";' > /etc/apt/apt.conf.d/99keep
fi
T="$(mktemp -d)"
PROV=/root/prov
PROVISION="/work/provision.sh"
PROFILE="$PROV/profile.env"
SECRETS="$PROV/secrets"
CONFIG_SRC="$PROV/backup-config.json"
OUT="$T/out"
ALL="$T/all-output"
: > "$ALL"
BU=ikenga-backup
ETC=/etc/ikenga-backup
STATE=/var/lib/ikenga-backup
PRIV=$STATE/private
STATUS_DIR=$STATE/status
STATUS=$STATUS_DIR/status.json
UNITS=/etc/systemd/system
FAKEGCS=/srv/fake-gcs
JOURNAL=/var/log/mock-journal
PGPORT=55432

fail() { echo "FAIL: $*" >&2; [[ -f "$OUT" ]] && { echo "--- last output ---" >&2; tail -40 "$OUT" >&2; }; exit 1; }
pass() { echo "==> [Container] $* PASSED"; }
ok() { local d="$1"; shift; "$@" || fail "$d"; }

# ------------------------------------------------------------------ canaries

PW_ONE='FAKE-pw-one'
PW_TWO='FAKE-pw@two#3'
PW_TWO_ENC='FAKE-pw%40two%233'
URL_ONE="postgres://bk1:$PW_ONE@127.0.0.1:$PGPORT/app_one?sslmode=disable"
URL_TWO="postgresql://bk2:$PW_TWO_ENC@127.0.0.1:$PGPORT/app_two?sslmode=disable&connect_timeout=5"
mkkey() {   # client_email -> base64 (one line) of a fake service-account key (no jq: it is not installed yet)
  printf '%s' '{"type":"service_account","project_id":"fake-project","private_key_id":"FAKEKEYID123","private_key":"-----BEGIN PRIVATE KEY-----\nFAKEPRIVATEKEYBODYabc123\n-----END PRIVATE KEY-----\n","client_email":"'"$1"'","client_id":"1","token_uri":"https://oauth2.googleapis.com/token"}' | base64 -w0
}
KEY_B64="$(mkkey backup@fake-project.iam.gserviceaccount.com)"
# Regex (the bracketed last letter keeps a grep from matching its own argv)
# for everything that must never appear in argv, logs or status: the passwords,
# the key id and body, and the first characters of the key's base64.
CANARY_RE='FAKE-pw-on[e]|FAKE-pw@tw[o]|FAKE-pw%40tw[o]|FAKEPRIVATEKEYBOD[Y]|FAKEKEY[I]D|eyJ0eXBlIjoic2VydmljZV9hY2NvdW50I[i]'
# The same set as plain strings, for scanning files (grep -F -f).
printf '%s\n' "$PW_ONE" "$PW_TWO" "$PW_TWO_ENC" FAKEPRIVATEKEYBODYabc123 FAKEKEYID123 "${KEY_B64:0:40}" "$KEY_B64" "$URL_ONE" "$URL_TWO" > "$T/canaries"
no_canary_in() {   # description, files/dirs...
  local d="$1"; shift
  ! grep -raFl -f "$T/canaries" -- "$@" >"$T/hits" 2>/dev/null || fail "$d: a secret value was found in: $(cat "$T/hits" | tr '\n' ' ')"
}

# ------------------------------------------------------------------- host

PRINC=/var/lib/ikenga-test/principals
mkacct() {   # name uid : mimics what `ikenga-server accounts create` leaves behind
  groupadd -g "$2" "ik-$1"
  useradd -u "$2" -g "$2" -M -d "$PRINC/$2/home" -s /bin/sh "ik-$1"
  install -d -m 0700 -o "$2" -g "$2" "$PRINC/$2/home"
}
mkdir -p "$PRINC"
mkacct ada 20001; mkacct rex 20003
useradd -m ops
# Run as an Ikenga account the way the daemon runs its sessions: uid + private
# gid, NO supplementary groups.
as() {
  local n="$1" u; shift
  u="$(id -u "ik-$n")"
  ( cd / && setpriv --reuid="$u" --regid="$u" --clear-groups env -i HOME="$PRINC/$u/home" PATH=/usr/local/bin:/usr/bin:/bin "$@" )
}
as_bu() { ( cd / && setpriv --reuid="$(id -u $BU)" --regid="$(id -g $BU)" --clear-groups env -i HOME=$PRIV PATH=/usr/local/bin:/usr/bin:/bin "$@" ); }

# A mock systemctl: records calls; `start <unit>` runs the unit's ExecStart as
# its User/Group with its Environment= lines (nothing else of the sandbox).
mkdir -p /var/lib/mock-systemctl/enabled /var/lib/mock-systemctl/active "$JOURNAL" "$UNITS"
cat > /usr/local/bin/systemctl <<'MOCK'
#!/bin/bash
D=/var/lib/mock-systemctl
echo "$*" >> $D/calls.log
cmd="${1:-}"; shift || true
units=(); for a in "$@"; do [[ "$a" == -* ]] || units+=("$a"); done
now=0; for a in "$@"; do [[ "$a" == --now ]] && now=1; done
case "$cmd" in
  daemon-reload) ;;
  enable) for u in "${units[@]}"; do touch "$D/enabled/$u"; [[ $now -eq 1 ]] && touch "$D/active/$u"; done ;;
  disable) for u in "${units[@]}"; do rm -f "$D/enabled/$u"; [[ $now -eq 1 ]] && rm -f "$D/active/$u"; done ;;
  restart) for u in "${units[@]}"; do touch "$D/active/$u"; echo "$u" >> $D/restarts.log; done ;;
  is-enabled) [[ -e "$D/enabled/${units[0]}" ]]; exit $? ;;
  is-active) [[ -e "$D/active/${units[0]}" ]]; exit $? ;;
  start)
    u="${units[0]}"; base="${u%.service}"; inst="${base#*@}"; tmpl="/etc/systemd/system/${base%%@*}@.service"
    [[ -f "$tmpl" ]] || { echo "Unit $u not found." >&2; exit 5; }
    user="$(sed -n 's/^User=//p' "$tmpl")"; group="$(sed -n 's/^Group=//p' "$tmpl")"
    mapfile -t envs < <(sed -n 's/^Environment=//p' "$tmpl" | sed "s/%i/$inst/g")
    read -ra exe <<<"$(sed -n 's/^ExecStart=//p' "$tmpl" | sed "s/%i/$inst/g")"
    echo "--- start $u $(date -u +%T)" >> "$D/../../log/mock-journal/$u.log"
    ( cd / && umask 0077 && exec setpriv --reuid="$(id -u "$user")" --regid="$(id -g "$group")" --clear-groups \
        env -i "${envs[@]}" "${exe[@]}" ) >> "$D/../../log/mock-journal/$u.log" 2>&1
    exit $? ;;
  *) ;;
esac
exit 0
MOCK
chmod +x /usr/local/bin/systemctl
sc_calls() { cat /var/lib/mock-systemctl/calls.log 2>/dev/null || true; }

# ---------------------------------------------------------------- fixtures

install -d -m 0700 "$PROV"
echo '{"databases":[]}' > "$T/placeholder"

write_config() {   # bucket for fake-weekly
  cat > "$CONFIG_SRC" <<JSON
{
  "description": "test fixture",
  "databases": [
    { "name": "fake-one",    "connection_secret": "FAKE_ONE_URL", "schedule": "daily",   "gcs_bucket": "bucket-a", "enabled": true },
    { "name": "fake-two",    "connection_secret": "FAKE_TWO_URL", "schedule": "daily",   "gcs_bucket": "bucket-a", "enabled": true },
    { "name": "fake-fast",   "connection_secret": "FAKE_ONE_URL", "schedule": "4hourly", "gcs_bucket": "bucket-b", "enabled": true },
    { "name": "fake-weekly", "connection_secret": "FAKE_TWO_URL", "schedule": "weekly",  "gcs_bucket": "${1:-bucket-a}", "enabled": true },
    { "name": "fake-off",    "connection_secret": "FAKE_OFF_URL", "schedule": "daily",   "gcs_bucket": "bucket-a", "enabled": false },
    { "name": "fake-unset",  "connection_secret": "FAKE_ONE_URL", "schedule": "daily",   "gcs_bucket": "bucket-a" }
  ]
}
JSON
  chmod 0600 "$CONFIG_SRC"
}
write_config

write_secrets() {   # the standard set plus any extra lines passed
  printf '%s\n' "# fake values only" ""  \
    "[everyone] SHARED_NOTE=FAKE-everyone-note" \
    "[rex] REX_ONLY=FAKE-rex-only-222" \
    "[ada] ADA_ONLY=FAKE-ada-only-111" \
    "[backup] FAKE_ONE_URL=$URL_ONE" \
    "[backup] FAKE_TWO_URL=${TWO_URL:-$URL_TWO}" \
    "[backup] UNUSED_BACKUP=FAKE-unused-backup" \
    "[root] GCS_KEY_B64=${KEY:-$KEY_B64}" \
    "$@" > "$SECRETS"
  chmod 0600 "$SECRETS"
}
ENABLED=1; SCHEDS=(); BUSER=$BU
write_profile() {
  {
    echo 'ADMIN_USER=ops'
    echo 'ACCOUNTS=(ada rex)'
    echo 'AGENT_ACCOUNTS=(rex)'
    echo "SECRETS_FILE=$SECRETS"
    echo "BACKUPS_ENABLED=$ENABLED"
    echo "BACKUP_USER=$BUSER"
    echo "BACKUP_CONFIG=$CONFIG_SRC"
    echo 'BACKUP_GCS_KEY_SECRET=GCS_KEY_B64'
    [[ ${#SCHEDS[@]} -eq 0 ]] || printf 'BACKUP_SCHEDULES=(%s)\n' "$(printf '"%s" ' "${SCHEDS[@]}")"
  } > "$PROFILE"
  chmod 0600 "$PROFILE"
}
write_secrets; write_profile

RC=0
prov() { RC=0; "$PROVISION" backups --profile "$PROFILE" "$@" >"$OUT" 2>&1 || RC=$?; cat "$OUT" >> "$ALL"; }
provfull() { RC=0; "$PROVISION" "$@" --profile "$PROFILE" >"$OUT" 2>&1 || RC=$?; cat "$OUT" >> "$ALL"; }
expect_refusal() {   # description, grep pattern; runs a dry run
  prov --dry-run
  [[ $RC -ne 0 ]] || fail "$1: accepted"
  grep -q -- "$2" "$OUT" || fail "$1: refused, but not with '$2'"
  ! grep -qE "FAKE-|eyJ0eXBl" "$OUT" || fail "$1: the refusal printed a secret"
}

sample_argv() { ( while :; do grep -alE "$CANARY_RE" /proc/[0-9]*/cmdline 2>/dev/null || true; done > "$1" ) & SAMPLER=$!; }
stop_sampler() { kill "$SAMPLER" 2>/dev/null || true; wait "$SAMPLER" 2>/dev/null || true; }
sample_pgdump() { ( while :; do grep -al 'pg_dum[p]' /proc/[0-9]*/cmdline 2>/dev/null || true; done > "$1" ) & SAMPLER2=$!; }

# --------------------------------------------- 0. the sampler can see argv

sample_argv "$T/control-hits"
bash -c 'sleep 1; true' FAKE-pw-one &
CONTROL=$!
sleep 0.6; stop_sampler; wait "$CONTROL" 2>/dev/null || true
[[ -s "$T/control-hits" ]] || fail "argv sampler control: a planted password was not seen"
pass "fixture: the argv sampler can see a planted secret"

# ---------------------------------- 1. dry run on a bare host changes nothing

prov --dry-run
[[ $RC -eq 0 ]] || fail "dry run exited $RC"
grep -q 'would: apt packages installed:' "$OUT" || fail "dry run did not plan the base packages"
grep -q 'jq is not installed here' "$OUT" || fail "dry run on a host without jq did not say the config is unvalidated"
! getent passwd $BU >/dev/null || fail "dry run created the backup user"
[[ ! -e $ETC && ! -e $STATE && ! -e /usr/local/lib/ikenga-backup ]] || fail "dry run created directories"
! command -v jq >/dev/null || fail "dry run installed jq"
grep -q 'FAKE-' "$OUT" && fail "dry run printed a secret value"
pass "dry run on a bare host plans packages, creates nothing, prints no secret"

# -------------------- 2. a tampered PGDG key is refused (fingerprint pinning)

cat > /usr/local/bin/curl <<'WRAP'
#!/bin/bash
# Test wrapper: serve a different key for the PGDG key URL when EVIL_PGDG is set.
if [[ -n "${EVIL_PGDG:-}" ]]; then
  out=""; hit=0; prev=""
  for a in "$@"; do
    [[ "$prev" == -o ]] && out="$a"
    [[ "$a" == *ACCC4CF8.asc ]] && hit=1
    prev="$a"
  done
  if [[ $hit -eq 1 && -n "$out" ]]; then cp "$EVIL_PGDG" "$out"; exit 0; fi
fi
exec /usr/bin/curl "$@"
WRAP
chmod +x /usr/local/bin/curl
EV="$(mktemp -d)"; chmod 700 "$EV"
GNUPGHOME="$EV" gpg --batch --pinentry-mode loopback --passphrase '' --quick-gen-key 'Evil Repo <evil@example.invalid>' rsa2048 sign never >/dev/null 2>&1
GNUPGHOME="$EV" gpg --batch --armor --export 'evil@example.invalid' > "$T/evil.asc"
[[ -s "$T/evil.asc" ]] || fail "fixture: could not generate the stand-in key"
RC=0; EVIL_PGDG="$T/evil.asc" "$PROVISION" backups --profile "$PROFILE" >"$OUT" 2>&1 || RC=$?; cat "$OUT" >> "$ALL"
[[ $RC -ne 0 ]] || fail "a PGDG key with the wrong fingerprint was accepted"
grep -q 'not one of the pinned' "$OUT" || fail "the key refusal did not say it was the fingerprint pin"
[[ ! -e /etc/apt/sources.list.d/ikenga-pgdg.list && ! -e /usr/share/keyrings/ikenga-pgdg.gpg ]] || fail "the refused key/repo was left installed"
! getent passwd $BU >/dev/null || fail "the backup user was created before the repository check passed"
command -v jq >/dev/null || fail "the base packages (jq) were not installed"
pass "a PGDG key with the wrong fingerprint is refused; no repo, keyring or user left behind"

# --------------------------------------------- 3. refusals (dry runs, jq now in)

write_secrets "[backup,ada] COMBO=FAKE-combo"
expect_refusal "scope backup,ada" "cannot be combined"
write_secrets "[everyone] FAKE_ONE_URL=FAKE-leaked-to-everyone"
expect_refusal "connection secret also delivered to accounts" "must not reach any Ikenga account"
write_secrets; sed -i 's/^\[backup\] FAKE_TWO_URL=/[root] FAKE_TWO_URL=/' "$SECRETS"
expect_refusal "connection secret in scope root" "not \[backup\]"
write_secrets; sed -i '/^\[root\] GCS_KEY_B64/d' "$SECRETS"
expect_refusal "no key secret" "must appear exactly once"
write_secrets; sed -i 's/^\[root\] GCS_KEY_B64/[everyone] GCS_KEY_B64/' "$SECRETS"
expect_refusal "key scoped everyone" "so accounts would get the GCS service-account key"
KEY='not base64 !!' write_secrets
expect_refusal "key not base64" "not valid base64"
KEY="$(printf '{"type":"user"}' | base64 -w0)" write_secrets
expect_refusal "key not a service account" "does not decode to a service-account key"
write_secrets
for bad in ik-ada ikenga root ops; do
  BUSER=$bad write_profile
  expect_refusal "BACKUP_USER=$bad" "BACKUP_USER"
done
BUSER=$BU
useradd -u 20009 -M bkbad
BUSER=bkbad write_profile
expect_refusal "BACKUP_USER inside the account uid range" "inside the Ikenga account range"
userdel bkbad; BUSER=$BU
SCHEDS=("daily=every banana"); write_profile
expect_refusal "unparsable OnCalendar" "cannot parse the calendar"
SCHEDS=("daily=*-*-* 02:00:00 UTC; rm -rf /"); write_profile
expect_refusal "OnCalendar with shell characters" "characters a systemd OnCalendar does not need"
SCHEDS=(); write_profile
cp "$CONFIG_SRC" "$CONFIG_SRC.good"
sed -i 's/"bucket-b"/"Bad Bucket!"/' "$CONFIG_SRC"; expect_refusal "bad bucket name" "gcs_bucket is not a bucket name"
cp "$CONFIG_SRC.good" "$CONFIG_SRC"
sed -i 's/"schedule": "weekly"/"schedule": "fortnightly"/' "$CONFIG_SRC"; expect_refusal "unknown schedule" "has no calendar"
cp "$CONFIG_SRC.good" "$CONFIG_SRC"
sed -i 's/"name": "fake-two"/"name": "fake-one"/' "$CONFIG_SRC"; expect_refusal "duplicate database" "duplicate database name"
cp "$CONFIG_SRC.good" "$CONFIG_SRC"; rm "$CONFIG_SRC.good"
write_secrets
! getent passwd $BU >/dev/null && [[ ! -e $ETC ]] || fail "a refused dry run changed the host"
pass "refusals: backup scope combined with others / delivered to accounts / wrong scope; bad or missing key; bad BACKUP_USER (root, admin, ik-*, T0 'ikenga', uid in the account range); bad calendars; bad config"

# --------------------------------------------------------- 4. the first run

# Real install: PGDG repo + postgresql-client-17, Google Cloud SDK repo + the
# google-cloud-cli package (no gcloud is on this host yet), user, directories,
# secrets, units. Sampled for secrets in argv throughout.
df -h / | tail -1 | sed 's/^/    disk: /'
sample_argv "$T/argv-hits-provision"
prov
stop_sampler
[[ $RC -eq 0 ]] || fail "first run exited $RC"
[[ ! -s "$T/argv-hits-provision" ]] || fail "a secret appeared in a process argv during provisioning: $(tr '\0' ' ' < "$T/argv-hits-provision" | head -c 300)"
command -v pg_dump >/dev/null || [[ -x /usr/lib/postgresql/17/bin/pg_dump ]] || fail "pg_dump 17 not installed"
[[ "$(/usr/lib/postgresql/17/bin/pg_dump --version)" == *" 17."* ]] || fail "pg_dump is not version 17: $(/usr/lib/postgresql/17/bin/pg_dump --version)"
command -v gcloud >/dev/null || fail "gcloud not installed"
gcloud --version >/dev/null 2>&1 || fail "gcloud does not run"
pass "first run: PGDG repo + pg_dump 17 and the Google Cloud SDK repo + gcloud installed; no secret in any argv"

# The repositories: signed-by their own keyring, pinned fingerprints.
grep -qxF 'deb [signed-by=/usr/share/keyrings/ikenga-pgdg.gpg] https://apt.postgresql.org/pub/repos/apt noble-pgdg main' /etc/apt/sources.list.d/ikenga-pgdg.list || fail "PGDG source line"
grep -qxF 'deb [signed-by=/usr/share/keyrings/ikenga-gcloud.gpg] https://packages.cloud.google.com/apt cloud-sdk main' /etc/apt/sources.list.d/ikenga-gcloud.list || fail "gcloud source line"
fps="$(GNUPGHOME="$(mktemp -d)" gpg --batch --show-keys --with-colons --fingerprint /usr/share/keyrings/ikenga-pgdg.gpg 2>/dev/null | awk -F: '$1=="pub"{p=1} $1=="fpr"&&p{print $10; p=0}')"
[[ "$fps" == B97B0AFCAA1A47F044F244A07FCC7D46ACCC4CF8 ]] || fail "PGDG keyring fingerprint is '$fps'"
fps="$(GNUPGHOME="$(mktemp -d)" gpg --batch --show-keys --with-colons --fingerprint /usr/share/keyrings/ikenga-gcloud.gpg 2>/dev/null | awk -F: '$1=="pub"{p=1} $1=="fpr"&&p{print $10; p=0}')"
[[ "$fps" == 35BAA0B33E9EB396F59CA838C0BA5CE6DC6315A3 ]] || fail "gcloud keyring fingerprint is '$fps'"
pol="$(apt-cache policy postgresql-client-17)"; grep -q 'apt.postgresql.org' <<<"$pol" || fail "postgresql-client-17 does not come from PGDG"
pol="$(apt-cache policy google-cloud-cli)"; grep -q 'packages.cloud.google.com' <<<"$pol" || fail "google-cloud-cli does not come from the Google repo"
pass "both apt repositories: signed-by a dedicated keyring whose fingerprint is the pinned one; packages resolve from them"

# --------------------------------------------- 5. user, directories, files

[[ "$(getent passwd $BU | cut -d: -f3)" -lt 1000 ]] || fail "backup user is not a system uid"
[[ "$(getent passwd $BU | cut -d: -f6,7)" == "$PRIV:/usr/sbin/nologin" ]] || fail "home/shell: $(getent passwd $BU | cut -d: -f6,7)"
[[ "$(id -nG $BU)" == "$BU" ]] || fail "backup user has supplementary groups: $(id -nG $BU)"
[[ ! -e /home/$BU ]] || fail "backup user has a home under /home"
[[ "$(stat -c '%a %U %G' $ETC)" == "750 root $BU" ]] || fail "$ETC: $(stat -c '%a %U %G' $ETC)"
[[ "$(stat -c '%a %U %G' $ETC/connections.env)" == "640 root $BU" ]] || fail "connections.env: $(stat -c '%a %U %G' $ETC/connections.env)"
[[ "$(stat -c '%a %U %G' $ETC/backup-config.json)" == "640 root $BU" ]] || fail "backup-config.json: $(stat -c '%a %U %G' $ETC/backup-config.json)"
[[ "$(stat -c '%a %U %G' $STATE)" == "755 root root" ]] || fail "$STATE is not root-only: $(stat -c '%a %U %G' $STATE)"
[[ "$(stat -c '%a %U %G' $STATUS_DIR)" == "755 $BU $BU" ]] || fail "$STATUS_DIR: $(stat -c '%a %U %G' $STATUS_DIR)"
[[ "$(stat -c '%F %N' $STATE/status.json)" == "symbolic link '$STATE/status.json' -> 'status/status.json'" ]] || fail "$STATE/status.json is not the root-made link: $(stat -c '%F %N' $STATE/status.json)"
[[ "$(stat -c '%a %U %G' $PRIV)" == "700 $BU $BU" ]] || fail "$PRIV: $(stat -c '%a %U %G' $PRIV)"
[[ "$(stat -c '%a %U %G' $PRIV/gcs-key.json)" == "600 $BU $BU" ]] || fail "gcs-key.json: $(stat -c '%a %U %G' $PRIV/gcs-key.json)"
[[ "$(stat -c '%a %U %G' $STATUS)" == "644 $BU $BU" ]] || fail "status.json: $(stat -c '%a %U %G' $STATUS)"
for f in run-backup.sh verify-backup.sh; do
  cmp -s /work/backup/$f /usr/local/lib/ikenga-backup/$f || fail "$f differs from the repo copy"
  [[ "$(stat -c '%a %U %G' /usr/local/lib/ikenga-backup/$f)" == "755 root root" ]] || fail "$f is not root-owned 0755"
done
# Only the referenced connection strings are in the env file; not the unused
# backup secret, not an account's, and the key is not in it.
[[ "$(sed -n 's/^\([A-Za-z_][A-Za-z0-9_]*\)=.*/\1/p' $ETC/connections.env | sort | tr '\n' ' ')" == "FAKE_ONE_URL FAKE_TWO_URL " ]] \
  || fail "connections.env holds: $(sed -n 's/^\([A-Za-z_][A-Za-z0-9_]*\)=.*/\1/p' $ETC/connections.env | tr '\n' ' ')"
[[ "$(jq -r .client_email $PRIV/gcs-key.json)" == backup@fake-project.iam.gserviceaccount.com ]] || fail "key file is not the decoded key"
grep -q 'unused backup-scoped secrets\|backup-scoped secrets no database refers to (not written): UNUSED_BACKUP' "$OUT" || fail "the unused backup secret was not reported by name"
no_canary_in "provisioner output" "$ALL"
pass "backup user is a plain system user (no login, no groups, home = 0700 private dir); files and modes as designed; env file holds only the referenced connection strings"

# ------------------------------------------------------ 6. the unit files

SVC=$UNITS/ikenga-backup@.service
for s in daily 4hourly weekly; do [[ -f $UNITS/ikenga-backup-$s.timer ]] || fail "no timer for $s"; done
[[ ! -e $UNITS/ikenga-backup-monthly.timer ]] || fail "a timer for an unused schedule exists"
grep -qxF 'OnCalendar=*-*-* 02:00:00 UTC' $UNITS/ikenga-backup-daily.timer || fail "daily calendar"
grep -qxF 'OnCalendar=*-*-* 00/4:00:00 UTC' $UNITS/ikenga-backup-4hourly.timer || fail "4hourly calendar"
grep -qxF 'OnCalendar=Sun *-*-* 03:00:00 UTC' $UNITS/ikenga-backup-weekly.timer || fail "weekly calendar"
for s in daily 4hourly weekly; do
  grep -qxF 'Persistent=true' $UNITS/ikenga-backup-$s.timer && grep -qxF 'RandomizedDelaySec=120' $UNITS/ikenga-backup-$s.timer \
    && grep -qxF "Unit=ikenga-backup@$s.service" $UNITS/ikenga-backup-$s.timer || fail "timer $s: Persistent/RandomizedDelaySec/Unit"
  [[ -e /var/lib/mock-systemctl/enabled/ikenga-backup-$s.timer && -e /var/lib/mock-systemctl/active/ikenga-backup-$s.timer ]] || fail "timer $s not enabled+started"
done
for want in "User=$BU" "Group=$BU" 'NoNewPrivileges=true' 'PrivateTmp=true' 'ProtectSystem=strict' "ReadWritePaths=$STATUS_DIR $PRIV" 'ProtectHome=true' 'CapabilityBoundingSet=' 'RestrictSUIDSGID=true' 'SystemCallFilter=@system-service' \
            'ExecStart=/usr/local/lib/ikenga-backup/run-backup.sh --schedule %i' "Environment=CLOUDSDK_CONFIG=$PRIV/gcloud/%i" 'Type=oneshot'; do
  grep -qxF -- "$want" $SVC || fail "service unit lacks: $want"
done
! grep -qiE 'FAKE|PASSWORD|EnvironmentFile|postgres(ql)?://|private_key|GOOGLE_APPLICATION' $SVC $UNITS/ikenga-backup-*.timer || fail "a unit file mentions a secret, a password or a connection string"
# systemd's own checks. `verify` parses every directive; `security` scores the sandbox.
systemd-analyze verify $SVC $UNITS/ikenga-backup-daily.timer >"$T/verify" 2>&1 || true
! grep -qiE 'unknown (key|section)|failed to parse|invalid|not valid|bad' "$T/verify" || { cat "$T/verify" >&2; fail "systemd-analyze verify found a problem"; }
sec="$(systemd-analyze security --offline=true --no-pager ikenga-backup@daily.service 2>/dev/null | tail -1 || true)"
echo "    systemd-analyze security: ${sec:-unavailable}"
pass "unit files: per-schedule timers in UTC with Persistent + RandomizedDelaySec, hardened oneshot service as the backup user, no secret anywhere; systemd-analyze verify clean"

# ---------------------------- 7. real Postgres 17 server + fake databases

apt-get install -y -qq --no-install-recommends postgresql-17 >/dev/null 2>&1 || fail "could not install postgresql-17 from PGDG"
pg_conftool 17 main set port $PGPORT >/dev/null
pg_ctlcluster 17 main start || fail "postgres 17 did not start"
for _ in $(seq 1 30); do pg_isready -q -h 127.0.0.1 -p $PGPORT && break; sleep 0.5; done
psqlsu() { su postgres -c "psql -p $PGPORT -v ON_ERROR_STOP=1 -qAt $*"; }
psqlsu "-c \"CREATE ROLE bk1 LOGIN PASSWORD '$PW_ONE'\""
psqlsu "-c \"CREATE ROLE bk2 LOGIN PASSWORD '$PW_TWO'\""
psqlsu "-c \"CREATE DATABASE app_one OWNER bk1\""
psqlsu "-c \"CREATE DATABASE app_two OWNER bk2\""
su postgres -c "psql -p $PGPORT -d app_one -qAt -c \"CREATE TABLE t(id int primary key, v text); INSERT INTO t SELECT g, md5(g::text) FROM generate_series(1,600000) g; ALTER TABLE t OWNER TO bk1\""
su postgres -c "psql -p $PGPORT -d app_two -qAt -c \"CREATE TABLE notes(id serial primary key, body text); INSERT INTO notes(body) SELECT 'row '||g FROM generate_series(1,100) g; ALTER TABLE notes OWNER TO bk2\""
PGPASSWORD="$PW_ONE" psql -h 127.0.0.1 -p $PGPORT -U bk1 -d app_one -qAt -c 'select count(*) from t' | grep -qxF 600000 || fail "fixture: app_one"
PGPASSWORD="$PW_TWO" psql -h 127.0.0.1 -p $PGPORT -U bk2 -d app_two -qAt -c 'select count(*) from notes' | grep -qxF 100 || fail "fixture: app_two"
! PGPASSWORD=wrong psql -h 127.0.0.1 -p $PGPORT -U bk1 -d app_one -qAt -c 'select 1' >/dev/null 2>&1 || fail "fixture: the server does not enforce passwords"
pass "fixture: PostgreSQL 17 on 127.0.0.1:$PGPORT, two databases, password auth enforced"

# The fake gcloud, on the backup user's PATH (/usr/local/bin precedes /usr/bin
# in the unit's PATH, so it shadows the real one that provisioning installed).
install -d -m 1777 $FAKEGCS
cat > /usr/local/bin/gcloud <<'SHIM'
#!/bin/bash
# Fake gcloud: no network. Records its argv and CLOUDSDK_CONFIG; `auth` needs a
# readable service-account key (a client_email containing "revoked" is
# rejected); `storage cp` needs a prior auth in THIS CLOUDSDK_CONFIG and copies
# the file under /srv/fake-gcs/buckets/<bucket>/<path>. Bucket deny-bucket is refused.
LOG=/srv/fake-gcs/calls.log
echo "gcloud $* | CLOUDSDK_CONFIG=${CLOUDSDK_CONFIG:-unset} uid=$(id -u)" >> "$LOG"
[[ -n "${CLOUDSDK_CONFIG:-}" ]] || { echo "ERROR: no config dir" >&2; exit 1; }
if [[ "${1:-}" == auth && "${2:-}" == activate-service-account ]]; then
  key="${3#--key-file=}"
  [[ -r "$key" ]] || { echo "ERROR: cannot read the key file" >&2; exit 1; }
  jq -e '.type == "service_account"' "$key" >/dev/null 2>&1 || { echo "ERROR: not a service account key" >&2; exit 1; }
  if jq -r .client_email "$key" | grep -q revoked; then echo "ERROR: (gcloud.auth.activate-service-account) invalid_grant" >&2; exit 1; fi
  mkdir -p "$CLOUDSDK_CONFIG" && : > "$CLOUDSDK_CONFIG/active-account"
  echo "Activated service account credentials for: [$(jq -r .client_email "$key")]" >&2
  exit 0
fi
if [[ "${1:-}" == storage && "${2:-}" == cp ]]; then
  shift 2; args=(); for a in "$@"; do [[ "$a" == -q || "$a" == -- ]] || args+=("$a"); done
  src="${args[0]}"; dst="${args[1]}"
  [[ -e "$CLOUDSDK_CONFIG/active-account" ]] || { echo "ERROR: You do not currently have an active account selected." >&2; exit 1; }
  [[ "$dst" == gs://* ]] || { echo "ERROR: bad destination" >&2; exit 1; }
  rest="${dst#gs://}"
  [[ "${rest%%/*}" == deny-bucket ]] && { echo "ERROR: 403 AccessDeniedException" >&2; exit 1; }
  sleep 0.4
  mkdir -p "/srv/fake-gcs/buckets/$(dirname "$rest")" && cp -- "$src" "/srv/fake-gcs/buckets/$rest"
  exit $?
fi
echo "fake gcloud: unsupported: $*" >&2; exit 2
SHIM
chmod +x /usr/local/bin/gcloud

# ------------------------------------------------ 8. a scheduled run, daily

units_start() { RC=0; systemctl start "$1" >"$T/start.out" 2>&1 || RC=$?; }
status_get() { jq -r "$1" $STATUS; }
sample_argv "$T/argv-hits-run"; sample_pgdump "$T/pgdump-seen"
units_start ikenga-backup@daily.service
stop_sampler; kill "$SAMPLER2" 2>/dev/null || true; wait "$SAMPLER2" 2>/dev/null || true
[[ $RC -eq 0 ]] || { cat "$JOURNAL/ikenga-backup@daily.service.log" $PRIV/errors/*.log >&2; fail "the daily run exited $RC"; }
[[ -s "$T/pgdump-seen" ]] || fail "the argv sampler never saw pg_dump run, so 'no secret in argv' would prove nothing"
[[ ! -s "$T/argv-hits-run" ]] || fail "a secret appeared in a process argv during the run: $(tr '\0' ' ' < "$T/argv-hits-run" | head -c 300)"
pass "daily run: exit 0; the sampler saw pg_dump and gcloud processes and no secret in any argv"

for d in fake-one fake-two; do
  [[ "$(status_get ".databases[\"$d\"].last_attempt_ok")" == true ]] || fail "status: $d not ok"
  [[ "$(status_get ".databases[\"$d\"].last_error_kind")" == null ]] || fail "status: $d has an error kind"
  [[ "$(status_get ".databases[\"$d\"].last_success")" =~ ^20[0-9-]+T[0-9:]+Z$ ]] || fail "status: $d last_success"
  [[ "$(status_get ".databases[\"$d\"].last_attempt")" =~ ^20[0-9-]+T[0-9:]+Z$ ]] || fail "status: $d last_attempt"
  obj="$(status_get ".databases[\"$d\"].object")"
  [[ "$obj" =~ ^gs://bucket-a/$d/[0-9]{4}/[0-9]{2}/$d-[0-9]{8}-[0-9]{6}\.sql\.gz$ ]] || fail "status: $d object path is '$obj'"
  f="$FAKEGCS/buckets/${obj#gs://}"
  [[ -s "$f" ]] || fail "$d: nothing uploaded at $f"
  gzip -t "$f" || fail "$d: uploaded dump is not valid gzip"
  gunzip -c "$f" | tail -n 10 | grep -q 'PostgreSQL database dump complete' || fail "$d: dump has no completion marker"
  [[ "$(status_get ".databases[\"$d\"].bytes")" == "$(stat -c %s "$f")" ]] || fail "status: $d bytes != uploaded size"
done
[[ "$(status_get '.databases["fake-fast"].last_attempt')" == null && "$(status_get '.databases["fake-weekly"].last_attempt')" == null ]] || fail "a daily run touched other schedules' databases"
[[ "$(status_get '.schedules.daily.last_run_ok')" == true && "$(status_get '.schedules.daily.succeeded')" == 2 ]] || fail "status: schedule summary"
[[ "$(status_get '.databases | keys | join(",")')" == "fake-fast,fake-one,fake-two,fake-weekly" ]] || fail "status: databases are $(status_get '.databases | keys | join(",")') (the disabled one must not appear)"
# The dump is real: restore it into a fresh database and count.
f="$FAKEGCS/buckets/$(status_get '.databases["fake-one"].object' | sed 's#^gs://##')"
su postgres -c "psql -p $PGPORT -qAt -c 'CREATE DATABASE restore_one'" >/dev/null
# The restore stops at the first error (ON_ERROR_STOP=1): a dump that restores "mostly" is not a good dump.
restore_dump() {   # file db: psql exit status, nothing on stderr
  gunzip -c "$1" | su postgres -c "psql -p $PGPORT -d $2 -qAt -v ON_ERROR_STOP=1" >/dev/null 2>"$T/restore.err" && [[ ! -s "$T/restore.err" ]]
}
restore_dump "$f" restore_one || fail "the uploaded dump does not restore cleanly: $(head -3 "$T/restore.err")"
[[ "$(su postgres -c "psql -p $PGPORT -d restore_one -qAt -c 'select count(*) from t'")" == 600000 ]] || fail "the uploaded dump does not restore to 600000 rows"
# Control: the same check must catch a dump with one bad statement in it. (With ON_ERROR_STOP=0 psql
# exits 0 on such a dump, which is why the old check could not fail on a restore error.)
su postgres -c "psql -p $PGPORT -qAt -c 'CREATE DATABASE restore_bad'" >/dev/null
{ gunzip -c "$f" | sed -n 1,40p; echo 'SELECT * FROM table_that_does_not_exist;'; gunzip -c "$f" | tail -n +41; } | gzip -c > "$T/broken.sql.gz"
! restore_dump "$T/broken.sql.gz" restore_bad || fail "the restore check passed a dump that contains a failing statement"
gunzip -c "$T/broken.sql.gz" | su postgres -c "psql -p $PGPORT -d restore_bad -qAt -v ON_ERROR_STOP=0" >/dev/null 2>&1 \
  || fail "fixture: the control should show psql exiting 0 under ON_ERROR_STOP=0"
pass "status.json: per db last attempt/success/error kind/object path/bytes; uploads exist, valid gzip, complete; the dump restores to the original 600000 rows"

# gcloud saw the isolated config dir and the private key path, and no secret.
grep -qF "auth activate-service-account --key-file=$PRIV/gcs-key.json -q | CLOUDSDK_CONFIG=$PRIV/gcloud/daily uid=$(id -u $BU)" $FAKEGCS/calls.log || fail "gcloud auth call: $(head -3 $FAKEGCS/calls.log)"
grep -q "^gcloud storage cp -q -- $PRIV/work/run-[A-Za-z0-9]*/fake-one-.*\.sql\.gz gs://bucket-a/fake-one/" $FAKEGCS/calls.log || fail "gcloud upload call"
[[ -f $PRIV/gcloud/daily/active-account ]] || fail "gcloud config was not written under the backup user's CLOUDSDK_CONFIG"
no_canary_in "gcloud argv log" $FAKEGCS/calls.log
no_canary_in "journal" $JOURNAL
no_canary_in "status.json and the world-readable state dir" $STATE/status.json
[[ -z "$(find $STATE -path $PRIV -prune -o -type f -print | xargs -r grep -aFl -f "$T/canaries" 2>/dev/null)" ]] || fail "a secret is in the world-readable part of $STATE"
grep -q 'db=fake-one status=ok' $JOURNAL/ikenga-backup@daily.service.log || fail "journal lacks the per-db ok line"
[[ -z "$(find $PRIV/work -mindepth 1 2>/dev/null)" ]] || fail "scratch space not cleaned: $(ls $PRIV/work)"
[[ ! -e $PRIV/errors/last-error-fake-one.log ]] || fail "a successful db left an error file"
pass "gcloud ran with the isolated CLOUDSDK_CONFIG and key path; no secret in the gcloud argv log, journal, status.json or the public state dir; scratch cleaned"

# Other schedules, other buckets.
units_start ikenga-backup@4hourly.service; [[ $RC -eq 0 ]] || fail "4hourly run exited $RC"
[[ "$(status_get '.databases["fake-fast"].object')" == gs://bucket-b/fake-fast/* ]] || fail "4hourly object: $(status_get '.databases["fake-fast"].object')"
units_start ikenga-backup@weekly.service; [[ $RC -eq 0 ]] || fail "weekly run exited $RC"
[[ -s "$FAKEGCS/buckets/$(status_get '.databases["fake-weekly"].object' | sed 's#^gs://##')" ]] || fail "weekly upload missing"
pass "4hourly and weekly runs use their own per-database buckets"

# ------------------------------------- 9. who can read what (the isolation)

# The accounts' own secrets (sync-accounts, same SECRETS_FILE): backup lines must not reach them.
provfull sync-accounts
[[ $RC -eq 0 ]] || fail "sync-accounts exited $RC"
for u in ada rex; do
  ! grep -qE 'FAKE_ONE_URL|FAKE_TWO_URL|UNUSED_BACKUP|GCS_KEY_B64|eyJ0eXBl' /etc/ikenga/secrets/ik-$u.env || fail "ik-$u's secrets file holds a backup secret"
done
grep -q '^ADA_ONLY=' /etc/ikenga/secrets/ik-ada.env && grep -q '^REX_ONLY=' /etc/ikenga/secrets/ik-rex.env && ! grep -q REX_ONLY /etc/ikenga/secrets/ik-ada.env || fail "account secrets changed shape"
! ls /etc/ikenga/secrets/ | grep -qi backup || fail "a backup file sits in the account secrets directory"
no_canary_in "account secret files" /etc/ikenga/secrets


for u in ada rex; do
  ! as $u cat $ETC/connections.env >/dev/null 2>&1 || fail "ik-$u can read the backup connection strings"
  ! as $u ls $ETC >/dev/null 2>&1 || fail "ik-$u can list $ETC"
  ! as $u cat $PRIV/gcs-key.json >/dev/null 2>&1 || fail "ik-$u can read the GCS key"
  ! as $u ls $PRIV >/dev/null 2>&1 || fail "ik-$u can list the private dir"
  as $u cat $STATUS | jq -e '.databases["fake-one"].last_success' >/dev/null || fail "ik-$u cannot read status.json"
  ! as $u sh -c "echo x >> $STATUS" 2>/dev/null || fail "ik-$u can write status.json"
  ! as $u sh -c "echo x > $STATE/planted" 2>/dev/null || fail "ik-$u can create files in the state dir"
done
! as_bu cat /etc/ikenga/secrets/ik-ada.env >/dev/null 2>&1 || fail "the backup user can read ik-ada's secrets"
! as_bu cat /etc/ikenga/secrets/ik-rex.env >/dev/null 2>&1 || fail "the backup user can read ik-rex's secrets"
! as_bu cat "$SECRETS" >/dev/null 2>&1 || fail "the backup user can read SECRETS_FILE"
! as_bu ls /etc/ikenga/secrets-backup >/dev/null 2>&1 || fail "the backup user can list the secrets backups"
as_bu cat $ETC/connections.env | grep -q '^FAKE_ONE_URL=' || fail "the backup user cannot read its own connection strings"
! as_bu sh -c "echo x >> $ETC/connections.env" 2>/dev/null || fail "the backup user can modify its own env file"
! as_bu sh -c "echo x >> /usr/local/lib/ikenga-backup/run-backup.sh" 2>/dev/null || fail "the backup user can modify the job script"
pass "isolation: accounts cannot read the backup env file, key or private dir (only status.json, read-only); the backup user cannot read any account's secrets, SECRETS_FILE or the secret backups; sync-accounts delivers no backup secret to any account"

# ---------------------------------------------------- 10. rerun is a no-op

cp -a $UNITS "$T/units-before"; sc_calls > "$T/calls-before"
prov
[[ $RC -eq 0 ]] || fail "rerun exited $RC"
grep -q 'no changes: the host already matches this profile' "$OUT" || fail "rerun reported changes: $(sed -n '/Summary/,$p' "$OUT")"
diff -r "$T/units-before" $UNITS >/dev/null || fail "rerun touched the unit files"
diff <(sc_calls) "$T/calls-before" | grep -E 'restart|enable|disable' && fail "rerun enabled/disabled/restarted a unit"
prov --dry-run
grep -q 'no changes' "$OUT" && ! grep -q 'would:' "$OUT" || fail "dry run after convergence still plans changes"
pass "converge: a rerun and a dry run after it report no changes and touch no unit"

# ------------------------------------------ 11. a changed schedule updates timers

SCHEDS=("daily=*-*-* 03:30:00 UTC"); write_profile
: > /var/lib/mock-systemctl/restarts.log
prov
[[ $RC -eq 0 ]] || fail "schedule change run exited $RC"
grep -qxF 'OnCalendar=*-*-* 03:30:00 UTC' $UNITS/ikenga-backup-daily.timer || fail "daily timer not updated"
grep -qxF 'OnCalendar=*-*-* 00/4:00:00 UTC' $UNITS/ikenga-backup-4hourly.timer || fail "4hourly timer changed"
[[ "$(cat /var/lib/mock-systemctl/restarts.log)" == ikenga-backup-daily.timer ]] || fail "restarted: $(cat /var/lib/mock-systemctl/restarts.log | tr '\n' ' ')"
grep -q 'reload' <<<"$(sc_calls)" || fail "no daemon-reload"
systemd-analyze calendar "*-*-* 03:30:00 UTC" >/dev/null || fail "the new calendar does not parse"
SCHEDS=(); write_profile; prov; grep -qxF 'OnCalendar=*-*-* 02:00:00 UTC' $UNITS/ikenga-backup-daily.timer || fail "default not restored"
pass "a changed schedule rewrites only that timer and restarts it; removing the override restores the default"

# ... and a schedule nobody uses any more loses its timer and its status entries.
sed -i '/"fake-weekly"/d' "$CONFIG_SRC"
prov
[[ $RC -eq 0 ]] || fail "config change run exited $RC"
[[ ! -e $UNITS/ikenga-backup-weekly.timer && ! -e /var/lib/mock-systemctl/enabled/ikenga-backup-weekly.timer ]] || fail "unused weekly timer survived"
[[ "$(status_get '.databases | keys | join(",")')" == "fake-fast,fake-one,fake-two" ]] || fail "status entries: $(status_get '.databases | keys | join(",")')"
write_config; prov
[[ -e $UNITS/ikenga-backup-weekly.timer ]] && [[ "$(status_get '.databases["fake-weekly"].last_attempt')" == null ]] || fail "weekly not restored"
pass "removing the last database of a schedule removes its timer and status entry; adding it back restores both"

# ----------------------------------- 12. failures are per database and named

check_fail() {   # db kind
  [[ "$(status_get ".databases[\"$1\"].last_attempt_ok")" == false ]] || fail "status: $1 not marked failed"
  [[ "$(status_get ".databases[\"$1\"].last_error_kind")" == "$2" ]] || fail "status: $1 kind is $(status_get ".databases[\"$1\"].last_error_kind"), wanted $2"
  [[ "$(status_get ".databases[\"$1\"].last_error_at")" =~ ^20 ]] || fail "status: $1 last_error_at"
  grep -q "db=$1 status=FAILED kind=$2" $JOURNAL/ikenga-backup@daily.service.log || fail "journal lacks 'db=$1 status=FAILED kind=$2'"
}
good_before="$(status_get '.databases["fake-two"].last_success')"; obj_before="$(status_get '.databases["fake-two"].object')"
sleep 1
# 12a. a wrong password for ONE database
TWO_URL="postgresql://bk2:WRONG-pw-never-valid@127.0.0.1:$PGPORT/app_two?sslmode=disable" write_secrets
prov
[[ $RC -eq 0 ]] || fail "secret change run exited $RC"
grep -q '~FAKE_TWO_URL' "$OUT" && ! grep -q 'WRONG-pw' "$OUT" || fail "the changed secret was not reported by name only"
: > $JOURNAL/ikenga-backup@daily.service.log
units_start ikenga-backup@daily.service
[[ $RC -ne 0 ]] || fail "a run with a failing database exited 0"
check_fail fake-two dump-failed
[[ "$(status_get '.databases["fake-one"].last_attempt_ok')" == true ]] || fail "the healthy database was affected by its neighbour's failure"
[[ "$(status_get '.databases["fake-two"].last_success')" == "$good_before" && "$(status_get '.databases["fake-two"].object')" == "$obj_before" ]] || fail "a failure erased the last success"
[[ "$(status_get '.schedules.daily.last_run_ok')" == false && "$(status_get '.schedules.daily.failed | join(",")')" == fake-two ]] || fail "status: schedule failed list"
ef=$PRIV/errors/last-error-fake-two.log
[[ "$(stat -c '%a %U' $ef)" == "600 $BU" ]] && grep -qi 'password authentication failed' $ef || fail "the raw error is not kept privately: $(ls -l $ef)"
no_canary_in "journal and status after a failure" $JOURNAL $STATUS $ef
! grep -q 'WRONG-pw' $JOURNAL/*.log $STATUS || fail "the wrong password leaked"
[[ -z "$(find $PRIV/work -mindepth 1)" ]] || fail "scratch not cleaned after a failure"
pass "a wrong password fails ONLY that database: dump-failed in status.json and the journal (names only), neighbour ok, last success kept, raw error private"

# 12b. a connection string that is not a URL
TWO_URL='mysql://user:FAKE-never-printed@host/db' write_secrets; prov
units_start ikenga-backup@daily.service; [[ $RC -ne 0 ]] || fail "bad URL run exited 0"
check_fail fake-two bad-connection-string
! grep -rq 'FAKE-never-printed' $JOURNAL $STATUS $PRIV/errors "$ALL" || fail "the bad URL's password leaked"
# 12c. an unsupported URL parameter
TWO_URL="postgres://bk2:x@127.0.0.1:$PGPORT/app_two?options=-c%20statement_timeout%3D1" write_secrets; prov
units_start ikenga-backup@daily.service; check_fail fake-two bad-connection-string
# 12d. the secret missing altogether: provisioning says so, the job reports no-secret
write_secrets; sed -i '/^\[backup\] FAKE_TWO_URL=/d' "$SECRETS"
prov
[[ $RC -ne 0 ]] && grep -q 'no \[backup\] line in SECRETS_FILE for: fake-two:FAKE_TWO_URL' "$OUT" || fail "a missing connection secret was not reported (rc=$RC)"
! grep -q '^FAKE_TWO_URL=' $ETC/connections.env || fail "the removed secret is still in the env file"
ls /etc/ikenga/secrets-backup/ | grep -q '^connections.env.bak-' || fail "the replaced env file was not backed up root-only"
[[ "$(stat -c '%a %U' /etc/ikenga/secrets-backup)" == "700 root" ]] || fail "secrets-backup is not root-only"
units_start ikenga-backup@daily.service; check_fail fake-two no-secret
# 12e. upload refused by the bucket
write_secrets; write_config deny-bucket; prov
units_start ikenga-backup@weekly.service; [[ $RC -ne 0 ]] || fail "refused upload exited 0"
[[ "$(status_get '.databases["fake-weekly"].last_error_kind')" == upload-failed ]] || fail "weekly kind: $(status_get '.databases["fake-weekly"].last_error_kind')"
write_config
# 12f. the GCS key is rejected: every database reports gcs-auth, none is dumped
KEY="$(mkkey revoked@fake-project.iam.gserviceaccount.com)" write_secrets; prov
[[ $RC -eq 0 ]] || fail "key change run exited $RC"
grep -q 'GCS key file' "$OUT" || fail "key change not reported"
[[ "$(jq -r .client_email $PRIV/gcs-key.json)" == revoked@* ]] || fail "key file not replaced"
: > $JOURNAL/ikenga-backup@daily.service.log; rm -f $FAKEGCS/calls.log
units_start ikenga-backup@daily.service; [[ $RC -ne 0 ]] || fail "gcs-auth run exited 0"
check_fail fake-one gcs-auth; check_fail fake-two gcs-auth
! grep -q 'storage cp' $FAKEGCS/calls.log || fail "uploads were attempted with a rejected key"
# back to healthy
write_secrets; prov
units_start ikenga-backup@daily.service; [[ $RC -eq 0 ]] || fail "recovery run exited $RC"
[[ "$(status_get '.databases["fake-two"].last_attempt_ok')" == true && "$(status_get '.databases["fake-two"].last_error_kind')" == null ]] || fail "status did not recover"
no_canary_in "everything after the failure tests" $JOURNAL $STATE/status.json $PRIV/errors "$ALL"
pass "failure kinds: bad-connection-string, unsupported parameter, no-secret (reported at provision time too, secrets backup root-only), upload-failed, gcs-auth (no dump attempted); recovery clears the error"

# ----------------------------------------- 13. disable keeps state, drops creds

cp $STATUS "$T/status-before"
ENABLED=0 write_profile
prov
[[ $RC -eq 0 ]] || fail "disable run exited $RC"
for s in daily 4hourly weekly; do
  [[ ! -e $UNITS/ikenga-backup-$s.timer && ! -e /var/lib/mock-systemctl/enabled/ikenga-backup-$s.timer && ! -e /var/lib/mock-systemctl/active/ikenga-backup-$s.timer ]] || fail "timer $s survived disabling"
done
[[ ! -e $SVC ]] || fail "service template survived disabling"
[[ ! -e $ETC/connections.env && ! -e $PRIV/gcs-key.json ]] || fail "credentials survived disabling"
[[ -s $STATUS && "$(status_get '.enabled')" == false && "$(status_get '.databases["fake-one"].last_success')" != null ]] || fail "status.json was lost or not marked disabled"
[[ -d $PRIV/errors && -f $PRIV/errors/last-error-fake-weekly.log ]] || fail "logs/state were removed"
[[ ! -e $PRIV/gcloud ]] || fail "gcloud's config (a copy of the service-account credentials) survived disabling: $(ls -A $PRIV/gcloud)"
getent passwd $BU >/dev/null && [[ -x /usr/local/lib/ikenga-backup/run-backup.sh ]] || fail "user or scripts removed"
ls /etc/ikenga/secrets-backup/ | grep -q '^connections.env.bak-' || fail "no root-only copy of the removed env file"
prov; grep -q 'no changes' "$OUT" || fail "disabled rerun reported changes: $(sed -n '/Summary/,$p' "$OUT")"
no_canary_in "state dir after disabling" $STATUS
pass "disable: timers and unit removed, credentials removed (root-only copy kept), status.json kept and marked enabled=false, error logs, user and scripts kept; a disabled rerun is a no-op"

ENABLED=1 write_profile
prov
[[ $RC -eq 0 ]] || fail "re-enable run exited $RC"
[[ "$(status_get '.enabled')" == true && "$(status_get '.databases["fake-one"].last_success')" != null ]] || fail "re-enable lost the history"
[[ -f $ETC/connections.env && -f $PRIV/gcs-key.json && -e $UNITS/ikenga-backup-daily.timer ]] || fail "re-enable did not restore"
units_start ikenga-backup@daily.service; [[ $RC -eq 0 ]] || fail "run after re-enable exited $RC"
# and the full provision flow knows about the action too
"$PROVISION" --help | grep -q 'backups' || fail "--help does not mention backups"
pass "re-enable restores timers, credentials and the status history; the job runs again"

echo "==> [Container] ALL BACKUP TESTS PASSED"
