import type { WorkflowEdge, WorkflowGraph, WorkflowNode } from '../graph';
import { createWorkflowGraph } from '../graph';

export interface GroundworkImportOptions {
  id?: string;
  title?: string;
  sourcePath?: string | null;
  projectId?: string;
}

interface GroundworkWave {
  id: string;
  title: string;
  wps: Array<{
    id: string;
    title: string;
    brief?: string;
    tier?: string;
    dependsOn?: string[];
  }>;
}

/**
 * Import a groundwork 09-orchestration.md or orchestrate.workflow.js into a WorkflowGraph.
 */
export function importGroundwork(
  content: string,
  options: GroundworkImportOptions = {},
): WorkflowGraph {
  // Check if content is orchestrate.workflow.js or markdown
  if (content.includes('const WAVES =') || content.includes('export const meta')) {
    return importFromWorkflowJs(content, options);
  }
  return importFromMarkdown(content, options);
}

function importFromWorkflowJs(
  content: string,
  options: GroundworkImportOptions,
): WorkflowGraph {
  // Extract plan title / meta
  const metaMatch = content.match(/export const meta\s*=\s*(\{[\s\S]*?\n\})/);
  let title = options.title ?? 'Groundwork Plan';
  let planId = options.id ?? 'groundwork-plan';

  if (metaMatch) {
    try {
      // Evaluate meta object safely with Function constructor
      const fn = new Function(`return (${metaMatch[1]});`);
      const meta = fn();
      if (meta.name) planId = options.id ?? meta.name;
      if (meta.description) title = options.title ?? meta.description;
    } catch {
      // fallback
    }
  }

  // Extract WAVES array
  const waves: GroundworkWave[] = [];
  const wavesStart = content.indexOf('const WAVES');

  if (wavesStart !== -1) {
    const bracketStart = content.indexOf('[', wavesStart);
    if (bracketStart !== -1) {
      let depth = 0;
      let bracketEnd = -1;
      for (let i = bracketStart; i < content.length; i++) {
        if (content[i] === '[') depth++;
        else if (content[i] === ']') {
          depth--;
          if (depth === 0) {
            bracketEnd = i;
            break;
          }
        }
      }

      if (bracketEnd !== -1) {
        try {
          const arrayStr = content.slice(bracketStart, bracketEnd + 1);
          const fn = new Function(`return (${arrayStr});`);
          const rawWaves = fn() as Array<{
            title: string;
            gate: string | null;
            wps?: Array<{ id: string; title: string; brief?: string; tier?: string }>;
          }>;

      for (let i = 0; i < rawWaves.length; i++) {
        const rawWave = rawWaves[i];
        const waveId = `wave-${i + 1}`;
        const wps = (rawWave.wps ?? []).map((wp) => ({
          id: wp.id,
          title: wp.title,
          brief: wp.brief,
          tier: wp.tier,
          dependsOn: extractDependsOn(wp.brief ?? ''),
        }));
        waves.push({
          id: waveId,
          title: rawWave.title,
          wps,
        });
      }
        } catch {
          // fallback to markdown parsing if JS eval fails
          return importFromMarkdown(content, options);
        }
      }
    }
  }

  return buildGraphFromWaves(planId, title, waves, options.sourcePath, options.projectId);
}

function importFromMarkdown(
  markdown: string,
  options: GroundworkImportOptions,
): WorkflowGraph {
  // Extract title
  const h1Match = markdown.match(/^#\s+(.+)$/m);
  const title = options.title ?? (h1Match ? h1Match[1].trim() : 'Groundwork Plan');
  const planId = options.id ?? 'groundwork-plan';

  const lines = markdown.split(/\r?\n/);
  const waves: GroundworkWave[] = [];
  let currentWave: GroundworkWave | null = null;
  let currentWp: { id: string; title: string; briefLines: string[] } | null = null;

  for (const line of lines) {
    const waveMatch = line.match(/^##\s+(?:Wave\s+)?(.+)$/i);
    if (waveMatch) {
      if (currentWp && currentWave) {
        const brief = currentWp.briefLines.join('\n');
        currentWave.wps.push({
          id: currentWp.id,
          title: currentWp.title,
          brief,
          dependsOn: extractDependsOn(brief),
        });
        currentWp = null;
      }
      const waveTitle = line.replace(/^##\s+/, '').trim();
      const waveId = `wave-${waves.length + 1}`;
      currentWave = { id: waveId, title: waveTitle, wps: [] };
      waves.push(currentWave);
      continue;
    }

    const wpMatch = line.match(/^###\s+(WP-[\dA-Za-z]+)\s*(?:[—–-]\s*(.+))?$/);
    if (wpMatch) {
      if (currentWp && currentWave) {
        const brief = currentWp.briefLines.join('\n');
        currentWave.wps.push({
          id: currentWp.id,
          title: currentWp.title,
          brief,
          dependsOn: extractDependsOn(brief),
        });
      }
      if (!currentWave) {
        currentWave = { id: 'wave-1', title: 'Default Wave', wps: [] };
        waves.push(currentWave);
      }
      currentWp = {
        id: wpMatch[1],
        title: wpMatch[2]?.trim() ?? wpMatch[1],
        briefLines: [line],
      };
      continue;
    }

    if (currentWp) {
      currentWp.briefLines.push(line);
    }
  }

  if (currentWp && currentWave) {
    const brief = currentWp.briefLines.join('\n');
    currentWave.wps.push({
      id: currentWp.id,
      title: currentWp.title,
      brief,
      dependsOn: extractDependsOn(brief),
    });
  }

  return buildGraphFromWaves(planId, title, waves, options.sourcePath, options.projectId);
}

function extractDependsOn(text: string): string[] {
  const match = text.match(/DEPENDS-ON[*\s]*:[*\s]*([^\n]+)/i);
  if (!match) return [];
  const deps: string[] = [];
  const wpMatches = match[1].matchAll(/WP-[\dA-Za-z]+/g);
  for (const m of wpMatches) {
    deps.push(m[0]);
  }
  return deps;
}

function buildGraphFromWaves(
  id: string,
  title: string,
  waves: GroundworkWave[],
  sourcePath?: string | null,
  projectId?: string,
): WorkflowGraph {
  const nodes: WorkflowNode[] = [];
  const edges: WorkflowEdge[] = [];
  const allWpIds = new Set<string>();

  for (const wave of waves) {
    // Add phase node
    nodes.push({
      id: wave.id,
      kind: 'phase',
      label: wave.title,
      run: null,
      source_ref: sourcePath ? `${sourcePath}#${wave.id}` : undefined,
    });

    for (const wp of wave.wps) {
      allWpIds.add(wp.id);
      nodes.push({
        id: wp.id,
        kind: 'step',
        label: `${wp.id}: ${wp.title}`,
        run: {
          kind: 'dispatch',
          ref: wp.brief?.slice(0, 300),
          engine_id: wp.tier,
        },
        source_ref: sourcePath ? `${sourcePath}#${wp.id}` : undefined,
      });

      // Edge from Phase to WP
      edges.push({
        from: wave.id,
        to: wp.id,
        kind: 'depends-on',
      });
    }
  }

  // Inter-WP dependencies
  for (const wave of waves) {
    for (const wp of wave.wps) {
      for (const depId of wp.dependsOn ?? []) {
        if (allWpIds.has(depId) && depId !== wp.id) {
          edges.push({
            from: depId,
            to: wp.id,
            kind: 'depends-on',
          });
        }
      }
    }
  }

  // Inter-wave dependencies: each wave depends on the previous wave
  for (let i = 1; i < waves.length; i++) {
    edges.push({
      from: waves[i - 1].id,
      to: waves[i].id,
      kind: 'depends-on',
    });
  }

  return createWorkflowGraph({
    id,
    title,
    source: 'groundwork',
    sourcePath,
    scope: projectId ? { kind: 'project', project_id: projectId } : { kind: 'personal' },
    nodes,
    edges,
  });
}
