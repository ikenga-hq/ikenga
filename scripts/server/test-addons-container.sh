#!/bin/bash
# provision.sh ADDONS hook (provision, sync-accounts, upgrade, apply-request),
# run as root in an ubuntu:24.04 container. Nothing here touches the host: the
# container has no mounts but this directory (read-only) and the provisioner.
#
# PROVISION_DIR=<dir holding a provision.sh> tests a different copy (used to
# prove each check fails when it is broken).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROVISION_DIR="$(cd "${PROVISION_DIR:-$SCRIPT_DIR}" && pwd)"
chmod +x "$SCRIPT_DIR/test-addons-inside.sh"

if ! docker image inspect ikenga-test:24.04 >/dev/null 2>&1; then
  echo "==> Building ikenga-test:24.04 image..."
  docker build --network=host -t ikenga-test:24.04 - << 'DOCKERFILE'
FROM ubuntu:24.04
RUN echo 'Acquire::ForceIPv4 "true";' > /etc/apt/apt.conf.d/99force-ipv4 && \
    apt-get update -qq && \
    apt-get install -y -qq curl ca-certificates python3
DOCKERFILE
fi

echo "==> Running provision.sh ADDONS test suite in ubuntu:24.04 container..."
docker run --rm -i \
  -v "$SCRIPT_DIR:/tests:ro" -v "$PROVISION_DIR:/work:ro" \
  ikenga-test:24.04 /tests/test-addons-inside.sh

echo "==> Container test suite completed successfully."
