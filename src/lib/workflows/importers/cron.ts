import type { WorkflowEdge, WorkflowGraph, WorkflowNode } from '../graph';
import { createWorkflowGraph } from '../graph';

export interface CronImportOptions {
  pkgId?: string;
  id?: string;
  title?: string;
  sourcePath?: string | null;
  projectId?: string;
}

export interface ManifestCronEntry {
  id: string;
  expr: string;
  handler: string;
  env_from_settings?: string[];
}

export interface PkgManifestWithCron {
  id?: string;
  name?: string;
  cron?: ManifestCronEntry[];
}

/**
 * Import cron entries from a manifest or cron array into a WorkflowGraph.
 * Maps each schedule to a `schedule` node triggering a `step` node for the handler.
 */
export function importCron(
  input: ManifestCronEntry[] | PkgManifestWithCron | string,
  options: CronImportOptions = {},
): WorkflowGraph {
  let pkgId = options.pkgId ?? 'cron';
  let title = options.title ?? 'Cron Schedules';
  let cronEntries: ManifestCronEntry[] = [];

  if (typeof input === 'string') {
    try {
      const parsed = JSON.parse(input);
      if (Array.isArray(parsed)) {
        cronEntries = parsed;
      } else if (parsed && typeof parsed === 'object') {
        if (parsed.id) pkgId = parsed.id;
        if (parsed.name) title = `${parsed.name} Schedules`;
        cronEntries = parsed.cron ?? [];
      }
    } catch {
      cronEntries = [];
    }
  } else if (Array.isArray(input)) {
    cronEntries = input;
  } else if (input && typeof input === 'object') {
    if (input.id) pkgId = input.id;
    if (input.name) title = `${input.name} Schedules`;
    cronEntries = input.cron ?? [];
  }

  const nodes: WorkflowNode[] = [];
  const edges: WorkflowEdge[] = [];

  for (const entry of cronEntries) {
    const schedNodeId = `sched-${pkgId}-${entry.id}`;
    const stepNodeId = `step-${pkgId}-${entry.id}`;

    // 1. Schedule trigger node
    nodes.push({
      id: schedNodeId,
      kind: 'schedule',
      label: `Every ${entry.expr} (${entry.id})`,
      run: {
        kind: 'schedule',
        ref: entry.expr,
      },
      source_ref: options.sourcePath ? `${options.sourcePath}#/cron/${entry.id}` : undefined,
    });

    // 2. Action step node
    nodes.push({
      id: stepNodeId,
      kind: 'step',
      label: `Run ${entry.id}`,
      run: {
        kind: 'command',
        ref: entry.handler,
      },
      source_ref: options.sourcePath ? `${options.sourcePath}#/cron/${entry.id}` : undefined,
    });

    // Schedule triggers the step
    edges.push({
      from: schedNodeId,
      to: stepNodeId,
      kind: 'triggers',
    });
  }

  return createWorkflowGraph({
    id: options.id ?? `cron:${pkgId}`,
    title,
    source: 'cron',
    sourcePath: options.sourcePath,
    scope: options.projectId
      ? { kind: 'project', project_id: options.projectId }
      : { kind: 'personal' },
    nodes,
    edges,
  });
}
