---
"ikenga-desktop": patch
---

Server releases: `min_upgrade_from` defaults to 0.19.4 (the first release with server tarballs) instead of the release itself, which blocked every `provision.sh upgrade`; `provision.sh` refuses to install an older `VERSION` over a newer server unless `--allow-downgrade` is passed.
