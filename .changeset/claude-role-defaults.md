---
"ikenga-desktop": minor
---

Claude sessions now launch on the model for their role when no model is chosen. Claude terminals and Claude seats are `pane` sessions and start on Claude Sonnet 5.5. A new "Claude terminal (plan)" entry in the new-tab menu starts a `plan` session on Claude Opus 5.5.

**Behaviour change:** Chi runs on Claude Code (`iyke chi run`, pinned and persistent runs) used to start on Claude Code's own default model. They now start on Claude Sonnet 5.5, the catalog default for Chi. A model passed with `--model` still wins, and other engines are unchanged.
