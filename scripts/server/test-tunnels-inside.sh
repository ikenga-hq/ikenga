#!/bin/bash
# provision.sh tunnels, run as root inside the privileged systemd container of
# test-tunnels-container.sh. See that file for what is real and what is faked.
set -euo pipefail

T="$(mktemp -d)"
PROVISION=/work/provision.sh
PROV=/root/prov
PROFILE=$PROV/profile.env
OUT=$T/out
ALL=$T/all-output
: > "$ALL"
TU=ikenga-tunnel
THOME=/var/lib/ikenga-tunnel
SSHDIR=$THOME/.ssh
KEY=$SSHDIR/id_ed25519
KH=$SSHDIR/known_hosts
UNITS=/etc/systemd/system
REMOTE=/etc/ssh-remote
T1='devotee-db=pgtunnel@127.0.0.1 5544:127.0.0.1:55432'
T2='alt-db=pgtunnel2@127.0.0.1:2222 5545:127.0.0.1:55433'
CHECKS=0

fail() { echo "FAIL: $*" >&2; [[ -f "$OUT" ]] && { echo "--- last provision output ---" >&2; tail -40 "$OUT" >&2; }; exit 1; }
pass() { echo "==> [Container] $* PASSED"; }
ok() { local d="$1"; shift; CHECKS=$((CHECKS + 1)); "$@" || fail "$d"; }

as_tun() { ( cd / && setpriv --reuid="$(id -u $TU)" --regid="$(id -g $TU)" --clear-groups env -i HOME=/nonexistent PATH=/usr/local/bin:/usr/bin:/bin "$@" ); }
RC=0
prov() {   # [args]: runs `tunnels` with the current profile; RC and $OUT
  if "$PROVISION" tunnels --profile "$PROFILE" "$@" > "$OUT" 2>&1; then RC=0; else RC=$?; fi
  cat "$OUT" >> "$ALL"
}
profile() {   # lines...
  printf '%s\n' "$@" > "$PROFILE"; chown root:root "$PROFILE"; chmod 0600 "$PROFILE"
}
out_has() { CHECKS=$((CHECKS + 1)); grep -qE -- "$1" "$OUT"; }
unit_state() { systemctl is-active "$1" 2>/dev/null || true; }
wait_for() {   # tries cmd...
  local n="$1" i; shift
  for i in $(seq 1 "$n"); do "$@" && return 0; sleep 1; done
  return 1
}
# Reads the banner of whatever answers on 127.0.0.1:<port> and checks the echo.
reach() {   # port, expected fake-postgres port
  ( exec 9<>"/dev/tcp/127.0.0.1/$1" 2>/dev/null || exit 1
    local b r
    read -t 5 -u 9 b || exit 1
    printf 'ping-%s\n' "$1" >&9
    read -t 5 -u 9 r || exit 1
    [[ "$b" == "fake-postgres-$2" && "$r" == "ping-$1" ]] )
}
# Nobody answers on the port (or the connection dies without a banner).
silent() {   # port
  ( exec 9<>"/dev/tcp/127.0.0.1/$1" 2>/dev/null || exit 0
    local b
    if read -t 3 -u 9 b; then exit 1; else exit 0; fi )
}
listening() { ss -ltn | grep -qE "127\.0\.0\.1:$1\s"; }
not_listening() { ! listening "$1"; }
fp() { as_tun ssh-keygen -l -f "$KEY.pub" | cut -d' ' -f2; }
nothing_created() {
  ! id "$TU" >/dev/null 2>&1 && [[ ! -e $THOME ]] && ! compgen -G "$UNITS/*-tunnel.service" >/dev/null && [[ ! -e /tmp/pwned ]]
}

# ------------------------------------------------------------------- host

# The fake remote: sshd on 127.0.0.1:22 and :2222 (own config, own host key),
# nologin users, and two "postgres" listeners.
systemctl mask ssh.service ssh.socket >/dev/null 2>&1 || true
mkdir -p "$REMOTE" "$PROV"; chmod 0755 "$REMOTE"
ssh-keygen -q -t ed25519 -N '' -f "$REMOTE/ssh_host_ed25519_key"
cp "$REMOTE/ssh_host_ed25519_key" "$T/hostkey.orig"
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
for u in pgtunnel pgtunnel2; do
  useradd --system --no-create-home --shell /usr/sbin/nologin "$u"
  usermod -p '*' "$u"
done
cat > /etc/systemd/system/fake-remote-sshd.service <<'EOF'
[Service]
RuntimeDirectory=sshd
ExecStart=/usr/sbin/sshd -D -e -f /etc/ssh-remote/sshd_config
Restart=on-failure
EOF
systemctl daemon-reload
systemctl start fake-remote-sshd
for p in 55432 55433; do
  systemd-run --quiet --unit="fake-pg-$p" --collect socat "TCP-LISTEN:$p,bind=127.0.0.1,fork,reuseaddr" "SYSTEM:echo fake-postgres-$p; cat" >/dev/null
done
wait_for 10 listening 55432 || fail "fake postgres did not start"
wait_for 10 listening 2222 || fail "fake remote sshd did not start"

HKEY="$(cut -d' ' -f1,2 "$REMOTE/ssh_host_ed25519_key.pub")"
KHP="TUNNEL_KNOWN_HOSTS=(\"127.0.0.1 $HKEY\" \"[127.0.0.1]:2222 $HKEY\")"
std_profile() { profile "TUNNEL_FROM=127.0.0.1" "$KHP" "TUNNELS=(\"$T1\" \"$T2\")"; }

# An Ikenga account (uid in the account range), an admin and the backup user, for the clash checks.
groupadd -g 20001 ik-ada; useradd -u 20001 -g 20001 -M -d /nonexistent -s /bin/sh ik-ada
groupadd -g 20050 tunsvc; useradd -u 20050 -g 20050 -M -d /nonexistent -s /usr/sbin/nologin tunsvc
useradd -m ops
useradd --system --no-create-home --shell /usr/sbin/nologin ikenga-backup

# ------------------------------------------------- 1. strict profile validation

refuse() {   # description, regexp for the message, profile lines...
  local d="$1" re="$2"; shift 2
  profile "$@"
  prov
  [[ $RC -ne 0 ]] || fail "$d: provisioning succeeded"
  grep -qE -- "$re" "$OUT" || fail "$d: refused, but not with /$re/"
  nothing_created || fail "$d: state was created before the refusal"
  CHECKS=$((CHECKS + 1))
}
refuse_t() {   # description, tunnel entry
  refuse "TUNNELS $1" 'TUNNELS entry 1|TUNNELS names|local port' "TUNNEL_FROM=127.0.0.1" "$KHP" "TUNNELS=($(printf '%q' "$2"))"
}
refuse_kh() {   # description, known-hosts entry
  refuse "KNOWN_HOSTS $1" 'TUNNEL_KNOWN_HOSTS' "TUNNEL_FROM=127.0.0.1" "TUNNEL_KNOWN_HOSTS=($(printf '%q' "$2"))" "TUNNELS=(\"$T1\")"
}
G='pgtunnel@127.0.0.1 5544:127.0.0.1:55432'
refuse_t 'uppercase name' "Bad=$G"
refuse_t 'name with ;' "a;touch /tmp/pwned=$G"
refuse_t 'name with $()' 'a$(touch /tmp/pwned)=pgtunnel@127.0.0.1 5544:127.0.0.1:55432'
refuse_t 'name with a space' "a b=$G"
refuse_t 'empty name' "=$G"
refuse_t 'host starting with -' 'x=pgtunnel@-oProxyCommand=touch 5544:127.0.0.1:55432'
refuse_t 'host with ;' 'x=pgtunnel@127.0.0.1;touch 5544:127.0.0.1:55432'
refuse_t 'user starting with -' 'x=-oProxyCommand=id@127.0.0.1 5544:127.0.0.1:55432'
refuse_t 'user with a space' 'x=pg tunnel@127.0.0.1 5544:127.0.0.1:55432'
refuse_t 'remote host with ;' 'x=pgtunnel@127.0.0.1 5544:127.0.0.1;touch:55432'
refuse_t 'extra ssh option after the spec' "x=$G -oProxyCommand=id"
refuse_t 'a newline and a second unit line' "x=$G"$'\n'"ExecStartPre=/bin/touch /tmp/pwned"
refuse_t 'two spaces' 'x=pgtunnel@127.0.0.1  5544:127.0.0.1:55432'
refuse_t 'a tab' $'x=pgtunnel@127.0.0.1\t5544:127.0.0.1:55432'
refuse_t 'a bind address in the local part' 'x=pgtunnel@127.0.0.1 0.0.0.0:5544:127.0.0.1:55432'
refuse_t 'local port 0' 'x=pgtunnel@127.0.0.1 0:127.0.0.1:55432'
refuse_t 'local port 65536' 'x=pgtunnel@127.0.0.1 65536:127.0.0.1:55432'
refuse_t 'remote port 99999' 'x=pgtunnel@127.0.0.1 5544:127.0.0.1:99999'
refuse_t 'remote port 0' 'x=pgtunnel@127.0.0.1 5544:127.0.0.1:0'
refuse_t 'port with a letter' 'x=pgtunnel@127.0.0.1 5544:127.0.0.1:55x32'
refuse_t 'leading-zero port' 'x=pgtunnel@127.0.0.1 05544:127.0.0.1:55432'
refuse_t 'ssh port 0' 'x=pgtunnel@127.0.0.1:0 5544:127.0.0.1:55432'
refuse_t 'ssh port 70000' 'x=pgtunnel@127.0.0.1:70000 5544:127.0.0.1:55432'
refuse_t 'missing forward' 'x=pgtunnel@127.0.0.1'
refuse "duplicate names" 'TUNNELS names' "TUNNEL_FROM=127.0.0.1" "$KHP" "TUNNELS=(\"$T1\" \"devotee-db=pgtunnel2@127.0.0.1 5599:127.0.0.1:55433\")"
refuse "duplicate local ports" 'local port 5544' "TUNNEL_FROM=127.0.0.1" "$KHP" "TUNNELS=(\"$T1\" \"other=pgtunnel2@127.0.0.1 5544:127.0.0.1:55433\")"
refuse "a tunnel whose host has no pinned key" 'has no key for it' "TUNNEL_FROM=127.0.0.1" "TUNNEL_KNOWN_HOSTS=(\"127.0.0.2 $HKEY\")" "TUNNELS=(\"$T1\")"
refuse "ssh port 2222 pinned only as a bare host" 'has no key for it' "TUNNEL_FROM=127.0.0.1" "TUNNEL_KNOWN_HOSTS=(\"127.0.0.1 $HKEY\")" "TUNNELS=(\"$T2\")"
refuse "no known hosts at all (no TOFU)" 'has no key for it' "TUNNEL_FROM=127.0.0.1" "TUNNELS=(\"$T1\")"
refuse_kh 'trailing comment field' "127.0.0.1 $HKEY comment"
refuse_kh 'a marker' "@cert-authority 127.0.0.1 $HKEY"
refuse_kh 'a wildcard host' "* $HKEY"
refuse_kh 'an option prefix' "command=\"id\" 127.0.0.1 $HKEY"
refuse_kh 'a newline and a second line' "127.0.0.1 $HKEY"$'\n'"evil.example ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGFAKEFAKEFAKEFAKEFAKEFAKEFAKEFAKEFAKEFAKEFA"
refuse_kh 'a disallowed key type' "127.0.0.1 ssh-dss ${HKEY#* }"
refuse_kh 'not base64' "127.0.0.1 ssh-ed25519 not*base64*at*all*here!"
refuse "KNOWN_HOSTS valid-looking garbage key" 'not a valid SSH public key' "TUNNEL_FROM=127.0.0.1" "TUNNEL_KNOWN_HOSTS=(\"127.0.0.1 ssh-ed25519 AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\")" "TUNNELS=(\"$T1\")"
refuse "KNOWN_HOSTS key of another type than its label" 'not a valid SSH public key|is not a ssh-rsa key' "TUNNEL_FROM=127.0.0.1" "TUNNEL_KNOWN_HOSTS=(\"127.0.0.1 ssh-rsa ${HKEY#* }\")" "TUNNELS=(\"$T1\")"
refuse "TUNNEL_FROM with a quote" 'TUNNEL_FROM must be' "TUNNEL_FROM='127.0.0.1\" command=\"id'" "$KHP" "TUNNELS=(\"$T1\")"
refuse "TUNNEL_USER root" 'TUNNEL_USER' "TUNNEL_USER=root" "$KHP" "TUNNELS=(\"$T1\")"
refuse "TUNNEL_USER an ik- account" 'TUNNEL_USER' "TUNNEL_USER=ik-ada" "$KHP" "TUNNELS=(\"$T1\")"
refuse "TUNNEL_USER the admin" 'TUNNEL_USER' "ADMIN_USER=ops" "TUNNEL_USER=ops" "$KHP" "TUNNELS=(\"$T1\")"
refuse "TUNNEL_USER the backup user" 'TUNNEL_USER' "TUNNEL_USER=ikenga-backup" "$KHP" "TUNNELS=(\"$T1\")"
refuse "TUNNEL_USER with a bad name" 'TUNNEL_USER' "TUNNEL_USER='x;id'" "$KHP" "TUNNELS=(\"$T1\")"
# tunsvc has uid 20050, inside UID_RANGE: refused when it would be adopted as the tunnel user.
profile "TUNNEL_USER=tunsvc" "TUNNEL_FROM=127.0.0.1" "$KHP" "TUNNELS=(\"$T1\")"; prov
[[ $RC -ne 0 ]] && out_has 'inside the Ikenga account range' || fail "TUNNEL_USER with a uid in UID_RANGE was accepted"
[[ ! -e /var/lib/tunsvc ]] && ! compgen -G "$UNITS/*-tunnel.service" >/dev/null || fail "state created for the clashing user"
profile "TUNNELS='not an array'"; prov
[[ $RC -ne 0 ]] && out_has 'TUNNELS must be a bash array' || fail "TUNNELS as a scalar was accepted"
pass "profile validation: injection, bad ports and names, unpinned hosts, clashing users"

# --------------------------------------------- 2. no TUNNELS in the profile: no-op

profile "TUNNEL_FROM=127.0.0.1" "$KHP"
prov
[[ $RC -eq 0 ]] && out_has 'does not define TUNNELS' || fail "a profile without TUNNELS should be a no-op"
nothing_created || fail "a profile without TUNNELS created something"
profile "TUNNELS=()"; prov
[[ $RC -eq 0 ]] && out_has 'TUNNELS is empty' || fail "TUNNELS=() on a fresh host should say nothing to do"
nothing_created || fail "TUNNELS=() created something"
pass "no TUNNELS / empty TUNNELS on a fresh host does nothing"

# ---------------------------------------------------------- 3. dry run, fresh

std_profile
prov --dry-run
[[ $RC -eq 0 ]] || fail "dry run failed"
out_has 'would: system user ikenga-tunnel created' || fail "dry run does not plan the user"
out_has 'would: .*id_ed25519 generated' || fail "dry run does not plan the key"
out_has 'would: unit devotee-db-tunnel.service installed' || fail "dry run does not plan the unit"
out_has 'would: unit alt-db-tunnel.service installed' || fail "dry run does not plan the second unit"
out_has 'permitopen="127.0.0.1:55432",from="127.0.0.1" <public key' || fail "dry run: authorized_keys line missing"
nothing_created || fail "the dry run changed the host"
profile "$KHP" "TUNNELS=(\"$T1\")"; prov --dry-run
{ out_has 'from="<THIS-BOX-PUBLIC-IP>"' && out_has 'public address could not be determined'; } || fail "no TUNNEL_FROM and no public address: the placeholder is missing"
pass "dry run plans everything and changes nothing"

# ----------------------------------------------------------- 4. fresh converge

std_profile
prov
[[ $RC -eq 0 ]] || fail "fresh provisioning failed"
ok "system user" id "$TU"
[[ "$(getent passwd $TU | cut -d: -f6,7)" == "$THOME:/usr/sbin/nologin" ]] || fail "home or shell wrong: $(getent passwd $TU)"
[[ "$(id -nG $TU)" == "$TU" ]] || fail "supplementary groups: $(id -nG $TU)"
[[ "$(id -u $TU)" -lt 1000 ]] || fail "not a system uid"
[[ "$(stat -c '%a %U %G' $THOME)" == "700 $TU $TU" ]] || fail "home: $(stat -c '%a %U %G' $THOME)"
[[ "$(stat -c '%a %U %G' $SSHDIR)" == "700 $TU $TU" ]] || fail ".ssh: $(stat -c '%a %U %G' $SSHDIR)"
[[ "$(stat -c '%a %U' $KEY)" == "600 $TU" ]] || fail "key mode/owner: $(stat -c '%a %U' $KEY)"
[[ "$(stat -c '%a %U' $KEY.pub)" == "644 $TU" ]] || fail "pub mode/owner"
[[ "$(cut -d' ' -f3 $KEY.pub)" == "$TU@"* ]] || fail "key comment: $(cut -d' ' -f3 $KEY.pub)"
[[ "$(cut -d' ' -f1 $KEY.pub)" == ssh-ed25519 ]] || fail "not ed25519"
[[ "$(stat -c '%a %U' $KH)" == "644 $TU" ]] || fail "known_hosts mode/owner"
[[ "$(sort $KH)" == "$(printf '%s\n' "127.0.0.1 $HKEY" "[127.0.0.1]:2222 $HKEY" | sort)" ]] || fail "known_hosts content: $(cat $KH)"
FP1="$(fp)"
for u in devotee-db alt-db; do
  f="$UNITS/$u-tunnel.service"
  [[ "$(stat -c '%a %u %g' $f)" == "644 0 0" ]] || fail "$f mode"
  for want in "User=$TU" "Group=$TU" 'NoNewPrivileges=yes' 'ProtectSystem=strict' 'ProtectHome=yes' 'PrivateTmp=yes' "ReadOnlyPaths=$THOME" \
              'Restart=always' 'RestartSec=10' 'WantedBy=multi-user.target' 'StrictHostKeyChecking=yes' 'IdentitiesOnly=yes' 'BatchMode=yes' \
              'ExitOnForwardFailure=yes' 'ServerAliveInterval=30' 'ServerAliveCountMax=3' "-i $KEY" "UserKnownHostsFile=$KH"; do
    grep -qF -- "$want" "$f" || fail "$f lacks: $want"
  done
  systemctl is-enabled --quiet "$u-tunnel.service" || fail "$u not enabled"
  if ! systemctl is-active --quiet "$u-tunnel.service" && [[ "$(unit_state $u-tunnel.service)" != activating ]]; then fail "$u not running"; fi
done
grep -qF -- '-L 127.0.0.1:5544:127.0.0.1:55432' $UNITS/devotee-db-tunnel.service || fail "forward spec (devotee-db)"
grep -qF -- 'pgtunnel@127.0.0.1' $UNITS/devotee-db-tunnel.service || fail "target (devotee-db)"
if grep -qE -- ' -p ' $UNITS/devotee-db-tunnel.service; then fail "ssh port 22 should add no -p"; fi
grep -qF -- '-p 2222' $UNITS/alt-db-tunnel.service || fail "ssh port 2222 missing in alt-db"
grep -qF -- '-L 127.0.0.1:5545:127.0.0.1:55433' $UNITS/alt-db-tunnel.service || fail "forward spec (alt-db)"
{ out_has 'unit devotee-db-tunnel.service installed' && out_has 'unit alt-db-tunnel.service installed'; } || fail "summary should list the changed units by name"
# The exact authorized_keys lines, with the real public key.
PUB="$(cut -d' ' -f1,2 $KEY.pub)"
L1="$(grep -F 'permitopen="127.0.0.1:55432"' "$OUT" | sed 's/^ *//')"
L2="$(grep -F 'permitopen="127.0.0.1:55433"' "$OUT" | sed 's/^ *//')"
[[ "$L1" == "restrict,port-forwarding,permitopen=\"127.0.0.1:55432\",from=\"127.0.0.1\" $PUB "* ]] || fail "authorized_keys line (devotee-db): $L1"
[[ "$L2" == "restrict,port-forwarding,permitopen=\"127.0.0.1:55433\",from=\"127.0.0.1\" $PUB "* ]] || fail "authorized_keys line (alt-db): $L2"
out_has 'not listening yet' || fail "an unauthorised tunnel should say so"
# The private key is never printed.
KEYBODY="$(sed -n '2p' $KEY)"
if grep -qF -- "$KEYBODY" "$ALL" || grep -q 'BEGIN OPENSSH PRIVATE KEY' "$ALL"; then fail "the private key reached the provisioner output"; fi
pass "fresh converge: user, key, known_hosts, units, authorized_keys lines"

# ------------------------------------- 5. the remote end authorises, it connects

printf '%s\n' "$L1" > $REMOTE/authorized_keys.pgtunnel
printf '%s\n' "$L2" > $REMOTE/authorized_keys.pgtunnel2
chmod 0644 $REMOTE/authorized_keys.*
systemctl restart devotee-db-tunnel.service alt-db-tunnel.service
wait_for 20 listening 5544 || { journalctl -u devotee-db-tunnel.service --no-pager | tail -15 >&2; fail "tunnel devotee-db did not come up"; }
wait_for 20 listening 5545 || fail "tunnel alt-db did not come up"
ok "127.0.0.1:5544 reaches the fake postgres on 55432" reach 5544 55432
ok "127.0.0.1:5545 (ssh port 2222) reaches 55433" reach 5545 55433
if ss -ltn | grep -E ':(5544|5545)\s' | grep -vqE '127\.0\.0\.1:(5544|5545)\s'; then fail "a tunnel listens on more than 127.0.0.1"; fi
pass "tunnels connect through the restricted authorized_keys line"

# --------------------------------------------------------- 6. rerun = no changes

INV1="$(systemctl show -p InvocationID --value devotee-db-tunnel.service)"
INV2="$(systemctl show -p InvocationID --value alt-db-tunnel.service)"
prov
{ [[ $RC -eq 0 ]] && out_has 'no changes: the host already matches this profile'; } || fail "rerun should report no changes"
out_has 'is listening' || fail "rerun should say the tunnels are up"
[[ "$(systemctl show -p InvocationID --value devotee-db-tunnel.service)" == "$INV1" ]] || fail "rerun restarted devotee-db"
[[ "$(systemctl show -p InvocationID --value alt-db-tunnel.service)" == "$INV2" ]] || fail "rerun restarted alt-db"
[[ "$(fp)" == "$FP1" ]] || fail "the key changed on a rerun"
prov --dry-run
out_has 'no changes' || fail "dry run of a converged host should say no changes"
pass "rerun reports no changes, restarts nothing, keeps the key"

# --------------------------------------------- 7. what the key may NOT do (sshd)

SSHO=(-i "$KEY" -o "UserKnownHostsFile=$KH" -o StrictHostKeyChecking=yes -o BatchMode=yes -o IdentitiesOnly=yes -o ConnectTimeout=5)
set +e; as_tun ssh "${SSHO[@]}" pgtunnel@127.0.0.1 id > "$T/shell.out" 2>&1; SRC=$?; set -e
if [[ $SRC -eq 0 ]] || grep -q 'uid=' "$T/shell.out"; then fail "a shell via the tunnel key was NOT refused: $(cat "$T/shell.out")"; fi
set +e; as_tun ssh "${SSHO[@]}" -tt pgtunnel@127.0.0.1 > "$T/tty.out" 2>&1 < /dev/null; set -e
if grep -q 'uid=' "$T/tty.out"; then fail "a tty session was allowed"; fi
# Positive control: the permitted forward works with this very command line; the
# one to another port is refused by permitopen.
( as_tun timeout 14 ssh "${SSHO[@]}" -N -L 127.0.0.1:6000:127.0.0.1:55432 pgtunnel@127.0.0.1 2> "$T/fwd-ok.log" & )
( as_tun timeout 14 ssh "${SSHO[@]}" -v -N -L 127.0.0.1:6001:127.0.0.1:55433 pgtunnel@127.0.0.1 2> "$T/fwd-bad.log" & )
{ wait_for 8 listening 6000 && wait_for 8 listening 6001; } || fail "the manual forwards did not start"
ok "control: the permitted forward (55432) works" reach 6000 55432
ok "a forward to another port (55433) is refused" silent 6001
wait_for 5 grep -q 'administratively prohibited' "$T/fwd-bad.log" || fail "sshd did not log a permitopen refusal"
pass "the key is restricted: no shell, no tty, one forward target"

# -------------------------------------------------- 8. host key mismatch refuses

KH_BEFORE="$(cat $KH)"
systemctl stop fake-remote-sshd
ssh-keygen -q -t ed25519 -N '' -f "$T/otherhostkey"
cp "$T/otherhostkey" $REMOTE/ssh_host_ed25519_key; chmod 600 $REMOTE/ssh_host_ed25519_key
systemctl start fake-remote-sshd
wait_for 10 listening 2222 || true
systemctl restart devotee-db-tunnel.service
wait_for 5 not_listening 5544 || fail "the tunnel stayed up after the remote host key changed"
sleep 3
not_listening 5544 || fail "the tunnel connected to a host with another key"
journalctl -u devotee-db-tunnel.service --no-pager | grep -qE 'Host key verification failed|REMOTE HOST IDENTIFICATION' || fail "no host-key refusal in the journal"
[[ "$(cat $KH)" == "$KH_BEFORE" ]] || fail "ssh learned or changed a host key (known_hosts differs)"
# Back to the real key: the tunnel recovers on its own.
systemctl stop fake-remote-sshd
cp "$T/hostkey.orig" $REMOTE/ssh_host_ed25519_key; chmod 600 $REMOTE/ssh_host_ed25519_key
systemctl start fake-remote-sshd
wait_for 20 listening 5544 || fail "the tunnel did not recover once the right host key was back"
ok "recovered" reach 5544 55432
# A wrong pin in the profile: known_hosts is rewritten (the old one kept aside)
# and the tunnel restarts and refuses.
ssh-keygen -q -t ed25519 -N '' -f "$T/bogus"; BOGUS="$(cut -d' ' -f1,2 $T/bogus.pub)"
profile "TUNNEL_FROM=127.0.0.1" "TUNNEL_KNOWN_HOSTS=(\"127.0.0.1 $BOGUS\" \"[127.0.0.1]:2222 $HKEY\")" "TUNNELS=(\"$T1\" \"$T2\")"
prov
{ [[ $RC -eq 0 ]] && out_has 'known_hosts written' && out_has 'unit devotee-db-tunnel.service restarted \(pinned host keys changed\)'; } || fail "a changed pin should rewrite known_hosts and restart"
compgen -G "$SSHDIR/known_hosts.bak-*" >/dev/null || fail "the old known_hosts was not kept"
sleep 3
not_listening 5544 || fail "the tunnel connected with a wrong pinned key"
std_profile; prov
wait_for 15 listening 5544 || fail "the tunnel did not come back after the pin was restored"
ok "restored" reach 5544 55432
ok "alt-db (its pin never changed) is fine" reach 5545 55433
pass "a host key that is not the pinned one is refused (remote key swap and wrong pin)"

# ---------------------------------------------------- 9. symlinks, owners, modes

assert_refused() {   # description
  [[ $RC -ne 0 ]] || fail "$1: provisioning succeeded"
}
std_profile
# (a) the key is a symlink
as_tun mv "$KEY" "$KEY.real"; as_tun ln -s "$KEY.real" "$KEY"
prov; assert_refused "key symlink"; out_has 'symbolic link' || fail "key symlink: message"
as_tun rm "$KEY"; as_tun mv "$KEY.real" "$KEY"
# (b) known_hosts is a symlink to a root file; the profile would change it
echo SENSITIVE > /root/sensitive; chmod 0600 /root/sensitive
as_tun mv "$KH" "$KH.real"; as_tun ln -s /root/sensitive "$KH"
profile "TUNNEL_FROM=127.0.0.1" "TUNNEL_KNOWN_HOSTS=(\"127.0.0.1 $HKEY\" \"[127.0.0.1]:2222 $HKEY\" \"127.0.0.9 $HKEY\")" "TUNNELS=(\"$T1\" \"$T2\" \"x=pgtunnel@127.0.0.9 5599:127.0.0.1:55432\")"
prov; assert_refused "known_hosts symlink"; out_has 'symbolic link' || fail "known_hosts symlink: message"
[[ "$(cat /root/sensitive)" == SENSITIVE && "$(stat -c %a /root/sensitive)" == 600 ]] || fail "root wrote through the known_hosts symlink"
[[ ! -e "$UNITS/x-tunnel.service" ]] || fail "a unit was written before the symlink refusal"
as_tun rm "$KH"; as_tun mv "$KH.real" "$KH"; std_profile
# (c) .ssh is a symlink
as_tun mv "$SSHDIR" "$THOME/ssh.real"; as_tun ln -s ssh.real "$SSHDIR"
prov; assert_refused ".ssh symlink"; out_has 'symbolic link' || fail ".ssh symlink: message"
as_tun rm "$SSHDIR"; as_tun mv "$THOME/ssh.real" "$SSHDIR"
# (d) the home is a symlink; the target must not be chown/chmod-ed
mkdir -p /var/lib/it.real; chmod 0755 /var/lib/it.real
mv "$THOME" /var/lib/it.keep; ln -s it.real "$THOME"
prov; assert_refused "home symlink"; out_has 'symbolic link' || fail "home symlink: message"
[[ "$(stat -c '%a %U' /var/lib/it.real)" == "755 root" ]] || fail "root chowned/chmodded through the home symlink"
unlink /var/lib/ikenga-tunnel; mv /var/lib/it.keep "$THOME"; rmdir /var/lib/it.real
# (e) a key that belongs to someone else
chown root:root "$KEY"
prov; assert_refused "key owned by root"; out_has 'owned by uid 0' || fail "key owner: message"
chown "$TU:$TU" "$KEY"
# (f) a key readable by others: mode fixed, key kept
chmod 0640 "$KEY"
prov
{ [[ $RC -eq 0 ]] && out_has 'id_ed25519: mode set to 0600' && [[ "$(stat -c %a $KEY)" == 600 && "$(fp)" == "$FP1" ]]; } || fail "a 0640 key should be fixed to 0600 and kept"
# (g) the unit is a symlink
cp -p "$UNITS/alt-db-tunnel.service" "$T/alt.unit"; unlink "$UNITS/alt-db-tunnel.service"; ln -s "$T/alt.unit" "$UNITS/alt-db-tunnel.service"
prov; assert_refused "unit symlink"; out_has 'symbolic link' || fail "unit symlink: message"
unlink "$UNITS/alt-db-tunnel.service"; cp -p "$T/alt.unit" "$UNITS/alt-db-tunnel.service"
prov; { [[ $RC -eq 0 ]] && out_has 'no changes'; } || fail "state not clean after the symlink checks"
[[ "$(fp)" == "$FP1" ]] || fail "key changed during the symlink checks"
pass "symlinks (key, known_hosts, .ssh, home, unit) and a foreign-owned key are refused, nothing written through them"

# ------------------------------------------------------------------ 10. removal

profile "TUNNEL_FROM=127.0.0.1" "$KHP" "TUNNELS=(\"$T1\")"
prov
{ [[ $RC -eq 0 ]] && out_has 'unit alt-db-tunnel.service stopped, disabled and removed'; } || fail "removing a tunnel should say so"
[[ ! -e "$UNITS/alt-db-tunnel.service" ]] || fail "unit file not removed"
if systemctl is-enabled --quiet alt-db-tunnel.service 2>/dev/null; then fail "unit still enabled"; fi
[[ "$(unit_state alt-db-tunnel.service)" != active ]] || fail "unit still active"
wait_for 5 not_listening 5545 || fail "port 5545 still open"
[[ "$(fp)" == "$FP1" ]] || fail "the key must be kept on removal"
{ id "$TU" >/dev/null && [[ -f "$KEY" ]]; } || fail "user/key must be kept on removal"
[[ -e "$UNITS/devotee-db-tunnel.service" ]] || fail "the other tunnel's unit was removed"
wait_for 15 listening 5544 || fail "the remaining tunnel is down"
# A hand-made unit of the tunnel user is left alone when the profile never mentions TUNNELS.
cp "$UNITS/devotee-db-tunnel.service" "$UNITS/legacy-tunnel.service"; systemctl daemon-reload
profile "TUNNEL_FROM=127.0.0.1" "$KHP"; prov
[[ $RC -eq 0 && -e "$UNITS/legacy-tunnel.service" && -e "$UNITS/devotee-db-tunnel.service" ]] || fail "a profile without TUNNELS must leave tunnel units alone"
# TUNNELS=() removes every unit of the tunnel user (an unrelated unit is not touched).
printf '[Service]\nUser=nobody\nExecStart=/bin/true\n' > "$UNITS/unrelated-tunnel.service"
profile "TUNNELS=()"; prov --dry-run
{ out_has 'would: unit devotee-db-tunnel.service stopped' && out_has 'would: unit legacy-tunnel.service stopped' && [[ -e "$UNITS/legacy-tunnel.service" ]]; } || fail "dry run of TUNNELS=() should plan the removals only"
prov
[[ $RC -eq 0 ]] || fail "TUNNELS=() failed"
[[ ! -e "$UNITS/devotee-db-tunnel.service" && ! -e "$UNITS/legacy-tunnel.service" ]] || fail "TUNNELS=() left units behind"
[[ -e "$UNITS/unrelated-tunnel.service" ]] || fail "TUNNELS=() removed a unit of another user"
wait_for 5 not_listening 5544 || fail "port 5544 still open"
[[ "$(fp)" == "$FP1" ]] || fail "the key must survive removing every tunnel"
unlink "$UNITS/unrelated-tunnel.service"
pass "removal: unit stopped, disabled and removed; key, user and other units kept; no TUNNELS = hands off"

# ------------------------------------------------------ 11. adopting the box

# Tear down all of the above, then recreate the live box's HAND-MADE state: the
# user, its home and key, the pinned known_hosts, and the unit text exactly as
# on the box (only the remote address differs: it is this container's sshd).
systemctl disable --now devotee-db-tunnel.service alt-db-tunnel.service >/dev/null 2>&1 || true
rm -f /etc/systemd/system/*-tunnel.service; systemctl daemon-reload
userdel "$TU" 2>/dev/null || true
rm -rf /var/lib/ikenga-tunnel
if getent group "$TU" >/dev/null; then groupdel "$TU"; fi
nothing_created || fail "teardown incomplete"
useradd --system --user-group --no-create-home --home-dir "$THOME" --shell /usr/sbin/nologin "$TU"
install -d -m 0700 -o "$TU" -g "$TU" "$THOME"
as_tun install -d -m 0700 "$SSHDIR"
as_tun ssh-keygen -q -t ed25519 -N '' -C "ikenga-tunnel@royalti-box" -f "$KEY"
printf '127.0.0.1 %s\n' "$HKEY" | as_tun sh -c 'umask 022; cat > "$1"' sh "$KH"
cat > "$UNITS/devotee-db-tunnel.service" <<'EOF'
[Unit]
Description=SSH tunnel to the Devotee Postgres (Coolify nso-postgres) for backups
Documentation=https://github.com/ikenga-hq/ikenga/blob/main/scripts/server/README.md
After=network-online.target
Wants=network-online.target

[Service]
User=ikenga-tunnel
Group=ikenga-tunnel
ExecStart=/usr/bin/ssh -NT \
  -i /var/lib/ikenga-tunnel/.ssh/id_ed25519 \
  -o UserKnownHostsFile=/var/lib/ikenga-tunnel/.ssh/known_hosts \
  -o StrictHostKeyChecking=yes \
  -o IdentitiesOnly=yes \
  -o ExitOnForwardFailure=yes \
  -o ServerAliveInterval=30 \
  -o ServerAliveCountMax=3 \
  -o BatchMode=yes \
  -L 127.0.0.1:5544:127.0.0.1:55432 \
  pgtunnel@127.0.0.1
Restart=always
RestartSec=10
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
ReadOnlyPaths=/var/lib/ikenga-tunnel

[Install]
WantedBy=multi-user.target
EOF
chmod 0644 "$UNITS/devotee-db-tunnel.service"
FPB="$(fp)"
printf 'restrict,port-forwarding,permitopen="127.0.0.1:55432",from="127.0.0.1" %s\n' "$(cut -d' ' -f1,2 $KEY.pub)" > $REMOTE/authorized_keys.pgtunnel
systemctl daemon-reload; systemctl enable --now devotee-db-tunnel.service >/dev/null 2>&1
wait_for 20 listening 5544 || fail "the hand-made box tunnel did not come up"
ok "the hand-made tunnel works before adoption" reach 5544 55432
snap() { local f; for f in "$KEY" "$KEY.pub" "$KH" "$UNITS/devotee-db-tunnel.service"; do printf '%s %s %s\n' "$(sha256sum < "$f" | cut -c1-16)" "$(stat -c '%Y %a %U' "$f")" "$f"; done; stat -c '%Y %a %U' "$SSHDIR" "$THOME"; getent passwd $TU; }
SNAP1="$(snap)"; INVB="$(systemctl show -p InvocationID --value devotee-db-tunnel.service)"
sleep 1
profile "TUNNEL_FROM=127.0.0.1" "TUNNEL_KNOWN_HOSTS=(\"127.0.0.1 $HKEY\")" "TUNNELS=(\"$T1\")"
prov --dry-run
{ [[ $RC -eq 0 ]] && out_has 'no changes: the host already matches this profile' && ! out_has 'would:'; } || fail "adopt (dry run) should plan nothing"
prov
{ [[ $RC -eq 0 ]] && out_has 'no changes: the host already matches this profile'; } || fail "adopting the box's tunnel should report no changes"
[[ "$(snap)" == "$SNAP1" ]] || { diff <(echo "$SNAP1") <(snap) >&2 || true; fail "adoption touched the user, key, known_hosts or unit"; }
[[ "$(fp)" == "$FPB" ]] || fail "the key fingerprint changed on adoption"
[[ "$(cut -d' ' -f3 $KEY.pub)" == "ikenga-tunnel@royalti-box" ]] || fail "the key comment changed"
[[ "$(systemctl show -p InvocationID --value devotee-db-tunnel.service)" == "$INVB" ]] || fail "adoption restarted the tunnel"
ok "the adopted tunnel still works" reach 5544 55432
out_has "$(cut -d' ' -f2 $KEY.pub)" || fail "the authorized_keys line should print the adopted key"
# Only cosmetics differ: a comment and a Description are not a change either.
sed -i '1i # hand-edited note' "$UNITS/devotee-db-tunnel.service"; sed -i 's/^Description=.*/Description=renamed/' "$UNITS/devotee-db-tunnel.service"
prov; { [[ $RC -eq 0 ]] && out_has 'no changes'; } || fail "comment/Description edits should not count as a change"
# A real difference IS converged (old unit kept aside, tunnel restarted).
sed -i 's/^RestartSec=10/RestartSec=30/' "$UNITS/devotee-db-tunnel.service"
prov
{ [[ $RC -eq 0 ]] && out_has 'unit devotee-db-tunnel.service installed' && out_has 'devotee-db-tunnel.service restarted' && grep -q '^RestartSec=10' "$UNITS/devotee-db-tunnel.service" && compgen -G "$UNITS/devotee-db-tunnel.service.bak-*" >/dev/null; } || fail "a drifted unit should be restored, restarted and the old one kept"
[[ "$(fp)" == "$FPB" ]] || fail "the key changed"
wait_for 20 listening 5544 || fail "tunnel not up again"
ok "tunnel up again" reach 5544 55432
pass "adopt-existing: the box's hand-made user, key, known_hosts and unit are taken over with no change"

echo "==> [Container] ALL TUNNEL CHECKS PASSED ($CHECKS assertions)"
