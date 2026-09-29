---
"ikenga-desktop": minor
---

**The Ngwa Store works, and matches its design (D-02).**

- **Layout:** the install sheet no longer squeezes the result list away (#324), and the **Install ▾** scope menu is a proper popover that closes on Escape or a click outside.
- **Kinds are right** (#325). The `@ikenga/mcp-*` servers show as **tool**, and embedded and windowed pkgs as **app**. Installed pkgs use the kernel's own kind, so every registry entry now lands under a Kind chip.
- **Full install sheet** (#326):
  - an overview;
  - the **requires closure** you are about to pull in;
  - **permissions** with one consent checkbox per group; Install stays disabled until every box is ticked;
  - trust and provenance;
  - a sticky "Install to <project>" foot.

  Rows show what each pkg pulls in and asks for, and the manifest is fetched only for the row you open.
- **Install and Update actually run.** The Store now installs through the same signed-plan registry path as the other install surfaces:
  - one install per closure step;
  - **personal** maps to the workspace scope, **project** to the active project;
  - Update installs the latest version in the pkg's current scope, and holds back if the new version asks for more permissions;
  - lists refresh afterwards.
