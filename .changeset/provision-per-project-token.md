---
'ikenga-desktop': patch
---

Server provisioner: a project can now have its own git deploy token. The new optional profile key `PROJECTS_TOKEN_SECRETS=("project=SECRET_NAME" ...)` names a `[root]` secret for a listed project, and every other project keeps using `PROJECTS_TOKEN_SECRET` (a profile without the new key behaves exactly as before). This is needed because a GitHub fine-grained token belongs to one owner, so an organisation's repos and a personal repo cannot share one. Each token stays in its own 0600 file delivered through `GIT_ASKPASS` (never argv, the URL or a repo config), a project with no token is fetched with no credential at all, and a typo'd or duplicate project name, an unusable secret name, or a secret missing or repeated in `SECRETS_FILE` stops the run before anything is fetched.
