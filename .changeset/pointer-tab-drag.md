---
"ikenga-desktop": patch
---

Fix tab drag-to-split, tab reorder, dock drags and pin reordering on Windows. WebView2 blocks HTML5 drag events while Tauri's native file-drop handler is enabled, so these drags now run on pointer events instead; dropping files from the OS onto terminals keeps working on every platform.
