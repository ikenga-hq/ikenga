---
"ikenga-desktop": minor
---

Claude sessions can now be launched with a role, extra system prompt text and plugin folders. A chat or terminal session given the `chi` or `pane` role starts on Claude Sonnet 5.5, and a `plan` session starts on Claude Opus 5.5, unless a model is chosen explicitly. `appendSystemPrompt` is passed to Claude Code as `--append-system-prompt`, and `pluginDirs` reaches it as `CLAUDE_CODE_PLUGIN_DIRS`. Sessions that set none of these launch exactly as before. The model ids and prices come from a copy of the `@ikenga/contract` model catalog. The built-in Claude Code engine's default model setting is now `claude-sonnet-5-5`, and stale model names in the agent editors and Mission Control are updated.
