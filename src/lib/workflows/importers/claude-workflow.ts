import type { WorkflowEdge, WorkflowGraph, WorkflowNode } from '../graph';
import { createWorkflowGraph } from '../graph';

export interface ClaudeWorkflowImportOptions {
  id?: string;
  title?: string;
  sourcePath?: string | null;
  projectId?: string;
}

interface PhaseDefinition {
  title?: string;
  name?: string;
  id?: string;
  steps?: Array<{
    id?: string;
    title?: string;
    label?: string;
    command?: string;
    tool?: string;
  }>;
}

/**
 * Import a Claude Code workflow script (.claude/workflows/*.mjs) into a WorkflowGraph.
 * Parses `export const meta = { name, description, phases: [...] }` or heuristic step invocations.
 */
export function importClaudeWorkflow(
  scriptContent: string,
  options: ClaudeWorkflowImportOptions = {},
): WorkflowGraph {
  let title = options.title ?? 'Claude Code Workflow';
  let workflowId = options.id ?? 'claude-workflow';
  const nodes: WorkflowNode[] = [];
  const edges: WorkflowEdge[] = [];

  // 1. Extract meta object: export const meta = { ... }
  const metaStart = scriptContent.indexOf('export const meta');
  let parsedPhases: PhaseDefinition[] = [];

  if (metaStart !== -1) {
    const braceStart = scriptContent.indexOf('{', metaStart);
    if (braceStart !== -1) {
      let depth = 0;
      let braceEnd = -1;
      for (let i = braceStart; i < scriptContent.length; i++) {
        if (scriptContent[i] === '{') depth++;
        else if (scriptContent[i] === '}') {
          depth--;
          if (depth === 0) {
            braceEnd = i;
            break;
          }
        }
      }
      if (braceEnd !== -1) {
        const metaStr = scriptContent.slice(braceStart, braceEnd + 1);
        try {
          const fn = new Function(`return (${metaStr});`);
          const meta = fn() as {
            name?: string;
            title?: string;
            description?: string;
            phases?: PhaseDefinition[];
          };

          if (meta.name) workflowId = options.id ?? meta.name;
          if (meta.title) title = options.title ?? meta.title;
          else if (meta.description) title = options.title ?? meta.description;

          if (Array.isArray(meta.phases)) {
            parsedPhases = meta.phases;
          }
        } catch {
          // ignore eval error
        }
      }
    }
  }

  // 2. If meta.phases were extracted, map them
  if (parsedPhases.length > 0) {
    let prevPhaseId: string | null = null;

    parsedPhases.forEach((phase, index) => {
      const phaseId = phase.id ?? `phase-${index + 1}`;
      const phaseTitle = phase.title ?? phase.name ?? `Phase ${index + 1}`;

      nodes.push({
        id: phaseId,
        kind: 'phase',
        label: phaseTitle,
        run: null,
        source_ref: options.sourcePath ? `${options.sourcePath}#${phaseId}` : undefined,
      });

      if (prevPhaseId) {
        edges.push({
          from: prevPhaseId,
          to: phaseId,
          kind: 'depends-on',
        });
      }
      prevPhaseId = phaseId;

      // If phase contains explicit steps
      if (Array.isArray(phase.steps)) {
        let prevStepId: string | null = null;
        phase.steps.forEach((step, stepIndex) => {
          const stepId = step.id ?? `${phaseId}-step-${stepIndex + 1}`;
          const stepLabel = step.title ?? step.label ?? `Step ${stepIndex + 1}`;

          nodes.push({
            id: stepId,
            kind: 'step',
            label: stepLabel,
            run: step.command
              ? { kind: 'command', ref: step.command }
              : step.tool
                ? { kind: 'tool', ref: step.tool }
                : null,
            source_ref: options.sourcePath ? `${options.sourcePath}#${stepId}` : undefined,
          });

          // Step belongs to phase
          edges.push({
            from: phaseId,
            to: stepId,
            kind: 'depends-on',
          });

          if (prevStepId) {
            edges.push({
              from: prevStepId,
              to: stepId,
              kind: 'depends-on',
            });
          }
          prevStepId = stepId;
        });
      }
    });
  } else {
    // 3. Fallback: inspect functions or script calls (e.g. agent(), command(), etc.)
    const functionMatches = [
      ...scriptContent.matchAll(/(?:async\s+)?function\s+([a-zA-Z0-9_$]+)\s*\(/g),
    ];
    let prevStepId: string | null = null;

    functionMatches.forEach((match) => {
      const fnName = match[1];
      const stepId = `step-${fnName}`;
      nodes.push({
        id: stepId,
        kind: 'step',
        label: fnName,
        run: { kind: 'command', ref: fnName },
        source_ref: options.sourcePath ? `${options.sourcePath}#${fnName}` : undefined,
      });

      if (prevStepId) {
        edges.push({
          from: prevStepId,
          to: stepId,
          kind: 'depends-on',
        });
      }
      prevStepId = stepId;
    });

    if (nodes.length === 0) {
      // Single default step
      nodes.push({
        id: 'main',
        kind: 'step',
        label: title,
        run: { kind: 'command', ref: options.sourcePath ?? 'script.mjs' },
      });
    }
  }

  return createWorkflowGraph({
    id: workflowId,
    title,
    source: 'claude-workflow',
    sourcePath: options.sourcePath,
    scope: options.projectId
      ? { kind: 'project', project_id: options.projectId }
      : { kind: 'personal' },
    nodes,
    edges,
  });
}
