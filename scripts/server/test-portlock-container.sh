#!/bin/bash
# provision.sh tunnels, the PORT LOCK (nftables table inet ikenga_tunnels) and
# the squatting defence (require_auth in run-backup.sh), in a PRIVILEGED
# ubuntu:24.04 container with real systemd as PID 1 and the REAL kernel's nf_tables.
#
# Three stages, in one container:
#   1. test-portlock-inside.sh before   everything that needs no reboot
#   2. the container is STOPPED THE WAY SYSTEMD WANTS (SIGRTMIN+3) and started
#      again: a real boot, with /etc and the unit enablement kept and /run, /tmp
#      and the nft ruleset gone
#   3. test-portlock-inside.sh after    the lock was loaded by the unit at boot
#
# No network (--network none): the "remote host" is a second sshd on 127.0.0.1
# (ports 22 and 2222) behind the printed authorized_keys line, and a fake
# Postgres listener behind it. The squatting test runs the real run-backup.sh
# and the real libpq (postgresql-client from the Ubuntu archive; require_auth
# arrived in libpq 16, and the provisioner installs 17) against a fake server.
# Image: ikenga-test-portlock:24.04
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROVISION_DIR="$(cd "${PROVISION_DIR:-$SCRIPT_DIR}" && pwd)"
IMAGE=ikenga-test-portlock:24.04
chmod +x "$SCRIPT_DIR/test-portlock-inside.sh" "$SCRIPT_DIR/test-portlock-fakepg.py"

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
  echo "==> Building $IMAGE image..."
  docker build --network=host -t "$IMAGE" - << 'DOCKERFILE'
FROM ubuntu:24.04
RUN echo 'Acquire::ForceIPv4 "true";' > /etc/apt/apt.conf.d/99force-ipv4 && \
    apt-get update -qq && \
    apt-get install -y -qq --no-install-recommends systemd openssh-server openssh-client socat iproute2 util-linux procps ca-certificates \
      nftables ufw iptables python3 jq gzip postgresql-client && \
    systemctl disable ssh ssh.socket >/dev/null 2>&1 || true && \
    rm -rf /var/lib/apt/lists/*
DOCKERFILE
fi

name="ikenga-portlock-test-$$"
# PORTLOCK_KEEP=1 leaves the container behind (named in the last line) for a post-mortem.
trap '[[ -n "${PORTLOCK_KEEP:-}" ]] && { echo "container kept: $name"; exit; }; docker rm -f "$name" >/dev/null 2>&1 || true' EXIT

wait_systemd() {
  local state="" i
  for i in $(seq 1 90); do
    state="$(docker exec "$name" systemctl is-system-running 2>/dev/null || true)"
    [[ "$state" == running || "$state" == degraded ]] && return 0
    sleep 1
  done
  echo "systemd did not come up (state: ${state:-none})" >&2; docker logs "$name" 2>&1 | tail -20 >&2; return 1
}

echo "==> Running the port-lock test suite (real systemd, real nf_tables, no network) in $IMAGE..."
docker run -d --name "$name" --privileged --network none --tmpfs /run --tmpfs /run/lock --tmpfs /tmp \
  -v "$SCRIPT_DIR:/tests:ro" -v "$PROVISION_DIR:/work:ro" "$IMAGE" /lib/systemd/systemd >/dev/null
wait_systemd
docker exec -i "$name" /tests/test-portlock-inside.sh before

echo "==> Rebooting the container (SIGRTMIN+3 = systemd's clean shutdown; /etc and unit enablement survive, /run /tmp and the ruleset do not)..."
docker stop -s SIGRTMIN+3 -t 60 "$name" >/dev/null
docker start "$name" >/dev/null
wait_systemd
docker exec -i "$name" /tests/test-portlock-inside.sh after
echo "==> Container test suite completed successfully."
