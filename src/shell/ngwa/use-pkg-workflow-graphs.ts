// Read an installed pkg's §10 `workflows[]` and adapt each entry to a
// `WorkflowGraph` (WP-31 review fix, Round 29 — mounting the flow renderer).
//
// Neither `ngwa_snapshot` nor `pkg_kernel_status` carries `workflows[]` yet
// (the snapshot is the G-NGWA-ITEM join; kernel status is
// `installed[] + registries + api_version`). Rather than add a Tauri command
// — which would change the ACL surface for a read the shell can already do —
// this reads the manifest through the existing `pkg_preview_manifest` command
// and maps it with `fromPkgWorkflow`.

import { useQuery } from '@tanstack/react-query';
import type { WorkflowEntry, WorkflowGraph } from '@/lib/workflows/graph';
import { fromPkgWorkflow } from '@/lib/workflows/graph';
import { pkgPreviewManifest } from '@/lib/tauri-cmd';

/**
 * Adapt every `workflows[]` entry of a manifest to a `WorkflowGraph`.
 *
 * `source_ref` carries the entry's **index** per G-MANIFEST-V5 §10
 * (`<manifest>#/workflows/<i>/steps/<j>`), which is why the index is threaded
 * through rather than derived from the entry id.
 */
export function workflowGraphsFromManifest(
  pkgId: string,
  manifestPath: string,
  workflows: unknown,
): WorkflowGraph[] {
  if (!Array.isArray(workflows)) return [];
  const graphs: WorkflowGraph[] = [];
  workflows.forEach((entry, index) => {
    const wf = entry as Partial<WorkflowEntry>;
    if (!wf || typeof wf.id !== 'string' || !Array.isArray(wf.steps) || wf.steps.length === 0) {
      return;
    }
    graphs.push(fromPkgWorkflow(pkgId, manifestPath, wf as WorkflowEntry, index));
  });
  return graphs;
}

export interface UsePkgWorkflowGraphsResult {
  graphs: WorkflowGraph[];
  isLoading: boolean;
  error: unknown;
}

/**
 * `workflows[]` declared by the pkg installed at `installPath`, as graphs.
 * Disabled (and empty) when there is no install path to read.
 */
export function usePkgWorkflowGraphs(
  pkgId: string,
  installPath: string | null | undefined,
): UsePkgWorkflowGraphsResult {
  const query = useQuery<WorkflowGraph[]>({
    queryKey: ['pkg', 'workflows', pkgId, installPath],
    enabled: Boolean(installPath),
    staleTime: 60_000,
    retry: false,
    queryFn: async () => {
      const manifest = await pkgPreviewManifest(installPath!);
      const manifestPath = `${installPath}/manifest.json`;
      return workflowGraphsFromManifest(
        typeof manifest.id === 'string' ? manifest.id : pkgId,
        manifestPath,
        (manifest as { workflows?: unknown }).workflows,
      );
    },
  });

  return {
    graphs: query.data ?? [],
    isLoading: query.isLoading,
    error: query.error,
  };
}
