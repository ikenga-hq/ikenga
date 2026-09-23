import type { WorkflowEdge, WorkflowGraph, WorkflowNode } from '../graph';
import { createWorkflowGraph } from '../graph';

export interface HooksImportOptions {
  id?: string;
  title?: string;
  sourcePath?: string | null;
  projectId?: string;
}

export interface ClaudeHookHandler {
  type?: string;
  command?: string;
}

export interface ClaudeHooksSettings {
  hooks?: Record<string, ClaudeHookHandler[] | ClaudeHookHandler>;
}

// Canonical Claude Code hook lifecycle sequence
const LIFECYCLE_SEQUENCE = [
  'SessionStart',
  'UserPromptSubmit',
  'PreToolUse',
  'PostToolUse',
  'PreCompact',
  'SessionEnd',
];

/**
 * Import Claude Code hooks configuration (.claude/settings.json or hook descriptors) into a WorkflowGraph.
 */
export function importHooks(
  config: ClaudeHooksSettings | string,
  options: HooksImportOptions = {},
): WorkflowGraph {
  let parsed: ClaudeHooksSettings;
  if (typeof config === 'string') {
    try {
      parsed = JSON.parse(config);
    } catch {
      parsed = {};
    }
  } else {
    parsed = config;
  }

  const hooksMap = parsed.hooks ?? {};
  const nodes: WorkflowNode[] = [];
  const edges: WorkflowEdge[] = [];

  const declaredEvents = Object.keys(hooksMap);
  let prevSequenceNodeId: string | null = null;

  for (const event of LIFECYCLE_SEQUENCE) {
    if (declaredEvents.includes(event)) {
      const handlerData = hooksMap[event];
      const handlers = Array.isArray(handlerData)
        ? handlerData
        : handlerData
          ? [handlerData]
          : [];

      const nodeId = `hook-${event}`;
      const commandRef = handlers.map((h) => h.command).filter(Boolean).join('; ');

      nodes.push({
        id: nodeId,
        kind: 'step',
        label: `Hook: ${event}`,
        run: {
          kind: 'command',
          ref: commandRef || undefined,
        },
        source_ref: options.sourcePath ? `${options.sourcePath}#hooks.${event}` : undefined,
      });

      if (prevSequenceNodeId) {
        edges.push({
          from: prevSequenceNodeId,
          to: nodeId,
          kind: 'triggers',
        });
      }
      prevSequenceNodeId = nodeId;
    }
  }

  // Handle other events not in the standard sequence (e.g. PermissionRequest, Notification)
  for (const event of declaredEvents) {
    if (!LIFECYCLE_SEQUENCE.includes(event)) {
      const handlerData = hooksMap[event];
      const handlers = Array.isArray(handlerData)
        ? handlerData
        : handlerData
          ? [handlerData]
          : [];

      const nodeId = `hook-${event}`;
      const commandRef = handlers.map((h) => h.command).filter(Boolean).join('; ');

      nodes.push({
        id: nodeId,
        kind: 'step',
        label: `Hook: ${event}`,
        run: {
          kind: 'command',
          ref: commandRef || undefined,
        },
        source_ref: options.sourcePath ? `${options.sourcePath}#hooks.${event}` : undefined,
      });
    }
  }

  return createWorkflowGraph({
    id: options.id ?? 'hooks:claude-code',
    title: options.title ?? 'Claude Code Lifecycle Hooks',
    source: 'hooks',
    sourcePath: options.sourcePath,
    scope: options.projectId
      ? { kind: 'project', project_id: options.projectId }
      : { kind: 'personal' },
    nodes,
    edges,
  });
}
