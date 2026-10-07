---
"ikenga-desktop": minor
---

WSL network health. Before a WSL terminal or agent launches, and when a WSL session prints a network error such as `EAI_AGAIN`, Ikenga checks whether WSL can reach the network. The check results are cached for 30 seconds, and nothing polls in the background. When the network is broken, the check names the cause, including a failed mirrored-networking setup (for example `0x8007054f`) read from the Windows event log. The affected terminal pane shows a banner, one notification is raised for each problem episode, and Settings › Engines gains a "WSL health" row. Each surface offers the fitting fix:

- **Repair DNS** rewrites `/etc/resolv.conf` and backs up the old file first.
- **Restart WSL networking** opens a single administrator prompt, then shuts WSL down and restarts the Host Network Service.
- **Switch to NAT** first explains the trade-off for LAN and Tailscale access. It backs up `.wslconfig` with a timestamp, sets `networkingMode=nat` without disturbing other lines, and keeps the file's encoding.

After a fix that restarts WSL, Ikenga reopens the WSL sessions it closed and resumes their Claude conversations.
