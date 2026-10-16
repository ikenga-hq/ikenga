---
'ikenga-desktop': patch
---

Server: the release tarball now ships `chi-runner` next to `ikenga-server` (built from the public iyke-cli repo at a pinned commit, for both amd64 and arm64). Until now the headless server could not find it, so persistent Chi runs quietly fell back to in-process runs that died with every restart ("chi-runner not found"). With the tarball carrying it, a persistent Chi run survives a server restart or upgrade. Upgrading with `provision.sh upgrade` installs the new `chi-runner`, keeps the old one as `chi-runner.prev-<version>`, and puts it back if the upgrade rolls back; upgrading to an older release that has no `chi-runner` still works.
