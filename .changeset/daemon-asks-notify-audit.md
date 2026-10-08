---
"ikenga-desktop": patch
---

Permission asks from a terminal on a server now show up where the desktop's do. When a Claude terminal on an Ikenga server holds a tool call for approval, the server records the same notification the desktop does, so the bell, the Companion cards, the home "Waiting on you" tile and Web Push all see it, and answering Allow or Deny from any of them in a browser answers that very request, once. The notification closes itself when the ask is answered, times out (denied), its terminal exits, or the hook gives up, so a dead ask is never left looking pending. On a shared server each person's asks live in their own account: nobody else sees or can answer them, and a project member can only answer asks that belong to the project they were invited to.

Each of those outcomes is also recorded in the access audit log, best effort: who allowed or denied the ask, or that the server denied it on a timeout, or that the terminal ended or the hook gave up with the ask still waiting. A decision that did not take effect is never recorded as one. On a multi-account server each account's process hands its audit records to the server's main process, which writes them; if that process stops before the hand-off, that one record is lost.
