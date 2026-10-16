#!/bin/bash
# provision.sh per-project deploy tokens (PROJECTS_TOKEN_SECRETS), run as root in
# an ubuntu:24.04 container via `refresh-projects`. No network is needed: the
# private repos are served by a loopback git smart-HTTP server that wants a
# different fake token per repo. Reuses the ikenga-test-accounts:24.04 image
# (ubuntu + git + acl + python3) that test-accounts-container.sh builds.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
chmod +x "$SCRIPT_DIR/test-projects-token-inside.sh"

if ! docker image inspect ikenga-test-accounts:24.04 >/dev/null 2>&1; then
  echo "==> Building ikenga-test-accounts:24.04 image..."
  docker build --network=host -t ikenga-test-accounts:24.04 - << 'DOCKER'
FROM ubuntu:24.04
RUN echo 'Acquire::ForceIPv4 "true";' > /etc/apt/apt.conf.d/99force-ipv4 && \
    apt-get update -qq && \
    apt-get install -y -qq --no-install-recommends git acl python3 ca-certificates curl && \
    rm -rf /var/lib/apt/lists/*
DOCKER
fi

echo "==> Running provision.sh per-project deploy token test suite in ubuntu:24.04 container..."
docker run --rm -i \
  -v "$SCRIPT_DIR:/work:ro" \
  ikenga-test-accounts:24.04 /work/test-projects-token-inside.sh

echo "==> Container test suite completed successfully."
