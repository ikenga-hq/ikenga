---
"ikenga-desktop": patch
---

Serve hardened git branch and dirty status to shared-project members and remote browser sessions. The read runs git against a private sandbox git dir with an allowlist-sanitized config, refuses repositories that use alternate object stores or symlinks under `refs/`/`objects/`, refuses an index naming paths outside the project, and serves shallow and split-index repositories. Dirty submodules are not shown (status runs with `--ignore-submodules=all`, because a submodule's own config is untrusted).
