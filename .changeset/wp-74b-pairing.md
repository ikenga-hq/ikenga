---
"ikenga-desktop": patch
---

G-ACCESS WP-74b: device pairing and the remote client. A short code (10-minute expiry, QR included) drives a SPAKE2 handshake (Rust `spake2 =0.4.0` on the host, a `@noble/curves` port in the browser, shared test vectors); both sides show a 4-word fingerprint phrase (EFF Large Wordlist) and the host confirms on a full-window `pair-confirm` before a per-device grant is issued as an HttpOnly cookie. Attempts are throttled per address and per host. Settings › Devices lists paired devices with capability tiers and immediate revoke; a paired device below `full` boots into the 390 px `/remote` client (sessions, permission inbox, dispatch bar); the re-auth overlay gains "Pair this device".
