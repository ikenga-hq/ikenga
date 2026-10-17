#!/bin/bash
# provision.sh push tokens (PROJECTS_PUSH_TOKEN_SECRETS) and shared-main projects
# (PROJECTS_SHARED_MAIN), run as root in an ubuntu:24.04 container via
# sync-accounts and refresh-projects. No outside network is needed: the projects
# are served by a loopback git smart-HTTP server that accepts pushes. Reuses the
# ikenga-test-accounts:24.04 image (ubuntu + git + acl + python3) that
# test-accounts-container.sh builds.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
chmod +x "$SCRIPT_DIR/test-projects-push-inside.sh"

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

echo "==> Running provision.sh PROJECTS_PUSH_TOKEN_SECRETS + PROJECTS_SHARED_MAIN test suite in ubuntu:24.04 container..."
docker run --rm -i \
  -v "$SCRIPT_DIR:/work:ro" \
  ikenga-test-accounts:24.04 /work/test-projects-push-inside.sh

echo "==> Container test suite completed successfully."
