---
"ikenga-desktop": minor
---

Add a streamed file download route to the server (`GET /api/fs/download`). It saves a file straight from disk to the browser without loading it into memory first, follows the same access rules as opening a file (including inside a shared project), refuses folders, and only shows PDFs, common images and plain text inline.
