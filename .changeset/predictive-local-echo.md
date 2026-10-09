---
"ikenga-desktop": minor
---

Browser terminals against a remote `ikenga-server` show typed characters immediately (predictive local echo, mosh-style), drawn dim and underlined until the server's echo confirms them, and rolled back cleanly when it doesn't. Never predicted: echo-off prompts, full-screen apps (Claude Code's input box excepted), pastes, control and escape keys, or after recent mispredictions. Auto turns it on above 80 ms measured round trip; Settings → Engines → Terminal & shells has Auto / Always / Off. Browser keystrokes now travel over the terminal's WebSocket, in order, instead of one HTTP request per key, which a jittery link could reorder. The desktop app is unchanged.
