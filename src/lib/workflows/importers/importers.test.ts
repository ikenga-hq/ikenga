import { describe, it, expect } from 'vitest';
import * as fs from 'node:fs';
import * as path from 'node:path';

import { importGroundwork } from './groundwork';
import { importClaudeWorkflow } from './claude-workflow';
import { importHooks } from './hooks';
import { importCron } from './cron';
import { validateWorkflowDag, getTopologicalOrder } from '../graph';

describe('Workflow Importers', () => {
  describe('groundwork importer', () => {
    it('round-trips orchestrate.workflow.js into a valid WorkflowGraph', () => {
      // Find orchestrate.workflow.js in the plans folder
      const workflowPath = path.resolve(
        process.cwd(),
        '../plans/shell-ux-rearchitecture/artifact/orchestrate.workflow.js',
      );

      let content: string;
      if (fs.existsSync(workflowPath)) {
        content = fs.readFileSync(workflowPath, 'utf-8');
      } else {
        // Fallback inline sample representing the exact shape
        content = `
export const meta = {
  name: 'groundwork-orchestrate-shell-ux-rearchitecture',
  description: 'Run the Shell UX rearchitecture — Phase 4 build wave-by-wave',
  phases: [{title:"Wave 9b"},{title:"Wave 9c · G-WORKFLOW-ADDR"},{title:"Wave 9d · Phase 4 close"}],
};
const WAVES = [
  {
    "title": "Wave 9b",
    "gate": null,
    "wps": [
      {
        "id": "WP-29",
        "title": "ikenga-pkgs ui.nav->ui.views PR",
        "tier": "haiku",
        "brief": "### WP-29\\n- **DEPENDS-ON**: none\\n- **GOAL**: Migrated views"
      },
      {
        "id": "WP-30",
        "title": "Registry backfill",
        "tier": "sonnet",
        "brief": "### WP-30\\n- **DEPENDS-ON**: WP-29\\n- **GOAL**: Verified catalog"
      }
    ]
  },
  {
    "title": "Wave 9c",
    "gate": "G-WORKFLOW-ADDR",
    "wps": [
      {
        "id": "WP-31",
        "title": "Workflow importers",
        "tier": "opus",
        "brief": "### WP-31\\n- **DEPENDS-ON**: WP-30\\n- **GOAL**: Importers"
      }
    ]
  }
];
`;
      }

      const graph = importGroundwork(content, {
        sourcePath: 'plans/shell-ux-rearchitecture/artifact/orchestrate.workflow.js',
      });

      expect(graph.source).toBe('groundwork');
      expect(graph.nodes.length).toBeGreaterThan(0);
      expect(graph.edges.length).toBeGreaterThan(0);

      // Verify phase nodes and step nodes exist
      const phases = graph.nodes.filter((n) => n.kind === 'phase');
      const steps = graph.nodes.filter((n) => n.kind === 'step');
      expect(phases.length).toBeGreaterThan(0);
      expect(steps.length).toBeGreaterThan(0);

      // Verify DAG is valid
      const dagCheck = validateWorkflowDag(graph);
      expect(dagCheck.valid).toBe(true);

      // Verify topological order resolves
      const topoOrder = getTopologicalOrder(graph);
      expect(topoOrder.length).toBe(graph.nodes.length);
    });

    it('round-trips 09-orchestration.md markdown into a valid WorkflowGraph', () => {
      const mdContent = `
# Shell UX Rearchitecture Plan

## Wave 9b
### WP-29 — Manifest Migration
- **DEPENDS-ON**: none
- **GOAL**: Move apps to views

### WP-30 — Registry Scan
- **DEPENDS-ON**: WP-29
- **GOAL**: Zero nav-only

## Wave 9c
### WP-31 — Workflows
- **DEPENDS-ON**: WP-30
- **GOAL**: Importers and flow renderer
`;

      const graph = importGroundwork(mdContent, {
        sourcePath: 'plans/shell-ux-rearchitecture/09-orchestration.md',
      });

      expect(graph.source).toBe('groundwork');
      const step29 = graph.nodes.find((n) => n.id === 'WP-29');
      const step30 = graph.nodes.find((n) => n.id === 'WP-30');
      const step31 = graph.nodes.find((n) => n.id === 'WP-31');

      expect(step29).toBeDefined();
      expect(step30).toBeDefined();
      expect(step31).toBeDefined();

      // Check depends-on edge from WP-29 to WP-30
      const dep29to30 = graph.edges.find(
        (e) => e.from === 'WP-29' && e.to === 'WP-30' && e.kind === 'depends-on',
      );
      expect(dep29to30).toBeDefined();

      const dagCheck = validateWorkflowDag(graph);
      expect(dagCheck.valid).toBe(true);
    });
  });

  describe('claude-workflow importer', () => {
    it('round-trips Claude Code workflow script with meta.phases', () => {
      const script = `
export const meta = {
  name: 'code-review-workflow',
  title: 'Automated Code Review',
  description: 'Inspects PR, runs lint and security checks, posts review comments',
  phases: [
    {
      id: 'phase-lint',
      title: 'Lint & Format',
      steps: [
        { id: 'step-lint', title: 'Run Biome', command: 'biome lint .' },
        { id: 'step-typecheck', title: 'Typecheck', command: 'tsc --noEmit' },
      ],
    },
    {
      id: 'phase-test',
      title: 'Run Tests',
      steps: [
        { id: 'step-unit', title: 'Vitest', command: 'vitest run' },
      ],
    },
  ],
};
`;

      const graph = importClaudeWorkflow(script, {
        sourcePath: '.claude/workflows/review.mjs',
      });

      expect(graph.id).toBe('code-review-workflow');
      expect(graph.title).toBe('Automated Code Review');
      expect(graph.source).toBe('claude-workflow');

      const phaseNodes = graph.nodes.filter((n) => n.kind === 'phase');
      const stepNodes = graph.nodes.filter((n) => n.kind === 'step');

      expect(phaseNodes.length).toBe(2);
      expect(stepNodes.length).toBe(3);

      const lintStep = stepNodes.find((n) => n.id === 'step-lint');
      expect(lintStep?.run?.kind).toBe('command');
      expect(lintStep?.run?.ref).toBe('biome lint .');

      const dagCheck = validateWorkflowDag(graph);
      expect(dagCheck.valid).toBe(true);
    });
  });

  describe('hooks importer', () => {
    it('round-trips Claude Code hooks settings into lifecycle graph', () => {
      const hooksSettings = {
        hooks: {
          SessionStart: [{ type: 'command', command: 'curl http://127.0.0.1:4000/start' }],
          UserPromptSubmit: [{ type: 'command', command: 'curl http://127.0.0.1:4000/prompt' }],
          PreToolUse: [{ type: 'command', command: 'curl http://127.0.0.1:4000/pre-tool' }],
          PostToolUse: [{ type: 'command', command: 'curl http://127.0.0.1:4000/post-tool' }],
          PermissionRequest: [{ type: 'command', command: 'curl http://127.0.0.1:4000/perm' }],
        },
      };

      const graph = importHooks(hooksSettings, {
        sourcePath: '.claude/settings.json',
      });

      expect(graph.source).toBe('hooks');
      expect(graph.nodes.length).toBe(5);

      const preTool = graph.nodes.find((n) => n.id === 'hook-PreToolUse');
      expect(preTool).toBeDefined();
      expect(preTool?.run?.ref).toContain('pre-tool');

      // Sequence triggers: UserPromptSubmit -> PreToolUse -> PostToolUse
      const triggerEdge = graph.edges.find(
        (e) => e.from === 'hook-PreToolUse' && e.to === 'hook-PostToolUse' && e.kind === 'triggers',
      );
      expect(triggerEdge).toBeDefined();

      const dagCheck = validateWorkflowDag(graph);
      expect(dagCheck.valid).toBe(true);
    });
  });

  describe('cron importer', () => {
    it('round-trips cron declarations into schedule and step nodes', () => {
      const cronManifest = {
        id: 'com.ikenga.finance',
        name: 'Finance',
        cron: [
          {
            id: 'daily-sync',
            expr: '0 0 12 * * *',
            handler: 'sidecar:pa-finance sync',
          },
          {
            id: 'hourly-quote',
            expr: '0 0 * * * *',
            handler: 'event:finance.quote',
          },
        ],
      };

      const graph = importCron(cronManifest, {
        sourcePath: 'packages/apps/finance/manifest.json',
      });

      expect(graph.id).toBe('cron:com.ikenga.finance');
      expect(graph.title).toBe('Finance Schedules');
      expect(graph.source).toBe('cron');

      const schedNodes = graph.nodes.filter((n) => n.kind === 'schedule');
      const stepNodes = graph.nodes.filter((n) => n.kind === 'step');

      expect(schedNodes.length).toBe(2);
      expect(stepNodes.length).toBe(2);

      const syncSched = schedNodes.find((n) => n.id === 'sched-com.ikenga.finance-daily-sync');
      const syncStep = stepNodes.find((n) => n.id === 'step-com.ikenga.finance-daily-sync');

      expect(syncSched?.run?.kind).toBe('schedule');
      expect(syncSched?.run?.ref).toBe('0 0 12 * * *');

      expect(syncStep?.run?.kind).toBe('command');
      expect(syncStep?.run?.ref).toBe('sidecar:pa-finance sync');

      // Trigger edge connects sched to step
      const triggerEdge = graph.edges.find(
        (e) => e.from === syncSched?.id && e.to === syncStep?.id && e.kind === 'triggers',
      );
      expect(triggerEdge).toBeDefined();

      const dagCheck = validateWorkflowDag(graph);
      expect(dagCheck.valid).toBe(true);
    });
  });
});
