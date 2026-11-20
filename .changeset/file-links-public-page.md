---
"ikenga-desktop": minor
---

When a server has outside file links turned on (`--file-links` / `IKENGA_FILE_LINKS`), a link now opens a simple page at `/s/…` on the server's own address: the file's name and size, a preview for PDFs, images and plain text, and a Download button. A link with a passcode asks for it first and remembers it for an hour. Expired, revoked or unknown links all show the same "This link isn't available" page, repeated wrong passcodes lock the link for an hour, and an installed app no longer catches `/s/` links. With the setting off, every `/s/` address shows the "not available" page. Making links from the app comes in a later release.
