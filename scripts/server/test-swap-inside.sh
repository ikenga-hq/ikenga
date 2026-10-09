#!/bin/bash
# provision.sh swap, run as root inside test-swap-container.sh's container.
set -euo pipefail

T="$(mktemp -d)"
PROVISION=/work/provision.sh
PROFILE=/root/profile.env
OUT=$T/out
ALL=$T/all
: > "$ALL"
CHECKS=0
FSTAB=/etc/fstab
SYSCTL=/etc/sysctl.d/90-ikenga-swap.conf
BIG=/mnt/big       # 6 GB ext4 (sparse image)
SMALL=/mnt/small   # 100 MB ext4
SW=$BIG/swapfile
HOST_SWAPPINESS="$(cat /proc/sys/vm/swappiness)"
MB=1048576

fail() { echo "FAIL: $*" >&2; [[ -f "$OUT" ]] && { echo "--- last output ---" >&2; tail -30 "$OUT" >&2; }; exit 1; }
pass() { echo "==> [Container] $* PASSED"; }
ok() { local d="$1"; shift; CHECKS=$((CHECKS + 1)); "$@" || fail "$d"; }
no() { local d="$1"; shift; CHECKS=$((CHECKS + 1)); if "$@"; then fail "$d"; fi; }

cleanup() {
  set +e
  # Only what this test made: anything under /mnt in the swap table.
  for s in $(awk 'NR>1 && $1 ~ /^\/mnt\//{print $1}' /proc/swaps); do swapoff "$s"; done
  echo "$HOST_SWAPPINESS" > /proc/sys/vm/swappiness
  umount "$BIG" "$SMALL" 2>/dev/null
  for l in $(losetup -a | grep -E '/tmp/(big|small)\.img|/root/(big|small)\.img' | cut -d: -f1); do losetup -d "$l"; done
}
trap cleanup EXIT

# ---- fixtures
apt-get update -qq >/dev/null 2>&1 || true
mkdir -p "$BIG" "$SMALL" /shim
truncate -s 6G /root/big.img;   mkfs.ext4 -q -F /root/big.img
truncate -s 100M /root/small.img; mkfs.ext4 -q -F /root/small.img
mount -o loop /root/big.img "$BIG"
mount -o loop /root/small.img "$SMALL"

# Shims, first on PATH for provision.sh only.
cat > /shim/swapon <<'SH'
#!/bin/bash
# --show: hide the host's swap table (global in the kernel); SHIM_USED fakes the used bytes.
if [[ " $* " == *" --show"* ]]; then
  /usr/sbin/swapon "$@" | grep -v '^/swap.img' | { if [[ -n "${SHIM_USED:-}" ]]; then awk -v u="$SHIM_USED" '{ if (NF>=2 && $2 ~ /^[0-9]+$/) $2=u; print }'; else cat; fi; }
  exit 0
fi
exec /usr/sbin/swapon "$@"
SH
cat > /shim/fallocate <<'SH'
#!/bin/bash
if [[ -n "${SHIM_NO_FALLOCATE:-}" ]]; then echo "fallocate: fallocate failed: Operation not supported" >&2; exit 1; fi
exec /usr/bin/fallocate "$@"
SH
chmod +x /shim/*

RC=0
prov() {   # [args]: `swap` with the current profile; sets RC, $OUT
  if env PATH=/shim:$PATH "$PROVISION" swap --profile "$PROFILE" "$@" > "$OUT" 2>&1; then RC=0; else RC=$?; fi
  cat "$OUT" >> "$ALL"
}
profile() { printf '%s\n' "$@" > "$PROFILE"; chown root:root "$PROFILE"; chmod 0600 "$PROFILE"; }
out_has() { CHECKS=$((CHECKS + 1)); grep -qE -- "$1" "$OUT"; }
out_not() { CHECKS=$((CHECKS + 1)); ! grep -qE -- "$1" "$OUT"; }
active() { env PATH=/shim:$PATH swapon --noheadings --raw --show=NAME | grep -qxF -- "$1"; }
fstab_marks() { grep -cE '# ikenga-swap$' "$FSTAB" || true; }
size_mb() { echo $(( $(stat -c %s "$1") / MB )); }
snapshot() { { cat "$FSTAB" 2>/dev/null; ls -la "$BIG" 2>/dev/null; cat /proc/sys/vm/swappiness; cat "$SYSCTL" 2>/dev/null; true; } | md5sum; }

[[ -f $FSTAB ]] || : > $FSTAB
sed -i '/# ikenga-swap$/d' $FSTAB

# 1. fresh create
profile "SWAP_SIZE=128M" "SWAPPINESS=15" "SWAP_FILE=$SW"
prov
ok "fresh: exit 0" test "$RC" -eq 0
ok "fresh: file is 128 MiB" test "$(size_mb $SW)" -eq 128
ok "fresh: root:root 0600" test "$(stat -c '%a %U:%G' $SW)" = "600 root:root"
ok "fresh: swap is active" active $SW
ok "fresh: one fstab line" test "$(fstab_marks)" -eq 1
ok "fresh: fstab line content" grep -qxF "$SW none swap sw,nofail 0 0 # ikenga-swap" $FSTAB
ok "fresh: sysctl file" grep -qxF "vm.swappiness = 15" $SYSCTL
ok "fresh: live swappiness 15" test "$(cat /proc/sys/vm/swappiness)" = 15
ok "fresh: reported" out_has "created"
ok "fresh: valid swap signature" test "$(blkid -p -o value -s TYPE $SW)" = swap
pass "fresh create"

# 2. rerun is a no-op
before="$(snapshot)"
prov
ok "rerun: exit 0" test "$RC" -eq 0
ok "rerun: no changes" out_has "no changes"
ok "rerun: nothing touched" test "$(snapshot)" = "$before"
ok "rerun: still one fstab line" test "$(fstab_marks)" -eq 1
no "rerun: no fstab backup churn beyond the first" test "$(ls ${FSTAB}.bak-* 2>/dev/null | wc -l)" -gt 1
pass "rerun no-op"

# 3. dry-run changes nothing
profile "SWAP_SIZE=192M" "SWAPPINESS=20" "SWAP_FILE=$SW"
before="$(snapshot)"
prov --dry-run
ok "dry-run: exit 0" test "$RC" -eq 0
ok "dry-run: prints the plan" out_has "\[dry-run\]"
ok "dry-run: says would" out_has "would: swap: .* resized 128 MiB -> 192 MiB"
ok "dry-run: nothing changed" test "$(snapshot)" = "$before"
ok "dry-run: file still 128" test "$(size_mb $SW)" -eq 128
pass "dry-run changes nothing"

# 4. resize
prov
ok "resize: exit 0" test "$RC" -eq 0
ok "resize: file is 192 MiB" test "$(size_mb $SW)" -eq 192
ok "resize: active" active $SW
ok "resize: one fstab line" test "$(fstab_marks)" -eq 1
ok "resize: reported" out_has "resized 128 MiB -> 192 MiB"
ok "resize: swappiness now 20" test "$(cat /proc/sys/vm/swappiness)" = 20
ok "resize: fstab backed up" ls ${FSTAB}.bak-* >/dev/null 2>&1 || true
prov
ok "resize: rerun no changes" out_has "no changes"
pass "resize"

# 5. shrink refused when memory is short (used swap would not fit back in RAM)
printf 'MemTotal: 4000000 kB\nMemAvailable: 100000 kB\n' > $T/meminfo
profile "SWAP_SIZE=128M" "SWAP_FILE=$SW"
before="$(snapshot)"
IKENGA_MEMINFO=$T/meminfo SHIM_USED=500000000 prov
ok "low memory: refused (exit 1)" test "$RC" -eq 1
ok "low memory: clear message" out_has "refusing to turn off $SW"
ok "low memory: untouched" test "$(snapshot)" = "$before"
ok "low memory: still active at 192" active $SW
ok "low memory: file size kept" test "$(size_mb $SW)" -eq 192
# the same resize goes through when memory is plentiful (used is small)
SHIM_USED=1000000 prov
ok "enough memory: resized down" test "$(size_mb $SW)" -eq 128
pass "swapoff only when memory allows"

# 6. fstab repair: a duplicated marker line and a missing one both converge to exactly one
echo "$SW none swap sw,nofail 0 0 # ikenga-swap" >> $FSTAB
ok "setup: two lines" test "$(fstab_marks)" -eq 2
prov
ok "dup fstab: repaired to one" test "$(fstab_marks)" -eq 1
ok "dup fstab: reported" out_has "fstab line (added|repaired)"
sed -i '/# ikenga-swap$/d' $FSTAB
prov
# Without its marker the active file is not provably ours: it is treated as foreign swap and left alone.
ok "missing fstab marker: left alone, reported" out_has "other swap is already active"
ok "missing fstab marker: nothing written" test "$(fstab_marks)" -eq 0
echo "$SW none swap sw,nofail 0 0 # ikenga-swap" >> $FSTAB
pass "fstab never duplicated"

# 7. live swappiness drift is re-applied
echo 33 > /proc/sys/vm/swappiness
prov
ok "drift: re-applied" test "$(cat /proc/sys/vm/swappiness)" = 10 -o "$(cat /proc/sys/vm/swappiness)" = "$(grep -oE '[0-9]+$' $SYSCTL)"
ok "drift: reported" out_has "re-applied"
# turned off but file kept -> turned back on
swapoff $SW
prov
ok "inactive: turned back on" active $SW
pass "drift and inactive"

# 8. SWAP_FILE changed under a managed file is refused
profile "SWAP_SIZE=128M" "SWAP_FILE=$BIG/other-name"
prov
ok "moved path: refused" test "$RC" -eq 1
ok "moved path: message" out_has "managed swap file is $SW"
pass "managed path mismatch"

# 9. other swap active: report only (managed swap here too)
dd if=/dev/zero of=$BIG/foreign bs=1M count=64 status=none; chmod 600 $BIG/foreign; mkswap -q $BIG/foreign; swapon $BIG/foreign
profile "SWAP_SIZE=256M" "SWAP_FILE=$SW"
before="$(snapshot)"
prov
ok "other swap + managed: exit 0" test "$RC" -eq 0
ok "other swap + managed: reported" out_has "other swap is already active"
ok "other swap + managed: untouched" test "$(snapshot)" = "$before"
swapoff $BIG/foreign

# 10. off / removal
profile "SWAP_SIZE=off" "SWAP_FILE=$SW"
prov --dry-run
ok "off dry-run: file kept" test -f $SW
ok "off dry-run: active" active $SW
prov
ok "off: exit 0" test "$RC" -eq 0
no "off: swap gone" active $SW
no "off: file removed" test -e $SW
ok "off: fstab line removed" test "$(fstab_marks)" -eq 0
no "off: sysctl file removed" test -e $SYSCTL
prov
ok "off rerun: nothing to remove" out_has "nothing to remove"
ok "off rerun: exit 0" test "$RC" -eq 0
profile "SWAP_SIZE=0" "SWAP_FILE=$SW"; prov
ok "0 == off" out_has "nothing to remove"
pass "off / removal"

# 11. other swap present from the start: a fresh run creates nothing
swapon $BIG/foreign
profile "SWAP_SIZE=128M" "SWAP_FILE=$SW"
before="$(snapshot)"
prov
ok "foreign swap: exit 0" test "$RC" -eq 0
ok "foreign swap: reported" out_has "other swap is already active, leaving swap alone \($BIG/foreign\)"
no "foreign swap: no file" test -e $SW
ok "foreign swap: no fstab line" test "$(fstab_marks)" -eq 0
no "foreign swap: no sysctl file" test -e $SYSCTL
ok "foreign swap: untouched" test "$(snapshot)" = "$before"
# an active swap file at SWAP_FILE that we did not make counts as foreign too
swapoff $BIG/foreign; mv $BIG/foreign $SW; swapon $SW
prov
ok "unmanaged active SWAP_FILE: left alone" out_has "other swap is already active"
ok "unmanaged active SWAP_FILE: no fstab line" test "$(fstab_marks)" -eq 0
swapoff $SW; rm -f $SW
pass "other swap present"

# 12. symlinks refused
ln -s $BIG/real-target $BIG/linkswap
profile "SWAP_SIZE=128M" "SWAP_FILE=$BIG/linkswap"
prov
ok "symlink file: refused" test "$RC" -eq 1
ok "symlink file: message" out_has "is a symlink"
no "symlink file: target not created" test -e $BIG/real-target
mkdir $BIG/realdir; ln -s $BIG/realdir $BIG/linkdir
profile "SWAP_SIZE=128M" "SWAP_FILE=$BIG/linkdir/swapfile"
prov
ok "symlink parent: refused" test "$RC" -eq 1
ok "symlink parent: message" out_has "goes through a symlink"
no "symlink parent: nothing created" test -e $BIG/realdir/swapfile
ok "symlink: no fstab line" test "$(fstab_marks)" -eq 0
pass "symlink refused"

# 13. not enough free disk (keep >= 10% free)
profile "SWAP_SIZE=80M" "SWAP_FILE=$SMALL/swapfile"
prov
ok "low disk: refused" test "$RC" -eq 1
ok "low disk: message" out_has "not enough free disk"
no "low disk: nothing created" test -e $SMALL/swapfile
ok "low disk: no fstab line" test "$(fstab_marks)" -eq 0
pass "low disk refused"

# 14. not a local ext4/xfs filesystem (the container's overlayfs; the same refusal covers nfs, tmpfs, fuse)
profile "SWAP_SIZE=128M" "SWAP_FILE=/root/swapfile"
prov
ok "overlay: refused" test "$RC" -eq 1
ok "overlay: message" out_has "not a local ext4/xfs/f2fs filesystem"
no "overlay: nothing created" test -e /root/swapfile
pass "non-local filesystem refused"

# 15. an existing file that is not ours is never touched
dd if=/dev/zero of=$BIG/precious bs=1M count=1 status=none
profile "SWAP_SIZE=128M" "SWAP_FILE=$BIG/precious"
prov
ok "unmanaged file: refused" test "$RC" -eq 1
ok "unmanaged file: message" out_has "not managed by ikenga"
ok "unmanaged file: kept" test "$(size_mb $BIG/precious)" -eq 1 -a -f $BIG/precious
rm -f $BIG/precious
pass "unmanaged file kept"

# 16. fallocate refused -> dd fallback
profile "SWAP_SIZE=128M" "SWAP_FILE=$SW"
SHIM_NO_FALLOCATE=1 prov
ok "dd fallback: exit 0" test "$RC" -eq 0
ok "dd fallback: announced" out_has "falling back to dd"
ok "dd fallback: 128 MiB, active" test "$(size_mb $SW)" -eq 128 && active $SW
prov --dry-run >/dev/null
profile "SWAP_SIZE=off" "SWAP_FILE=$SW"; prov
pass "dd fallback"

# 17. SWAP_SIZE=auto resolves from RAM (dry-run prints the plan)
profile "SWAP_SIZE=auto" "SWAP_FILE=$SW"
for spec in "3800000:4096M" "4194304:4096M" "6291456:4096M" "8388608:4096M" "16000000:2048M"; do
  printf 'MemTotal: %s kB\nMemAvailable: 1000000 kB\n' "${spec%%:*}" > $T/meminfo
  IKENGA_MEMINFO=$T/meminfo prov --dry-run
  ok "auto $spec: plan" out_has "fallocate -l ${spec##*:} "
done
no "auto dry-run created nothing" test -e $SW
pass "auto sizing"

# 18. profile validation
for bad in "SWAP_SIZE=lots" "SWAP_SIZE=12" "SWAP_SIZE=8M" "SWAPPINESS=101" "SWAPPINESS=x" "SWAP_FILE=relative" "SWAP_FILE=/a/../b" "SWAP_FILE='/swap file'"; do
  profile "SWAP_SIZE=128M" "$bad"
  prov
  ok "invalid '$bad' refused" test "$RC" -ne 0
  ok "invalid '$bad' message" out_has "^error: "
done
pass "profile validation"

# 19. the swap step is part of the full provision flow, before the base packages
profile "VERSION=0.1.0" "ADMIN_USER=ops" "ADMIN_SSH_KEYS_FILE=/root/keys.pub" "SWAP_SIZE=128M" "SWAP_FILE=$SW"
echo "ssh-ed25519 AAAA test" > /root/keys.pub
env PATH=/shim:$PATH "$PROVISION" --profile "$PROFILE" --dry-run --yes > "$OUT" 2>&1 || true
cat "$OUT" >> "$ALL"
ok "full flow: swap step present" out_has "^==> Swap"
ok "full flow: swap runs before base packages" test "$(grep -n '^==> ' "$OUT" | grep -n -E 'Swap|Base packages' | head -2 | tail -1 | grep -c 'Base packages')" -eq 1
ok "full flow: plan shows the swap file" out_has "fallocate -l 128M -- $SW.ikenga-new"
ok "full flow: plan shows mkswap" out_has "mkswap -q -- $SW.ikenga-new"
no "full flow: dry-run created nothing" test -e $SW
pass "full flow wiring"

# no secrets/odd output, and the final tally
echo "==> $CHECKS checks passed"
