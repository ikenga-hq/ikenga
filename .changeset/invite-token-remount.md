---
"ikenga-desktop": patch
---

Accepting a share invite while already signed in no longer says "This invite link is incomplete". The invite page drops the token from the address bar after reading it, and a signed-in visit mounts the page a second time; that mount now reuses the token read by the first.
