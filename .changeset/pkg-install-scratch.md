---
"ikenga-desktop": patch
---
Package installs no longer leave scratch files behind in the pkgs folder. Each registry install now names its downloaded tarball after the full package id (previously every `com.ikenga.*` install shared one `.staging-com.ikenga.tgz`), and on startup the shell cleans up staging folders, tarballs and backups left by an install that was interrupted by a crash or restart, restoring the previous version of a package if the update died before the new one was put in place.
