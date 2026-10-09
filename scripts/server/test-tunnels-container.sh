#!/bin/bash
# provision.sh tunnels (SSH tunnels with a pinned host key and a per-box key),
# run as root in a PRIVILEGED ubuntu:24.04 container with real systemd as PID 1,
# so the tunnel units run with their real sandbox (ProtectSystem=strict, ...).
#
# The container has NO network (--network none): the "remote host" is a second
# sshd on 127.0.0.1 (ports 22 and 2222), a nologin user authorised with the
# restrict/permitopen line the provisioner prints, and a fake "postgres" TCP
# listener behind it. No real host can be reached, whatever a unit says.
# Image: ikenga-test-tunnels:24.04-nft (ubuntu + systemd + openssh + socat + iproute2 + nftables:
# the tunnels' port lock is an nftables table, so the real kernel enforces it here).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROVISION_DIR="$(cd "${PROVISION_DIR:-$SCRIPT_DIR}" && pwd)"
IMAGE=ikenga-test-tunnels:24.04-nft
chmod +x "$SCRIPT_DIR/test-tunnels-inside.sh"

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
  echo "==> Building $IMAGE image..."
  docker build --network=host -t "$IMAGE" - << 'DOCKERFILE'
FROM ubuntu:24.04
RUN echo 'Acquire::ForceIPv4 "true";' > /etc/apt/apt.conf.d/99force-ipv4 && \
    apt-get update -qq && \
    apt-get install -y -qq --no-install-recommends systemd openssh-server openssh-client socat iproute2 util-linux procps ca-certificates nftables && \
    systemctl disable ssh ssh.socket >/dev/null 2>&1 || true && \
    rm -rf /var/lib/apt/lists/*
DOCKERFILE
fi

name="ikenga-tunnels-test-$$"
trap 'docker rm -f "$name" >/dev/null 2>&1 || true' EXIT
echo "==> Running provision.sh tunnels test suite (real systemd, no network) in $IMAGE..."
docker run -d --name "$name" --privileged --network none --tmpfs /run --tmpfs /run/lock --tmpfs /tmp \
  -v "$SCRIPT_DIR:/tests:ro" -v "$PROVISION_DIR:/work:ro" "$IMAGE" /lib/systemd/systemd >/dev/null
state=""
for i in $(seq 1 60); do
  state="$(docker exec "$name" systemctl is-system-running 2>/dev/null || true)"
  [[ "$state" == running || "$state" == degraded ]] && break
  sleep 1
done
[[ "$state" == running || "$state" == degraded ]] || { echo "systemd did not come up (state: ${state:-none})" >&2; docker logs "$name" 2>&1 | tail -20 >&2; exit 1; }
docker exec -i "$name" /tests/test-tunnels-inside.sh
echo "==> Container test suite completed successfully."
