---
"ikenga-desktop": patch
---
An installed package's recorded version now stays in step with the version the shell shows. Hot-reloading a dev package, or starting the shell after a package's manifest changed on disk, now updates the stored install record too, so it no longer reports an old version (for example 0.6.0 while the Explorer shows 0.8.0).
