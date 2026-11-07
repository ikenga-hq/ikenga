---
"ikenga-desktop": patch
---

In a browser session, the viewer now says why it can't open a relative path, instead of showing a raw "path does not exist" error. A relative path is resolved on the server against its own working directory, which in a shared project is the owner's home, so the file was never where the reader expected.
