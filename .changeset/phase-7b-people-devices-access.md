---
"ikenga-desktop": minor
---

**Phase 7 Part B: people, devices and access.** One Ikenga can now be shared safely, with your own phone or laptop and with the people you work with.

- **Accounts on an Ikenga server (T1).** A server can hold several local accounts. Each person signs in with a username and password, and a broker runs their workspace as that person, never as anyone else. Signing out, changing a password or revoking sessions closes that person's live connections.
- **Device pairing.** Pair a phone or laptop with a short code (or its QR). Both screens show the same four-word fingerprint, and you confirm on this machine before the device is let in. **Settings → Devices** lists every paired device with where and when it was last seen.
- **Per-device capabilities.** Each device has its own level (View only, View + dispatch, Dispatch + approve, or Full). Change it or revoke the device at any time; a revoke takes effect at once. A paired device below Full opens a compact remote client: sessions, the permission inbox and a dispatch bar.
- **Permission routing.** Choose whether a permission ask may be answered on **this device only** or on any paired device, always capped by that device's level. In a shared project, asks that need the Owner go to the Owner, and asks about secrets always do.
- **Members, roles and invites.** On a T1 server, Owners and Operators invite people with **Share kola**: a single-use, expiring invite that fixes the role (Owner, Operator, Reviewer or Guest) and what is shared. **Members** lists people, roles and pending invites; **Policies** shows what each role can do. On a single machine you remain the only member.
- **Your secrets stay yours.** On a T1 server each person has their own secret store, sealed under a key the server holds, so one member cannot read another's.
- **An audit log you can check.** Sign-ins, pairings, grants, revokes, role changes, approvals and dispatches are recorded in a hash-chained log. **Settings → Audit** filters and searches it, verifies the chain, exports it and reseals it after a repair; `ikenga-server audit verify|export|reseal` does the same from the command line.
- **The People surface (D-05).** Profile, Devices, Members, Policies and Audit share one layout with a Personal / Project scope switch, a visible keyboard focus ring on every control, and light and dark modes.
