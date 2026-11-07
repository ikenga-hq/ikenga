---
"ikenga-desktop": patch
---

Release builds now run on Node 22. The 0.31.3 desktop build failed because a new skill in the pkgs repo needs a Node feature that Node 20 lacks, so 0.31.3 was never published and its changes ship in 0.31.4.
