---
"ikenga-desktop": minor
---

Terminal overhaul (Waves 1-4), OpenRouter engine, and pkg-trust relay.

- Terminal: desktop PtyManager daemon proxy and session continuity (WP-01, WP-02); deep linking and navigation (WP-05, WP-07); OSC 133 semantic prompts (WP-08); live foreground CWD, rich search options and renderer hygiene (WP-182, WP-183, WP-185).
- Engines: OpenRouter HTTP engine — the shell half of WP-20.
- Iyke: pkg-trust relay endpoint for studio project access (WP-04).
- Manifest parser: accept a boolean `sqlite` capability and a `//` comment field (#179); accept an optional `description`.
- ACL: grant `pty_daemon_info` and `pty_daemon_shutdown` in allow-app-commands.
- Scripts and tests: unbreak two dead royalti-video-engine references; cover migration 0063's meetings schema and reader-pool visibility.
