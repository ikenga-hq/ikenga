#!/bin/bash
# provision.sh sync-accounts (shared project clones + scoped secrets), run as
# root in an ubuntu:24.04 container. No network is needed at test time: the
# project is a local bare repo plus a loopback HTTP server for the private-repo
# path. Image: ikenga-test-accounts:24.04 (ubuntu + git + acl + python3).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
chmod +x "$SCRIPT_DIR/test-accounts-inside.sh"

if ! docker image inspect ikenga-test-accounts:24.04 >/dev/null 2>&1; then
  echo "==> Building ikenga-test-accounts:24.04 image..."
  docker build --network=host -t ikenga-test-accounts:24.04 - << 'EOF'
FROM ubuntu:24.04
RUN echo 'Acquire::ForceIPv4 "true";' > /etc/apt/apt.conf.d/99force-ipv4 && \
    apt-get update -qq && \
    apt-get install -y -qq --no-install-recommends git acl python3 ca-certificates curl && \
    rm -rf /var/lib/apt/lists/*
EOF
fi

echo "==> Running provision.sh sync-accounts test suite in ubuntu:24.04 container..."
docker run --rm -i \
  -v "$SCRIPT_DIR:/work:ro" \
  ikenga-test-accounts:24.04 /work/test-accounts-inside.sh

echo "==> Container test suite completed successfully."
