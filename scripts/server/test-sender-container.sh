#!/bin/bash
# test-sender-container.sh — provision.sh sender test suite runner in docker container
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
chmod +x "$SCRIPT_DIR/test-sender-inside.sh"

echo "==> Running provision.sh sender test suite in ubuntu:24.04 container..."
docker run --rm -i \
  -v "$SCRIPT_DIR:/work:ro" \
  ikenga-test-accounts:24.04 /work/test-sender-inside.sh

echo "==> Container sender test suite completed successfully."
