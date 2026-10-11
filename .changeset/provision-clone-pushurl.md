---
"ikenga-desktop": patch
---

`provision.sh sync-accounts`: an account's project clone now pushes to the project's upstream, not to the read-only local mirror, so `git push` from a box terminal works with the account's own `gh` login. New clones get the push URL at creation. Existing clones are repointed only while their push URL still equals the mirror.
