#!/bin/bash
# The tunnels PORT LOCK and the squatting defence, run as root inside the
# privileged systemd container of test-portlock-container.sh. Two invocations:
#   before   everything that needs no reboot (and ends with the box set up)
#   after    the same box after a real container restart: the lock must have
#            been loaded by its unit at boot, before the tunnels started
set -euo pipefail

PHASE="${1:?usage: test-portlock-inside.sh before|after}"
T="$(mktemp -d)"
PROVISION=/work/provision.sh
PROV=/root/prov
PROFILE=$PROV/profile.env
OUT=$T/out
ALL=$T/all-output
: > "$ALL"
TU=ikenga-tunnel
BU=ikenga-backup
UNITS=/etc/systemd/system
LOCKUNIT=ikenga-tunnel-lock.service
LOCKFILE=/etc/ikenga-tunnels/lock.nft
TABLE=ikenga_tunnels
REMOTE=/etc/ssh-remote
KEY=/var/lib/ikenga-tunnel/.ssh/id_ed25519
T1='devotee-db=pgtunnel@127.0.0.1 5544:127.0.0.1:55432'
T2='alt-db=pgtunnel2@127.0.0.1:2222 5545:127.0.0.1:55433'
SQ=/var/lib/pl-squat
CHECKS=0

fail() {
  echo "FAIL: $*" >&2
  { echo "--- listeners ---"; ss -ltn; echo "--- tunnel units ---"; systemctl is-active devotee-db-tunnel.service alt-db-tunnel.service $LOCKUNIT 2>&1
    journalctl -u devotee-db-tunnel.service --no-pager -n 8 2>&1; echo "--- ruleset ---"; nft list ruleset 2>&1 | head -40; } >&2 || true
  [[ -f "$OUT" ]] && { echo "--- last provision output ---" >&2; tail -40 "$OUT" >&2; }
  exit 1
}
pass() { echo "==> [Container/$PHASE] $* PASSED"; }
ok() { local d="$1"; shift; CHECKS=$((CHECKS + 1)); "$@" >/dev/null || fail "$d"; }
no() { local d="$1"; shift; CHECKS=$((CHECKS + 1)); if "$@" >/dev/null 2>&1; then fail "$d"; fi; }

RC=0
prov() {   # [args]: `tunnels` with the current profile; RC and $OUT
  if "$PROVISION" tunnels --profile "$PROFILE" "$@" > "$OUT" 2>&1; then RC=0; else RC=$?; fi
  cat "$OUT" >> "$ALL"
}
profile() { printf '%s\n' "$@" > "$PROFILE"; chown root:root "$PROFILE"; chmod 0600 "$PROFILE"; }
out_has() { CHECKS=$((CHECKS + 1)); grep -qE -- "$1" "$OUT"; }
wait_for() { local n="$1" i; shift; for i in $(seq 1 "$n"); do "$@" && return 0; sleep 1; done; return 1; }
listening() { ss -ltn | grep -qE "127\.0\.0\.1:$1\s"; }
not_listening() { ! listening "$1"; }
unit_state() { systemctl is-active "$1" 2>/dev/null || true; }

as_user() {   # user cmd... (NAME=value words before cmd become its environment)
  local u="$1"; shift
  ( cd / && setpriv --reuid="$(id -u "$u")" --regid="$(id -g "$u")" --clear-groups env -i HOME=/nonexistent PATH=/usr/local/bin:/usr/bin:/bin "$@" )
}
# The two client probes (installed once; they run as the account under test).
cat > /usr/local/bin/pl-connect <<'EOF'
#!/bin/bash
# pl-connect <host> <port>: exit 0 if the TCP connection is accepted
exec 9<>"/dev/tcp/$1/$2"
EOF
cat > /usr/local/bin/pl-reach <<'EOF'
#!/bin/bash
# pl-reach <host> <port> <backend port>: exit 0 only if connected AND the fake postgres behind answered
exec 9<>"/dev/tcp/$1/$2" || exit 1
read -t 5 -u 9 b || exit 1
printf 'ping\n' >&9
read -t 5 -u 9 r || exit 1
[[ "$b" == "fake-postgres-$3" && "$r" == ping ]]
EOF
chmod 0755 /usr/local/bin/pl-connect /usr/local/bin/pl-reach
reaches() { as_user "$1" timeout 10 pl-reach "${4:-127.0.0.1}" "$2" "$3"; }          # user port backend [host]
# Rejected: the connect fails, and fast (a reset from the rule, not a hang until a timeout).
rejected() {   # user port [host]
  local t0=$SECONDS rc=0
  as_user "$1" timeout 8 pl-connect "${3:-127.0.0.1}" "$2" 2>/dev/null || rc=$?
  [[ $rc -ne 0 && $((SECONDS - t0)) -le 3 ]]
}
counters() { nft list table inet "$TABLE" 2>/dev/null | grep -oE 'packets [0-9]+' | awk '{s+=$2} END{print s+0}'; }
lock_rules() { nft list table inet "$TABLE" 2>/dev/null; }
# The uids the first rule lets through, e.g. "0,995" (nft prints a one-element set without braces).
lock_uidset() { lock_rules | grep -m1 'skuid' | sed -E 's/.*skuid != (.*) counter .*/\1/' | tr -d '{} '; }

# ------------------------------------------------------------------- fixture

HKEY=""
start_fixture() {   # the fake remote host: sshd on 127.0.0.1:22 and :2222, and two "postgres" listeners
  systemctl start fake-remote-sshd
  local p
  for p in 55432 55433; do
    systemctl is-active --quiet "fake-pg-$p" 2>/dev/null || systemd-run --quiet --unit="fake-pg-$p" --collect socat "TCP-LISTEN:$p,bind=127.0.0.1,fork,reuseaddr" "SYSTEM:echo fake-postgres-$p; cat" >/dev/null
  done
  wait_for 10 listening 55432 || fail "fake postgres did not start"
  wait_for 10 listening 2222 || fail "fake remote sshd did not start"
}

if [[ $PHASE == before ]]; then
  systemctl mask ssh.service ssh.socket >/dev/null 2>&1 || true
  mkdir -p "$REMOTE" "$PROV"; chmod 0755 "$REMOTE"
  ssh-keygen -q -t ed25519 -N '' -f "$REMOTE/ssh_host_ed25519_key"
  cat > "$REMOTE/sshd_config" <<EOF
Port 22
Port 2222
ListenAddress 127.0.0.1
HostKey $REMOTE/ssh_host_ed25519_key
PasswordAuthentication no
KbdInteractiveAuthentication no
PubkeyAuthentication yes
AuthorizedKeysFile $REMOTE/authorized_keys.%u
UsePAM no
AllowTcpForwarding yes
LogLevel VERBOSE
EOF
  for u in pgtunnel pgtunnel2; do useradd --system --no-create-home --shell /usr/sbin/nologin "$u"; usermod -p '*' "$u"; done
  cat > /etc/systemd/system/fake-remote-sshd.service <<'EOF'
[Service]
RuntimeDirectory=sshd
ExecStart=/usr/sbin/sshd -D -e -f /etc/ssh-remote/sshd_config
Restart=on-failure
EOF
  systemctl daemon-reload
  # An Ikenga account, an agent account, an unrelated local user.
  groupadd -g 20001 ik-ada;   useradd -u 20001 -g 20001 -M -d /nonexistent -s /bin/sh ik-ada
  groupadd -g 20002 ik-agent; useradd -u 20002 -g 20002 -M -d /nonexistent -s /bin/sh ik-agent
  useradd -u 1500 -M -d /nonexistent -s /bin/sh plain
  echo SENSITIVE > /root/sensitive; chmod 0600 /root/sensitive
  start_fixture
  ufw default deny incoming >/dev/null; ufw allow 22/tcp >/dev/null
  ufw --force enable >/dev/null 2>&1 || fail "ufw would not enable in the container (needs the host's netfilter)"
  ufw status | grep -q '^Status: active' || fail "ufw is not active"
fi
HKEY="$(cut -d' ' -f1,2 "$REMOTE/ssh_host_ed25519_key.pub")"
KHP="TUNNEL_KNOWN_HOSTS=(\"127.0.0.1 $HKEY\" \"[127.0.0.1]:2222 $HKEY\")"
std_profile() { profile "TUNNEL_FROM=127.0.0.1" "BACKUPS_ENABLED=1" "$KHP" "$@" "TUNNELS=(\"$T1\" \"$T2\")"; }
authorise_remote() {
  local pub; pub="$(cut -d' ' -f1,2 "$KEY.pub")"
  printf 'restrict,port-forwarding,permitopen="127.0.0.1:55432",from="127.0.0.1" %s\n' "$pub" > "$REMOTE/authorized_keys.pgtunnel"
  printf 'restrict,port-forwarding,permitopen="127.0.0.1:55433",from="127.0.0.1" %s\n' "$pub" > "$REMOTE/authorized_keys.pgtunnel2"
  chmod 0644 "$REMOTE"/authorized_keys.*
}

# The isolation matrix the lock must give, for tunnel ports 5544 and 5545.
check_matrix() {   # [allowed users, space separated; root is implied]
  local allow=" ${1:-} root " u port be
  for port in 5544 5545; do
    be=$((port == 5544 ? 55432 : 55433))
    for u in root ikenga-backup ik-ada ik-agent plain "$TU"; do
      if [[ "$allow" == *" $u "* ]]; then
        ok "$u reaches 127.0.0.1:$port" reaches "$u" "$port" "$be"
      else
        ok "$u is REJECTED on 127.0.0.1:$port" rejected "$u" "$port"
      fi
    done
  done
}

# ============================================================== phase: before

if [[ $PHASE == before ]]; then

# ----------------------------------------------- 1. validation of the new key

refuse() {   # description, regexp, profile lines...
  local d="$1" re="$2"; shift 2
  profile "$@"; prov
  [[ $RC -ne 0 ]] || fail "$d: provisioning succeeded"
  grep -qE -- "$re" "$OUT" || fail "$d: refused, but not with /$re/"
  { ! compgen -G "$UNITS/*-tunnel.service" >/dev/null && [[ ! -e $LOCKFILE && ! -e $UNITS/$LOCKUNIT ]] && ! id "$TU" >/dev/null 2>&1; } || fail "$d: state was created before the refusal"
  CHECKS=$((CHECKS + 1))
}
refuse "TUNNEL_ALLOW_USERS a bad name" 'TUNNEL_ALLOW_USERS entry 1' "TUNNEL_FROM=127.0.0.1" "$KHP" "TUNNEL_ALLOW_USERS=('x;touch /tmp/pwned')" "TUNNELS=(\"$T1\")"
refuse "TUNNEL_ALLOW_USERS a scalar" 'TUNNEL_ALLOW_USERS must be a bash array' "TUNNEL_FROM=127.0.0.1" "$KHP" "TUNNEL_ALLOW_USERS=ik-ada" "TUNNELS=(\"$T1\")"
refuse "TUNNEL_ALLOW_USERS a user that does not exist" "names 'nosuchuser'" "TUNNEL_FROM=127.0.0.1" "$KHP" "TUNNEL_ALLOW_USERS=(nosuchuser)" "TUNNELS=(\"$T1\")"
[[ ! -e /tmp/pwned ]] || fail "a TUNNEL_ALLOW_USERS value reached a shell"
pass "TUNNEL_ALLOW_USERS is validated before anything changes (bad name, scalar, unknown user)"

# ------------------------------------------------------ 2. dry run, fresh host

std_profile
prov --dry-run
[[ $RC -eq 0 ]] || fail "dry run failed"
out_has 'would: /etc/ikenga-tunnels/lock.nft written \(ports: 5544 5545 / allowed: root\(0\)\)' || fail "dry run does not plan the rules file"
out_has 'would: unit ikenga-tunnel-lock.service installed' || fail "dry run does not plan the lock unit"
out_has 'would: unit devotee-db-tunnel.service: requires ikenga-tunnel-lock.service' || fail "dry run does not plan the drop-in"
out_has 'ikenga-backup does not exist yet' || fail "dry run: the not-yet-created backup user is not mentioned"
{ [[ ! -e /etc/ikenga-tunnels ]] && [[ ! -e $UNITS/$LOCKUNIT ]] && ! nft list table inet $TABLE >/dev/null 2>&1; } || fail "the dry run changed the host"
pass "dry run plans the lock and changes nothing"

# --------------- 3. fresh converge; the backup user does not exist yet (pending)

prov
[[ $RC -eq 0 ]] || fail "fresh provisioning failed"
{ out_has 'ikenga-backup does not exist yet' && out_has 'unit ikenga-tunnel-lock.service enabled and started'; } || fail "pending backup user / lock start not reported"
[[ "$(stat -c '%a %U %G' /etc/ikenga-tunnels)" == "755 root root" ]] || fail "lock dir mode"
[[ "$(stat -c '%a %U %G' $LOCKFILE)" == "644 root root" ]] || fail "lock file mode"
[[ "$(stat -c '%a %U %G' $UNITS/$LOCKUNIT)" == "644 root root" ]] || fail "lock unit mode"
systemctl is-enabled --quiet $LOCKUNIT || fail "the lock unit is not enabled"
[[ "$(unit_state $LOCKUNIT)" == active ]] || fail "the lock unit is not active"
for u in devotee-db alt-db; do
  d="$UNITS/$u-tunnel.service.d/10-ikenga-lock.conf"
  grep -qxF 'Requires=ikenga-tunnel-lock.service' "$d" && grep -qxF 'After=ikenga-tunnel-lock.service' "$d" || fail "$d lacks Requires/After"
  [[ "$(systemctl show -p Requires --value $u-tunnel.service)" == *ikenga-tunnel-lock.service* ]] || fail "systemd does not see $u requiring the lock"
done
nft list table inet $TABLE >/dev/null || fail "the table is not loaded"
# Only root is allowed: the backup user does not exist yet, so { 0 }.
[[ "$(lock_uidset)" == 0 ]] || fail "expected only uid 0 while the backup user does not exist: $(lock_rules)"
for p in 5544 5545; do
  for fam in ip ip6; do lock_rules | grep -qE "$fam daddr [0-9a-f:.]+ tcp dport $p meta skuid != .* counter packets [0-9]+ bytes [0-9]+ reject with tcp reset comment \"ikenga:lock:[a-z-]+:$fam:$p:0\"" || fail "no $fam rule for port $p"; done
done
# Exactly this: one table, one base chain, 4 port rules, accept policy. Nothing else is touched.
[[ "$(lock_rules | grep -c 'dport')" == 4 ]] || fail "expected 4 port rules"
lock_rules | grep -qE 'type filter hook output priority (filter|0); policy accept;' || fail "chain is not an accept-policy output hook"
authorise_remote
systemctl restart devotee-db-tunnel.service alt-db-tunnel.service
wait_for 25 listening 5544 || { journalctl -u devotee-db-tunnel.service --no-pager | tail -15 >&2; fail "devotee-db did not come up"; }
wait_for 25 listening 5545 || fail "alt-db did not come up"
ok "root reaches the tunnel" reaches root 5544 55432
pass "fresh converge: table, file, unit, drop-ins; the missing backup user is noted, the lock is root-only"

# ---- the backup user appears (the backups phase creates it): the next run allows it
useradd --system --no-create-home --shell /usr/sbin/nologin "$BU"
prov
{ [[ $RC -eq 0 ]] && out_has 'lock.nft written' && out_has 'port lock reloaded \(rules changed\)'; } || fail "creating the backup user should reload the lock with its uid"
BUID="$(id -u $BU)"
[[ "$(lock_uidset)" == "0,$BUID" ]] || fail "the backup uid $BUID is not in the set: $(lock_rules)"
compgen -G "$LOCKFILE.bak-*" >/dev/null || fail "the previous rules file was not kept"
pass "once the backup user exists the lock is converged to allow it (the old file is kept aside)"

# ------------------------------------------------------ 4. who can connect

check_matrix "$BU"
[[ "$(counters)" -gt 0 ]] || fail "the reject counters did not move: the rejections did not come from the nft rules"
# Positive control: it is the table that blocks them. Take it away and ik-ada gets in.
nft delete table inet $TABLE
ok "control: with the table gone ik-ada reaches the port" reaches ik-ada 5544 55432
prov
{ [[ $RC -eq 0 ]] && out_has 'port lock reloaded \(the live table had drifted\)'; } || fail "a deleted table should be restored by a rerun"
ok "ik-ada is rejected again" rejected ik-ada 5544
# IPv6 loopback and an unlocked port.
ip -6 addr show dev lo 2>/dev/null | grep -q '::1' || { sysctl -qw net.ipv6.conf.lo.disable_ipv6=0 2>/dev/null || true; }
if ip -6 addr show dev lo | grep -q '::1'; then
  systemd-run --quiet --unit=pl-fake-pg6 --collect socat "TCP6-LISTEN:5544,bind=[::1],fork,reuseaddr" "SYSTEM:echo fake-postgres-55432; cat" >/dev/null
  sleep 1
  ok "control: root reaches [::1]:5544" reaches root 5544 55432 ::1
  ok "backup user reaches [::1]:5544" reaches "$BU" 5544 55432 ::1
  ok "ik-ada is REJECTED on [::1]:5544" rejected ik-ada 5544 ::1
  ok "ik-agent is REJECTED on [::1]:5544" rejected ik-agent 5544 ::1
  systemctl stop pl-fake-pg6
else
  fail "no ::1 on lo in this container: the ip6 rules are untested"
fi
systemd-run --quiet --unit=pl-fake-open --collect socat "TCP-LISTEN:5599,bind=127.0.0.1,fork,reuseaddr" "SYSTEM:echo fake-postgres-5599; cat" >/dev/null
wait_for 10 listening 5599
ok "a port that is not a tunnel port stays open to everyone (ik-ada, 5599)" reaches ik-ada 5599 5599
systemctl stop pl-fake-open
pass "the matrix: root and the backup user reach 127.0.0.1 and ::1; ik-ada, ik-agent, a plain user and the tunnel user are rejected; only tunnel ports are affected"

# -------------------------------------------------------------- 5. rerun is a no-op

INV1="$(systemctl show -p InvocationID --value devotee-db-tunnel.service)"
INV2="$(systemctl show -p InvocationID --value alt-db-tunnel.service)"
LOCKTS="$(systemctl show -p ActiveEnterTimestampMonotonic --value $LOCKUNIT)"
reaches ik-ada 5544 55432 >/dev/null 2>&1 || true   # (rejected; also bumps the counter)
C1="$(counters)"
SUM1="$(sha256sum $LOCKFILE $UNITS/$LOCKUNIT $UNITS/*-tunnel.service.d/10-ikenga-lock.conf | sha256sum)"
prov
{ [[ $RC -eq 0 ]] && out_has 'no changes: the host already matches this profile'; } || fail "rerun should report no changes"
[[ "$(counters)" -ge "$C1" && "$C1" -gt 0 ]] || fail "rerun reloaded the table (its counters were reset)"
[[ "$(sha256sum $LOCKFILE $UNITS/$LOCKUNIT $UNITS/*-tunnel.service.d/10-ikenga-lock.conf | sha256sum)" == "$SUM1" ]] || fail "rerun rewrote the lock files"
[[ "$(systemctl show -p InvocationID --value devotee-db-tunnel.service)" == "$INV1" ]] || fail "rerun restarted devotee-db"
[[ "$(systemctl show -p InvocationID --value alt-db-tunnel.service)" == "$INV2" ]] || fail "rerun restarted alt-db"
[[ "$(systemctl show -p ActiveEnterTimestampMonotonic --value $LOCKUNIT)" == "$LOCKTS" ]] || fail "rerun restarted the lock unit"
prov --dry-run
{ [[ $RC -eq 0 ]] && out_has 'no changes' && ! out_has 'would:'; } || fail "dry run of a converged host should say no changes"
pass "rerun: no changes, no reload (counters kept), no tunnel restarted, files untouched"

# ----------------------- 6. ufw cannot undo it: reset, flush, drift, hostile edits

ufw --force reset >/dev/null 2>&1 || fail "ufw reset failed"
ufw status | grep -q '^Status: inactive' || fail "ufw was not reset"
lock_rules | grep -qE 'ikenga:lock:devotee-db:ip:5544' || fail "ufw --force reset removed the lock table"
check_matrix "$BU"
nft list tables | grep -qxF "table inet $TABLE" || fail "our table is not listed"
[[ "$(nft list tables | grep -c "table inet $TABLE")" == 1 ]] || fail "more than one copy of the table"
ufw default deny incoming >/dev/null; ufw allow 22/tcp >/dev/null; ufw --force enable >/dev/null 2>&1
ufw status | grep -q '^Status: active' || fail "ufw would not re-enable"
check_matrix "$BU"
prov; { [[ $RC -eq 0 ]] && out_has 'no changes'; } || fail "after a ufw reset a rerun should still find nothing to do"
# ufw's own rules do not appear in our table, ours do not appear in ufw's.
if lock_rules | grep -qi 'ufw'; then fail "ufw rules leaked into the lock table"; fi
if iptables -S 2>/dev/null | grep -q 'ikenga'; then fail "the lock table appears in iptables: it is not separate"; fi
# iptables flush (what an admin or a docker restart may do) does not touch it either.
iptables -P INPUT ACCEPT; iptables -P FORWARD ACCEPT; iptables -F; iptables -X
check_matrix "$BU"
pass "survives ufw --force reset, ufw re-enable and an iptables flush (separate table); a rerun still reports no changes"

# a hostile edit: an extra accept rule inserted in front, and a rule removed
nft insert rule inet $TABLE output ip daddr 127.0.0.1 tcp dport 5544 accept
ok "control: with the extra accept rule ik-ada gets in" reaches ik-ada 5544 55432
prov
{ [[ $RC -eq 0 ]] && out_has 'port lock reloaded \(the live table had drifted\)'; } || fail "an extra rule is drift and should be reloaded away"
ok "ik-ada rejected again after the repair" rejected ik-ada 5544
h="$(nft -a list table inet $TABLE | grep 'ip6 daddr ::1 tcp dport 5545' | grep -oE 'handle [0-9]+' | awk '{print $2}')"
nft delete rule inet $TABLE output handle "$h"
prov
{ [[ $RC -eq 0 ]] && out_has 'drifted'; } || fail "a missing rule is drift and should be restored"
[[ "$(lock_rules | grep -c dport)" == 4 ]] || fail "the table was not restored to 4 rules"
pass "drift (extra accept rule, deleted rule, deleted table) is detected and repaired by a rerun"

# `nft flush ruleset` (the stock nftables.service does this at load) then the unit's reload path: equal to what boot does
nft flush ruleset      # (also drops ufw's chains: exactly why the lock does not live in nftables.service)
no "after nft flush ruleset the lock is gone" lock_rules
systemctl restart $LOCKUNIT      # (a restart passes down to the tunnels that require the lock; a reload does not)
ok "the unit restores the table" lock_rules
wait_for 40 listening 5544 && wait_for 40 listening 5545 || fail "the tunnels did not come back after the lock was restarted"
INVR="$(systemctl show -p InvocationID --value devotee-db-tunnel.service)"
systemctl reload $LOCKUNIT
ok "reload keeps it" lock_rules
[[ "$(systemctl show -p InvocationID --value devotee-db-tunnel.service)" == "$INVR" ]] || fail "reloading the lock restarted a tunnel"
check_matrix "$BU"
ufw --force reload >/dev/null 2>&1 || true
pass "the unit reloads the table from its file after a hostile nft flush ruleset (same path as boot)"

# --------------------------------------------------- 7. fail closed

systemctl stop $LOCKUNIT
no "table gone after stopping the lock unit" lock_rules
wait_for 10 not_listening 5544 || fail "stopping the lock did not stop the tunnel (Requires=)"
[[ "$(unit_state devotee-db-tunnel.service)" != active ]] || fail "the tunnel is still active without its lock"
systemctl start devotee-db-tunnel.service alt-db-tunnel.service
wait_for 25 listening 5544 || fail "starting a tunnel did not start its lock and itself"
ok "the lock came back with the tunnel" lock_rules
cp $LOCKFILE $T/lock.good
echo 'this is not nft' > $LOCKFILE
systemctl restart $LOCKUNIT 2>/dev/null && fail "a broken rules file loaded"
[[ "$(unit_state $LOCKUNIT)" == failed ]] || fail "the lock unit should be failed, is $(unit_state $LOCKUNIT)"
wait_for 10 not_listening 5544 || fail "a failed lock left the tunnel up"
[[ "$(unit_state devotee-db-tunnel.service)" != active ]] || fail "the tunnel runs although its lock failed to load"
systemctl start devotee-db-tunnel.service 2>/dev/null && [[ "$(unit_state devotee-db-tunnel.service)" == active ]] && fail "a tunnel started without its lock"
prov
{ [[ $RC -eq 0 ]] && out_has 'lock.nft written' && out_has 'unit ikenga-tunnel-lock.service enabled and started'; } || fail "a rerun should repair the file and restart the lock"
wait_for 30 listening 5544 || fail "the tunnels did not come back after the repair"
cmp -s $LOCKFILE $T/lock.good || fail "the repaired rules file differs from the good one"
check_matrix "$BU"
pass "fail closed: no lock, no tunnel (stop, and a rules file nft rejects); a rerun repairs it and the tunnels return"

# ------------------------------------------ 8. TUNNEL_ALLOW_USERS, and symlinks

std_profile "TUNNEL_ALLOW_USERS=(ik-ada)"
prov
{ [[ $RC -eq 0 ]] && out_has 'port lock reloaded \(rules changed\)'; } || fail "changing TUNNEL_ALLOW_USERS should reload the lock"
check_matrix "ik-ada"
std_profile "TUNNEL_ALLOW_USERS=()"
prov; [[ $RC -eq 0 ]] || fail "TUNNEL_ALLOW_USERS=() failed"
check_matrix ""
std_profile "TUNNEL_ALLOW_USERS=(ik-ada $BU ik-ada root)"
prov; [[ $RC -eq 0 ]] || fail "a list with duplicates and root failed"
[[ "$(lock_uidset)" == "0,$BUID,20001" ]] || fail "duplicates/root not folded: $(lock_rules | grep skuid | head -1)"
std_profile
prov; [[ $RC -eq 0 ]] || fail "back to the default failed"
check_matrix "$BU"
# the rules file as a symlink: refused, nothing written through it
cp -p $LOCKFILE $T/lock.real; rm -f $LOCKFILE; ln -s /root/sensitive $LOCKFILE
std_profile "TUNNEL_ALLOW_USERS=(ik-ada)"
prov
{ [[ $RC -ne 0 ]] && out_has 'symbolic link'; } || fail "a symlinked rules file should be refused"
[[ "$(cat /root/sensitive)" == SENSITIVE ]] || fail "root wrote through the rules-file symlink"
rm -f $LOCKFILE; cp -p $T/lock.real $LOCKFILE
std_profile; prov; { [[ $RC -eq 0 ]] && out_has 'no changes'; } || fail "state not clean after the symlink check"
pass "TUNNEL_ALLOW_USERS: a list, (), duplicates/root folded, back to the default; a symlinked rules file is refused"

# --------------------------------------------- 9. a tunnel removed loses its rules

profile "TUNNEL_FROM=127.0.0.1" "BACKUPS_ENABLED=1" "$KHP" "TUNNELS=(\"$T1\")"
prov
{ [[ $RC -eq 0 ]] && out_has 'unit alt-db-tunnel.service stopped, disabled and removed' && out_has 'port lock reloaded \(rules changed\)'; } || fail "removing a tunnel should say so and reload the lock"
[[ "$(lock_rules | grep -c dport)" == 2 ]] || fail "alt-db's rules are still there: $(lock_rules)"
no "5545 is no longer in the table" bash -c "nft list table inet $TABLE | grep -q 5545"
[[ ! -e "$UNITS/alt-db-tunnel.service.d/10-ikenga-lock.conf" && ! -d "$UNITS/alt-db-tunnel.service.d" ]] || fail "alt-db's drop-in was left behind"
ok "devotee-db is still locked" rejected ik-ada 5544
profile "TUNNEL_FROM=127.0.0.1" "$KHP"; prov    # TUNNELS not mentioned: hands off
{ [[ $RC -eq 0 ]] && out_has 'does not define TUNNELS'; } || fail "no TUNNELS should be a no-op"
ok "hands off: the lock is untouched" lock_rules
std_profile; prov; [[ $RC -eq 0 ]] || fail "re-adding the second tunnel failed"
[[ "$(lock_rules | grep -c dport)" == 4 ]] || fail "alt-db's rules did not come back"
authorise_remote; systemctl restart alt-db-tunnel.service; wait_for 25 listening 5545 || fail "alt-db did not come back"
check_matrix "$BU"
pass "removing a tunnel removes its rules and drop-in; no TUNNELS in the profile leaves the lock alone"

# --------------------------------------------------------- 10. squatting

# What the lock cannot do: another account can still BIND a tunnel's port while
# the tunnel is down. Here ik-agent does, with a fake Postgres that asks for a
# cleartext password; the real run-backup.sh (as the backup user) and the real
# libpq connect to it. The fake server's own log is the evidence.
mkdir -p $SQ/etc $SQ/state/status $SQ/state/private $SQ/bin /var/tmp/pl-agent
chown -R "$BU:$BU" $SQ/state; chmod 0700 $SQ/state/private
chown ik-agent:ik-agent /var/tmp/pl-agent
printf '{}' > $SQ/state/private/gcs-key.json; chown "$BU:$BU" $SQ/state/private/gcs-key.json; chmod 0600 $SQ/state/private/gcs-key.json
printf '#!/bin/sh\nexit 0\n' > $SQ/bin/gcloud; chmod 0755 $SQ/bin/gcloud
PW='FAKE-squat-pw-7c41e'
printf 'SQ_DB=postgres://bk:%s@127.0.0.1:5544/sqdb\nSQ_DB_URLAUTH=postgres://bk:%s@127.0.0.1:5544/sqdb?require_auth=scram-sha-256\nSQ_DB_BADAUTH=postgres://bk:%s@127.0.0.1:5544/sqdb?require_auth=bogus\nSQ_DB_WEAK=postgres://bk:%s@127.0.0.1:5544/sqdb?require_auth=none\n' "$PW" "$PW" "$PW" "$PW" > $SQ/etc/connections.env
chown root:"$BU" $SQ/etc/connections.env; chmod 0640 $SQ/etc/connections.env
FAKELOG=/var/tmp/pl-agent/fake.log
cfg() {   # secret, tunnel_ports (json array or "none"), require_auth (json or "none")
  local ra=''; [[ "$3" == none ]] || ra=", \"require_auth\": $3"
  local tp=''; [[ "$2" == none ]] || tp=", \"tunnel_ports\": $2"
  printf '{"databases":[{"name":"sq-db","connection_secret":"%s","schedule":"daily","gcs_bucket":"sq-bucket","enabled":true%s}]%s}\n' "$1" "$ra" "$tp" > $SQ/etc/config.json
  chmod 0644 $SQ/etc/config.json
}
run_backup() {
  rm -rf "${SQ:?}"/state/status/* 2>/dev/null || true
  set +e
  as_user "$BU" BACKUP_CONFIG_FILE=$SQ/etc/config.json BACKUP_ENV_FILE=$SQ/etc/connections.env BACKUP_STATE_DIR=$SQ/state \
    BACKUP_PG_MIN_MAJOR=16 PATH=$SQ/bin:/usr/lib/postgresql/16/bin:/usr/bin:/bin timeout 60 /work/backup/run-backup.sh --schedule daily > $T/bk.out 2>&1
  BRC=$?
  set -e
  cat $T/bk.out >> "$ALL"
}
kind() { jq -r '.databases["sq-db"].last_error_kind // "none"' $SQ/state/status/status.json; }
squatter() {   # mode: bind 127.0.0.1:5544 as ik-agent
  : > $FAKELOG; chown ik-agent:ik-agent $FAKELOG
  systemd-run --quiet --unit=pl-squat --collect --uid=ik-agent --gid=ik-agent python3 /tests/test-portlock-fakepg.py --port 5544 --mode "$1" --log $FAKELOG >/dev/null
  wait_for 10 listening 5544 || fail "the squatter did not bind 5544"
}
unsquat() { systemctl stop pl-squat 2>/dev/null || true; wait_for 5 not_listening 5544 || true; }
got_password() { grep -qxF "PASSWORD-RECEIVED $PW" $FAKELOG; }

systemctl stop devotee-db-tunnel.service       # the tunnel is down: the port is free
wait_for 10 not_listening 5544 || fail "the tunnel did not stop"
squatter cleartext
SQPID="$(systemctl show -p MainPID --value pl-squat)"
[[ "$(ps -o user= -p "$SQPID" | tr -d ' ')" == ik-agent ]] || fail "the squatter is not running as ik-agent"
ok "the lock lets the backup user connect (so the squatter gets the connection): control" as_user "$BU" timeout 5 pl-connect 127.0.0.1 5544
ok "...and ik-agent can bind a locked port (nft has no say in bind)" listening 5544

# (a) control: nothing pins the auth method. The password goes to the squatter.
cfg SQ_DB none none; run_backup
[[ $BRC -ne 0 ]] || fail "(a) the backup against a fake server succeeded?"
got_password || fail "(a) control: the squatter did not receive the password; the test proves nothing. log: $(cat $FAKELOG)"
echo "    squatter's log (a): $(tr '\n' '|' < $FAKELOG | sed "s/$PW/<fake-canary>/")"
pass "squat (a) CONTROL: without require_auth the squatter receives the backup's password in cleartext"
# (b) per-database require_auth
: > $FAKELOG; cfg SQ_DB none '"scram-sha-256"'; run_backup
[[ $BRC -ne 0 ]] || fail "(b) should fail"
no "(b) the password reached the squatter" got_password
grep -qxF 'client-closed-without-password' $FAKELOG || fail "(b) the client did not hang up at the auth request: $(cat $FAKELOG)"
[[ "$(kind)" == auth-refused ]] || fail "(b) status kind is $(kind), want auth-refused"
grep -q 'status=FAILED kind=auth-refused' $T/bk.out || fail "(b) the journal line should say kind=auth-refused"
grep -qF 'authentication method requirement' $SQ/state/private/errors/last-error-sq-db.log || fail "(b) the libpq refusal is not in the error log"
if grep -qF "$PW" $T/bk.out $SQ/state/private/errors/last-error-sq-db.log; then fail "(b) the password is in the output"; fi
echo "    squatter's log (b): $(tr '\n' '|' < $FAKELOG | sed "s/$PW/<fake-canary>/")"
pass "squat (b): with \"require_auth\": \"scram-sha-256\" libpq refuses the cleartext request, the password is NOT sent (server log: client-closed-without-password), status kind auth-refused"
# (c) the default for a connection that ends at a tunnel port
: > $FAKELOG; cfg SQ_DB '[5544]' none; run_backup
no "(c) the password reached the squatter" got_password
[[ "$(kind)" == auth-refused ]] || fail "(c) with tunnel_ports=[5544] the default should refuse; kind=$(kind)"
pass "squat (c): config tunnel_ports=[5544] and no per-db field: the default scram-sha-256 applies (password not sent)"
# (d) opt-out
: > $FAKELOG; cfg SQ_DB '[5544]' false; run_backup
got_password || fail "(d) \"require_auth\": false should opt out (the password goes through again)"
pass "squat (d): \"require_auth\": false opts one database out (control: the squatter gets the password again)"
# (e) the port is not a tunnel port: no default
: > $FAKELOG; cfg SQ_DB '[6000]' none; run_backup
got_password || fail "(e) a port that is not a tunnel port must not get the default"
pass "squat (e): the default applies only to a tunnel port (tunnel_ports=[6000], connecting to 5544: no require_auth)"
# (f) the URL parameter is accepted; config wins over a weaker URL value; garbage is refused
: > $FAKELOG; cfg SQ_DB_URLAUTH none none; run_backup
no "(f) URL require_auth: password reached the squatter" got_password
[[ "$(kind)" == auth-refused ]] || fail "(f) require_auth in the URL: kind=$(kind)"
: > $FAKELOG; cfg SQ_DB_WEAK none '"scram-sha-256"'; run_backup
no "(f) config should win over require_auth=none in the URL" got_password
[[ "$(kind)" == auth-refused ]] || fail "(f) config over URL: kind=$(kind)"
cfg SQ_DB_BADAUTH none none; run_backup
[[ "$(kind)" == bad-connection-string ]] || fail "(f) require_auth=bogus in the URL: kind=$(kind)"
pass "squat (f): require_auth is accepted in the URL, the config field wins over a weaker URL value, a bogus value is a bad-connection-string"
# (g) a server that skips authentication and answers anyway (forged data)
unsquat; squatter trust
cfg SQ_DB none none; run_backup
grep -qxF 'query-received' $FAKELOG || fail "(g) control: without require_auth the forged server was given the session; log: $(cat $FAKELOG)"
: > $FAKELOG; cfg SQ_DB none '"scram-sha-256"'; run_backup
[[ "$(kind)" == auth-refused ]] || fail "(g) kind=$(kind)"
no "(g) a query reached the forged server" grep -qxF query-received $FAKELOG
pass "squat (g): a server that never authenticates is refused too (control: without require_auth it is given the session)"
unsquat
# (h) the tunnel itself notices: it cannot bind a squatted port
squatter cleartext
systemctl start devotee-db-tunnel.service || true
sleep 6
[[ "$(unit_state devotee-db-tunnel.service)" != active ]] || fail "the tunnel claims to be active while the port is squatted"
journalctl -u devotee-db-tunnel.service --no-pager | grep -qiE 'forwarding failed|Address already in use|cannot listen' || fail "(h) the tunnel did not report the failed forward"
pass "squat (h): a squatted port keeps the tunnel from starting (ExitOnForwardFailure): a visible failure, not a silent one"
systemctl stop devotee-db-tunnel.service; unsquat
systemctl start devotee-db-tunnel.service
wait_for 25 listening 5544 || fail "the tunnel did not recover after the squatter left"
ok "recovered" reaches root 5544 55432

# -------------------- 11. ip_local_reserved_ports does not stop an explicit bind

OLD_RES="$(sysctl -n net.ipv4.ip_local_reserved_ports)"
sysctl -qw net.ipv4.ip_local_reserved_ports=5546
cat > $T/bind.py <<'EOF'
import socket, sys
s = socket.socket(); s.bind(("127.0.0.1", int(sys.argv[1]))); s.listen(1)
print("bound")
EOF
chmod 0644 $T/bind.py; chmod 0755 $T
[[ "$(as_user ik-agent timeout 5 python3 $T/bind.py 5546)" == bound ]] || fail "ik-agent could NOT bind a port in ip_local_reserved_ports: the sysctl would be a mitigation after all"
sysctl -qw net.ipv4.ip_local_reserved_ports="$OLD_RES"
pass "net.ipv4.ip_local_reserved_ports=5546 does not stop an explicit bind to 5546 by an unprivileged account (it only steers ephemeral ports), so it is not used"

echo "==> [Container/before] BEFORE-REBOOT CHECKS PASSED ($CHECKS assertions)"
exit 0
fi

# =============================================================== phase: after

# The container was stopped and started: /run, /tmp and the whole ruleset are
# gone; /etc, the units and their enablement are not. Nobody has run the
# provisioner since. The lock must be there because its unit loaded it.
ok "the lock unit is enabled" systemctl is-enabled --quiet $LOCKUNIT
[[ "$(unit_state $LOCKUNIT)" == active ]] || fail "the lock unit is not active after boot"
ok "the table was loaded at boot" lock_rules
[[ "$(lock_rules | grep -c dport)" == 4 ]] || fail "expected 4 port rules after boot: $(lock_rules)"
BUID="$(id -u $BU)"
[[ "$(lock_uidset)" == "0,$BUID" ]] || fail "wrong uid set after boot"
ufw status | grep -q '^Status: active' || fail "ufw is not active after boot"
nft list tables | grep -qxF "table inet $TABLE" || fail "our table is missing after boot"
# Ordering: the lock came up before the tunnels' ssh was even started.
# A container shares the host's boot id, so `journalctl -b` cannot tell this boot from the
# one before: keep only entries stamped after THIS systemd instance started.
US="$(systemctl show -p UserspaceTimestampMonotonic --value)"
J="$(journalctl -o short-monotonic --no-pager -u devotee-db-tunnel.service 2>/dev/null \
     | awk -v us="$US" '{t=$0; sub(/^\[ */,"",t); sub(/\].*/,"",t); if (t * 1000000 >= us) print}')"
LT="$(systemctl show -p ActiveEnterTimestampMonotonic --value $LOCKUNIT | awk '{printf "%.6f", $1 / 1000000}')"   # (the lock's own log line can predate journald)
TT="$(grep -m1 -E 'Started devotee-db-tunnel.service' <<<"$J" | sed -E 's/^\[ *([0-9.]+)\].*/\1/' || true)"
[[ -n "$LT" && -n "$TT" ]] || { echo "$J" >&2; fail "could not read the boot order from the journal (lock='$LT', tunnel='$TT')"; }
awk -v l="$LT" -v t="$TT" 'BEGIN{exit !(l+0 <= t+0)}' || fail "the tunnel started at $TT s, before the lock was loaded at $LT s"
echo "    boot order (journal, monotonic): lock loaded at $LT s, devotee-db tunnel started at $TT s"
start_fixture
systemctl restart devotee-db-tunnel.service alt-db-tunnel.service
wait_for 30 listening 5544 || { journalctl -u devotee-db-tunnel.service --no-pager | tail -10 >&2; fail "devotee-db did not come up after boot"; }
wait_for 30 listening 5545 || fail "alt-db did not come up after boot"
check_matrix "$BU"
pass "reboot: the lock unit loaded the table before the tunnels started; the matrix holds with ufw active"

HKEY="$(cut -d' ' -f1,2 "$REMOTE/ssh_host_ed25519_key.pub")"
KHP="TUNNEL_KNOWN_HOSTS=(\"127.0.0.1 $HKEY\" \"[127.0.0.1]:2222 $HKEY\")"
std_profile
prov
{ [[ $RC -eq 0 ]] && out_has 'no changes: the host already matches this profile'; } || fail "after boot a rerun should find nothing to do"
ufw --force reset >/dev/null 2>&1
check_matrix "$BU"
pass "after boot: a rerun reports no changes; a ufw reset still leaves the lock in force"

# --------------------------------------------------- everything removed

profile "TUNNELS=()"
prov --dry-run
{ [[ $RC -eq 0 ]] && out_has 'would: unit ikenga-tunnel-lock.service stopped, disabled and removed' && lock_rules >/dev/null; } || fail "dry run of TUNNELS=() should plan the lock removal and change nothing"
prov
{ [[ $RC -eq 0 ]] && out_has 'unit ikenga-tunnel-lock.service stopped, disabled and removed'; } || fail "TUNNELS=() should remove the lock"
no "table gone" lock_rules
[[ ! -e $LOCKFILE && ! -e $UNITS/$LOCKUNIT ]] || fail "lock file/unit left behind"
compgen -G "$UNITS/*-tunnel.service.d/*" >/dev/null && fail "a drop-in was left behind"
compgen -G "$UNITS/*-tunnel.service" >/dev/null && fail "a tunnel unit was left behind"
if systemctl is-enabled --quiet $LOCKUNIT 2>/dev/null; then fail "the lock unit is still enabled"; fi
[[ -f $KEY ]] || fail "the tunnel key must be kept"
prov; { [[ $RC -eq 0 ]] && out_has 'nothing to do'; } || fail "a second TUNNELS=() run should have nothing to do"
pass "TUNNELS=(): the lock table, file, unit and drop-ins are removed (the key is kept); a rerun has nothing to do"

echo "==> [Container/after] ALL PORT-LOCK CHECKS PASSED ($CHECKS assertions in this phase)"
