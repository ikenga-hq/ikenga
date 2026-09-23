import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';

import { importGroundwork } from './groundwork';
import { importClaudeWorkflow } from './claude-workflow';
import { importHooks } from './hooks';
import { importCron } from './cron';
import { validateWorkflowDag, getTopologicalOrder } from '../graph';
import { WorkflowImportError } from '../import-error';

/**
 * WP-31 DoD: each importer round-trips ONE REAL source. The fixtures under
 * `../__fixtures__/` are verbatim copies of real files in this workspace (see
 * that folder's README for provenance). `readFixture` throws when one is
 * missing — deliberately no fallback to a synthetic literal, which is what the
 * Round-29 review flagged.
 */
const FIXTURE_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../__fixtures__');

function readFixture(name: string): string {
  const p = path.join(FIXTURE_DIR, name);
  if (!fs.existsSync(p)) {
    throw new Error(
      `missing required fixture ${p} — the importer tests round-trip REAL sources and must not ` +
        'fall back to an inline sample (WP-31 DoD). Restore it from __fixtures__/README.md.',
    );
  }
  return fs.readFileSync(p, 'utf-8');
}

const PWNED_FLAGS = [
  '__IMPORTER_PWNED_TOP__',
  '__IMPORTER_PWNED_BOTTOM__',
  '__IMPORTER_PWNED_COMPUTED__',
  '__IMPORTER_PWNED_IIFE__',
] as const;

function expectNothingExecuted() {
  for (const flag of PWNED_FLAGS) {
    expect((globalThis as Record<string, unknown>)[flag], `${flag} was set`).toBeUndefined();
  }
}

afterEach(() => {
  for (const flag of PWNED_FLAGS) {
    delete (globalThis as Record<string, unknown>)[flag];
  }
});

describe('Workflow Importers', () => {
  describe('groundwork importer', () => {
    it('round-trips the real orchestrate.workflow.js into a valid WorkflowGraph', () => {
      const content = readFixture('orchestrate.workflow.js');

      const graph = importGroundwork(content, {
        sourcePath: 'plans/shell-ux-rearchitecture/artifact/orchestrate.workflow.js',
      });

      expect(graph.source).toBe('groundwork');
      // meta.name / meta.description are read out of the real file.
      expect(graph.id).toBe('groundwork-orchestrate-shell-ux-rearchitecture');
      expect(graph.title).toContain('Shell UX rearchitecture');

      const phases = graph.nodes.filter((n) => n.kind === 'phase');
      const steps = graph.nodes.filter((n) => n.kind === 'step');
      expect(phases.length).toBeGreaterThan(0);
      expect(steps.length).toBeGreaterThan(0);

      // The real plan's waves and WPs are present.
      expect(steps.map((n) => n.id)).toContain('WP-31');
      // …and the real DEPENDS-ON lines became edges. WP-30's brief opens with
      // "**DEPENDS-ON**: WP-29 published"; WP-31's names WPs outside this
      // plan's waves (WP-28 / WP-21b), which are correctly not edged.
      expect(
        graph.edges.some((e) => e.from === 'WP-29' && e.to === 'WP-30' && e.kind === 'depends-on'),
      ).toBe(true);

      expect(validateWorkflowDag(graph).valid).toBe(true);
      expect(getTopologicalOrder(graph).length).toBe(graph.nodes.length);
    });

    it('round-trips the real 09-orchestration.md into a valid WorkflowGraph', () => {
      const graph = importGroundwork(readFixture('09-orchestration.md'), {
        sourcePath: 'plans/shell-ux-rearchitecture/09-orchestration.md',
      });

      expect(graph.source).toBe('groundwork');
      const steps = graph.nodes.filter((n) => n.kind === 'step');
      expect(steps.length).toBeGreaterThan(0);
      expect(steps.map((n) => n.id)).toContain('WP-31');
      expect(graph.nodes.filter((n) => n.kind === 'phase').length).toBeGreaterThan(0);
      expect(validateWorkflowDag(graph).valid).toBe(true);
    });

    // ── security: parse, never evaluate (Round 29) ─────────────────────────
    it('parses a hostile workflow script without executing any of it', () => {
      const graph = importGroundwork(readFixture('hostile-waves.workflow.js'), {
        sourcePath: '__fixtures__/hostile-waves.workflow.js',
      });

      expectNothingExecuted();

      // The JSON literal still round-trips, brackets-inside-strings and all.
      expect(graph.id).toBe('hostile-plan');
      const steps = graph.nodes.filter((n) => n.kind === 'step');
      expect(steps.map((n) => n.id)).toEqual(['WP-01', 'WP-02']);
      expect(
        graph.edges.some((e) => e.from === 'WP-01' && e.to === 'WP-02' && e.kind === 'depends-on'),
      ).toBe(true);
    });

    it('refuses a non-JSON WAVES literal with a typed error instead of evaluating it', () => {
      const content = readFixture('hostile-waves-computed.workflow.js');

      let caught: unknown;
      try {
        importGroundwork(content, { sourcePath: 'hostile-waves-computed.workflow.js' });
      } catch (e) {
        caught = e;
      }

      expect(caught).toBeInstanceOf(WorkflowImportError);
      expect((caught as WorkflowImportError).code).toBe('waves-not-json');
      expect((caught as WorkflowImportError).sourcePath).toBe('hostile-waves-computed.workflow.js');
      expectNothingExecuted();
    });

    it('reports waves-not-found when the script carries no WAVES literal', () => {
      const content = "export const meta = { name: 'no-waves' };\n";
      expect(() => importGroundwork(content, {})).toThrowError(WorkflowImportError);
      try {
        importGroundwork(content, {});
      } catch (e) {
        expect((e as WorkflowImportError).code).toBe('waves-not-found');
      }
    });
  });

  describe('claude-workflow importer', () => {
    it('round-trips the real orchestrate.workflow.js meta.phases', () => {
      // orchestrate.workflow.js IS a real Claude Code Workflow script — the
      // contract requires `export const meta` as its first statement.
      const graph = importClaudeWorkflow(readFixture('orchestrate.workflow.js'), {
        sourcePath: 'plans/shell-ux-rearchitecture/artifact/orchestrate.workflow.js',
      });

      expect(graph.source).toBe('claude-workflow');
      expect(graph.id).toBe('groundwork-orchestrate-shell-ux-rearchitecture');

      // The real meta declares three phases, one per wave.
      const phaseNodes = graph.nodes.filter((n) => n.kind === 'phase');
      expect(phaseNodes.length).toBe(3);
      expect(phaseNodes[0].label).toBe('Wave 9b');
      expect(phaseNodes[1].label).toContain('G-WORKFLOW-ADDR');

      // Phases are sequenced.
      expect(
        graph.edges.some(
          (e) =>
            e.from === phaseNodes[0].id && e.to === phaseNodes[1].id && e.kind === 'depends-on',
        ),
      ).toBe(true);

      expect(validateWorkflowDag(graph).valid).toBe(true);
    });

    it('does not execute the script it imports', () => {
      importClaudeWorkflow(readFixture('hostile-waves.workflow.js'), {});
      expectNothingExecuted();
    });

    it('maps explicit phase steps to command nodes', () => {
      const script = `
export const meta = {
  name: 'code-review-workflow',
  title: 'Automated Code Review',
  phases: [
    {
      id: 'phase-lint',
      title: 'Lint & Format',
      steps: [{ id: 'step-lint', title: 'Run Biome', command: 'biome lint .' }],
    },
  ],
};
`;
      const graph = importClaudeWorkflow(script, { sourcePath: '.claude/workflows/review.mjs' });
      expect(graph.id).toBe('code-review-workflow');
      expect(graph.title).toBe('Automated Code Review');
      const lintStep = graph.nodes.find((n) => n.id === 'step-lint');
      expect(lintStep?.run?.kind).toBe('command');
      expect(lintStep?.run?.ref).toBe('biome lint .');
    });
  });

  describe('hooks importer', () => {
    it('round-trips the real ~/.claude/settings.json hooks block', () => {
      const raw = readFixture('claude-hooks-settings.json');
      const settings = JSON.parse(raw) as Parameters<typeof importHooks>[0];

      const graph = importHooks(settings, { sourcePath: '.claude/settings.json' });

      expect(graph.source).toBe('hooks');
      // The real block declares exactly one lifecycle event: PreToolUse.
      expect(graph.nodes.map((n) => n.id)).toEqual(['hook-PreToolUse']);
      expect(graph.nodes[0].kind).toBe('step');
      expect(graph.nodes[0].label).toBe('Hook: PreToolUse');
      expect(graph.nodes[0].source_ref).toBe('.claude/settings.json#hooks.PreToolUse');
      // One declared event ⇒ no sequencing edges.
      expect(graph.edges.length).toBe(0);
      expect(validateWorkflowDag(graph).valid).toBe(true);
    });

    it('sequences the lifecycle when several events are declared', () => {
      const graph = importHooks(
        {
          hooks: {
            SessionStart: [{ type: 'command', command: 'echo start' }],
            PreToolUse: [{ type: 'command', command: 'echo pre' }],
            PostToolUse: [{ type: 'command', command: 'echo post' }],
          },
        },
        { sourcePath: '.claude/settings.json' },
      );

      expect(graph.nodes.length).toBe(3);
      expect(
        graph.edges.some(
          (e) => e.from === 'hook-PreToolUse' && e.to === 'hook-PostToolUse' && e.kind === 'triggers',
        ),
      ).toBe(true);
      expect(validateWorkflowDag(graph).valid).toBe(true);
    });
  });

  describe('cron importer', () => {
    it('round-trips a real pkg manifest cron[] block into schedule and step nodes', () => {
      const manifest = JSON.parse(readFixture('cron-manifest.json')) as Parameters<
        typeof importCron
      >[0];

      const graph = importCron(manifest, {
        sourcePath: 'packages/sidecars/local-store-etl/manifest.json',
      });

      expect(graph.id).toBe('cron:com.ikenga.local-store-etl');
      expect(graph.source).toBe('cron');
      expect(graph.title).toContain('Local Store ETL');

      const schedNodes = graph.nodes.filter((n) => n.kind === 'schedule');
      const stepNodes = graph.nodes.filter((n) => n.kind === 'step');
      expect(schedNodes.length).toBe(1);
      expect(stepNodes.length).toBe(1);

      expect(schedNodes[0].id).toBe('sched-com.ikenga.local-store-etl-daily-sync');
      expect(schedNodes[0].run?.kind).toBe('schedule');
      expect(schedNodes[0].run?.ref).toBe('0 0 6 * * *');

      expect(stepNodes[0].id).toBe('step-com.ikenga.local-store-etl-daily-sync');
      expect(stepNodes[0].run?.kind).toBe('command');
      expect(stepNodes[0].run?.ref).toBe('sidecar:pa-com-ikenga-local-store-etl-main sync');

      expect(
        graph.edges.some(
          (e) => e.from === schedNodes[0].id && e.to === stepNodes[0].id && e.kind === 'triggers',
        ),
      ).toBe(true);

      expect(validateWorkflowDag(graph).valid).toBe(true);
    });
  });
});
