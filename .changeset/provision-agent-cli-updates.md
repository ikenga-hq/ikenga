---
"ikenga-desktop": patch
---

Server provisioning now keeps the agent CLIs current. `provision.sh` installs a daily `ikenga-agent-cli-update.timer` that runs `npm install -g <pkg>@latest` as root for each npm-installed CLI in the profile's `AGENT_CLIS` (claude, codex, opencode, pi). They are installed system-wide, so a person's own `claude` could not self-update and showed "Auto-update failed" in every terminal. Existing servers can add it with `provision.sh install-agent-cli-updates`.
