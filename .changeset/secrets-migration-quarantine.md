---
"ikenga-desktop": patch
---

One secret that fails its keychain round-trip no longer blanks the whole vault. The secrets migration now sets that single entry aside ("needs re-entry") and migrates the rest, so env-vault publication, and the jobs that depend on it, keep working. Settings → Secrets shows a "Needs re-entry" group with Enter again and Remove. A locked or unreachable keychain, or more than half the entries failing, still fails closed as before. Per-entry failures are now logged with their class (names and lengths only, never values).
