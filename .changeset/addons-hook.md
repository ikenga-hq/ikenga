---
"ikenga-desktop": patch
---

Server: the provisioner can now run your own scripts after a provision, an account sync or an update. List them as `ADDONS` in the profile; each must be a root-owned script that nobody else can write, or it is refused. A script that fails is named in the output with its exit code, and an update whose script fails exits with its own code (5) and a clear message instead of being reported as "did not start".
