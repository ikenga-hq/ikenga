---
"ikenga-desktop": patch
---

Chi seats now work in a browser connected to `ikenga-server`. The seat rail no longer shows a permanent error, and you can create, rename, clear, remove, hold and release seats. Dispatching to a seat resumes or fills it with a headless Chi run on the server, and a text queued behind a running turn is sent when that turn ends.

Under multi-user (T1) servers, each person sees and changes only their own seats. Things the server can't do are labelled as such rather than failing silently:

- Terminal sessions can't be seated on a server. A seat there runs headless Chi runs instead.
- `openrouter` seats need the desktop app.
- An engine missing from the server reads "not installed on this server".

The seat roster also refreshes after your own changes without needing live events.
