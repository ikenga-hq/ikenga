---
"ikenga-desktop": patch
---

`provision.sh`: new `gh` LIB installs the GitHub CLI from GitHub's apt repository, with its signing keys pinned like the Postgres and gcloud repositories. Each account signs in for itself with `gh auth login` in its own terminal, and the box holds no GitHub token for anyone. The example profiles include it.
