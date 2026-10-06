#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
chmod +x "$SCRIPT_DIR/test-upgrade-inside.sh"

if ! docker image inspect ikenga-test:24.04 >/dev/null 2>&1; then
  echo "==> Building ikenga-test:24.04 image..."
  docker build --network=host -t ikenga-test:24.04 - << 'EOF'
FROM ubuntu:24.04
RUN echo 'Acquire::ForceIPv4 "true";' > /etc/apt/apt.conf.d/99force-ipv4 && \
    apt-get update -qq && \
    apt-get install -y -qq curl ca-certificates python3
EOF
fi

echo "==> Running provision.sh upgrade test suite in ubuntu:24.04 container..."
docker run --rm -i \
  -v "$SCRIPT_DIR:/work:ro" \
  ikenga-test:24.04 /work/test-upgrade-inside.sh

echo "==> Container test suite completed successfully."
