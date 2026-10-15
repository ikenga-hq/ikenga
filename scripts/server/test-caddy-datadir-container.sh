#!/bin/bash
# provision.sh: Caddy install fallback (caddy_from_repo / caddy_from_deb /
# install_caddy) and the T1 data-dir mode (ensure_t1_data_dir).
#
# The functions are extracted from provision.sh by name and run for real as root
# in a plain ubuntu:24.04 container with NO network. curl, gpg and apt-get are
# shims, so what is proved is the script's own logic (ordering, cleanup, the
# pinned sha512 check, dry-run, exit paths), NOT that Cloudsmith or GitHub serve
# what we expect. The real .deb download and its pinned hashes are checked
# separately (see the PR notes). The data-dir checks use real chmod/chown/stat.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROVISION_DIR="$(cd "${PROVISION_DIR:-$SCRIPT_DIR}" && pwd)"
chmod +x "$SCRIPT_DIR/test-caddy-datadir-inside.sh"

echo "==> Running Caddy fallback + T1 data dir test suite in ubuntu:24.04 (no network)..."
docker run --rm -i --network none \
  -v "$SCRIPT_DIR:/tests:ro" -v "$PROVISION_DIR:/work:ro" \
  ubuntu:24.04 /tests/test-caddy-datadir-inside.sh
echo "==> Container test suite completed successfully."
