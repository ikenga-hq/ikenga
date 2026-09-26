---
"ikenga-desktop": patch
---

**Duplicate and blank surfaces from the 0.14 re-architecture.**

- **Settings** lists its nine sections once. The nav sits inside the pane (D-03), and the sidebar stays the Explorer.
- **Chi and Ngwa sidebars are no longer blank.** Chi shows Sessions and Ngwa shows the project's equipment (D-01). Clicking Ngwa now lands on `/ngwa/installed` instead of `/claude`.
- **One project switcher.** The rail-foot copy is gone; the title-row chip does the job.
- **One greeting on the Project dashboard.** The Obi canvas hides its own greeting while the daily address shows.
- **One toast per permission ask.** The terminal permission inbox no longer fires its own OS notification for asks the notification centre already records.
- **The Ngwa Store uses the shared D-07 states.** Loading shows the ember pulse, and the offline state has a Retry. Two unmounted legacy components (`PkgsSurface`, the old Ngwa facet bar) were deleted.
