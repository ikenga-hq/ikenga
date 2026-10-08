#!/bin/bash
# provision.sh backups (Postgres backups to GCS as system jobs, D-B10), run as
# root in an ubuntu:24.04 container. The container needs network access for
# apt (PGDG, Google Cloud SDK repo): PostgreSQL 17 is installed in the same
# container as a local server with two fake databases; GCS itself is never
# touched (a fake gcloud on the backup user's PATH writes to a local dir).
# Image: ikenga-test-backups:24.04 (ubuntu + systemd-analyze + gpg + openssl).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
chmod +x "$SCRIPT_DIR/test-backups-inside.sh"

if ! docker image inspect ikenga-test-backups:24.04 >/dev/null 2>&1; then
  echo "==> Building ikenga-test-backups:24.04 image..."
  docker build --network=host -t ikenga-test-backups:24.04 - << 'DOCKERFILE'
FROM ubuntu:24.04
RUN echo 'Acquire::ForceIPv4 "true";' > /etc/apt/apt.conf.d/99force-ipv4 && \
    apt-get update -qq && \
    apt-get install -y -qq --no-install-recommends systemd gnupg openssl acl ca-certificates curl && \
    rm -rf /var/lib/apt/lists/*
DOCKERFILE
fi

# Optional: APT_CACHE_DIR=/some/dir keeps the downloaded .debs between runs
# (the google-cloud-cli package is ~90 MB). Packages are still verified against
# the signed indexes; this only skips the download.
CACHE_ARGS=()
if [[ -n "${APT_CACHE_DIR:-}" ]]; then
  mkdir -p "$APT_CACHE_DIR/partial"
  CACHE_ARGS=(-v "$APT_CACHE_DIR:/var/cache/apt/archives" -e KEEP_APT_ARCHIVES=1)
fi

echo "==> Running provision.sh backups test suite in ubuntu:24.04 container..."
docker run --rm -i "${CACHE_ARGS[@]}" \
  -v "$SCRIPT_DIR:/work:ro" \
  ikenga-test-backups:24.04 /work/test-backups-inside.sh

echo "==> Container test suite completed successfully."
