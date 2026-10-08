#!/bin/bash
# Regression suite for the security fixes to `provision.sh backups` (symlink-safe
# ownership, gcloud credentials removal, concurrent schedules, "enabled" must be
# true, BACKUP_PG_MAJOR >= 17). It runs test-backups-hardening-inside.sh twice:
#
#   1. mock    plain `docker run`: a mock systemctl (like test-backups-container.sh)
#   2. systemd a privileged container with real systemd as PID 1; units are
#              started with the real `systemctl start`, sandbox included
#
# Both need network access for apt (PGDG, Google Cloud SDK repo). The checks are
# soft: every check runs and the suite prints a summary, so one run against an
# older provision.sh shows every check that fails there.
#
#   PROVISION_DIR=/path/to/older/scripts/server ./test-backups-hardening-container.sh mock
#       runs THIS directory's tests against another checkout's provision.sh
#
# Usage: test-backups-hardening-container.sh [mock|systemd|both]   (default both)
# Env:   APT_CACHE_DIR=/dir  keep downloaded .debs between runs
#        PROVISION_DIR=/dir  scripts/server of the code under test (default: this one)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROVISION_DIR="$(cd "${PROVISION_DIR:-$SCRIPT_DIR}" && pwd)"
MODE="${1:-both}"
IMAGE=ikenga-test-backups:24.04
chmod +x "$SCRIPT_DIR/test-backups-hardening-inside.sh"

if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
  echo "==> Building $IMAGE image..."
  docker build --network=host -t "$IMAGE" - << 'DOCKERFILE'
FROM ubuntu:24.04
RUN echo 'Acquire::ForceIPv4 "true";' > /etc/apt/apt.conf.d/99force-ipv4 && \
    apt-get update -qq && \
    apt-get install -y -qq --no-install-recommends systemd gnupg openssl acl ca-certificates curl && \
    rm -rf /var/lib/apt/lists/*
DOCKERFILE
fi

CACHE_ARGS=()
if [[ -n "${APT_CACHE_DIR:-}" ]]; then
  mkdir -p "$APT_CACHE_DIR/partial"
  CACHE_ARGS=(-v "$APT_CACHE_DIR:/var/cache/apt/archives" -e KEEP_APT_ARCHIVES=1)
fi
MOUNTS=(-v "$SCRIPT_DIR:/tests:ro" -v "$PROVISION_DIR:/work:ro")

run_mock() {
  echo "==> Hardening suite, mock systemctl (provision.sh from $PROVISION_DIR)..."
  docker run --rm -i "${CACHE_ARGS[@]}" "${MOUNTS[@]}" "$IMAGE" /tests/test-backups-hardening-inside.sh
}

run_systemd() {
  name="ikenga-bk-hardening-$$"   # global: the EXIT trap runs after this function returns
  echo "==> Hardening suite, real systemd as PID 1 (provision.sh from $PROVISION_DIR)..."
  trap 'docker rm -f "$name" >/dev/null 2>&1 || true' EXIT   # also when a check fails (set -e exits before a RETURN trap)
  docker run -d --name "$name" --privileged --tmpfs /run --tmpfs /run/lock --tmpfs /tmp \
    "${CACHE_ARGS[@]}" "${MOUNTS[@]}" "$IMAGE" /lib/systemd/systemd >/dev/null
  local i state=""
  for i in $(seq 1 60); do
    state="$(docker exec "$name" systemctl is-system-running 2>/dev/null || true)"
    [[ "$state" == running || "$state" == degraded ]] && break
    sleep 1
  done
  [[ "$state" == running || "$state" == degraded ]] || { echo "systemd did not come up (state: ${state:-none})" >&2; docker logs "$name" 2>&1 | tail -20 >&2; return 1; }
  docker exec -i "$name" /tests/test-backups-hardening-inside.sh
}

case "$MODE" in
  mock) run_mock ;;
  systemd) run_systemd ;;
  both) run_mock; run_systemd ;;
  *) echo "usage: $0 [mock|systemd|both]" >&2; exit 2 ;;
esac
echo "==> Hardening suite completed successfully."
