---
"ikenga-desktop": patch
---

An outside link can now only point at a file your own account owns. Before, an account could add a system folder such as `/etc` to its file roots and then share files it did not own, like `/etc/passwd`. The server now checks who owns the file both when the link is made and every time someone opens it, and a file that fails the check gets the same "not available" answer as any other link that can't be opened.
