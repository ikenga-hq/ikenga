#!/bin/bash
# provision.sh swap, run as root in a PRIVILEGED ubuntu:24.04 container.
#
# What is real: ext4 filesystems on loop devices (a swap file cannot live on
# the container's overlayfs), fallocate, mkswap, swapon/swapoff, /etc/fstab and
# vm.swappiness. What is shimmed (see test-swap-inside.sh): `swapon --show` hides
# the HOST's own swap (the swap table is global, not per container) and can
# report a fake "used" figure; `fallocate` can be made to fail.
#
# The swap table and vm.swappiness are kernel-global, so the test puts back
# what it changed on exit (swapoff of its own files, the original swappiness)
# and this wrapper prints the host swap table afterwards.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROVISION_DIR="$(cd "${PROVISION_DIR:-$SCRIPT_DIR}" && pwd)"
chmod +x "$SCRIPT_DIR/test-swap-inside.sh"

before="$(cat /proc/swaps)"
echo "==> Running provision.sh swap test suite (loop-mounted ext4, real swapon) in ubuntu:24.04..."
docker run --rm -i --privileged \
  -v "$SCRIPT_DIR:/tests:ro" -v "$PROVISION_DIR:/work:ro" \
  ubuntu:24.04 /tests/test-swap-inside.sh
after="$(cat /proc/swaps)"
if [[ "$before" == "$after" || "$(printf '%s\n' "$before" | awk '{print $1}')" == "$(printf '%s\n' "$after" | awk '{print $1}')" ]]; then
  echo "==> Host swap table unchanged after the run."
else
  echo "WARNING: host swap table differs after the run:" >&2; echo "$after" >&2; exit 1
fi
echo "==> Container test suite completed successfully."
