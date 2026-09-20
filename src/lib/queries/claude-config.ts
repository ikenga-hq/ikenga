import { useEffect, useRef } from 'react';
import { type QueryKey, queryOptions, useMutation, useQueryClient } from '@tanstack/react-query';

import {
	claudeConfigListen,
	claudeConfigLoad,
	claudeConfigUnwatch,
	claudeConfigWatch,
	claudePrimitiveCopy,
	claudePrimitiveDisable,
	claudePrimitiveDisableFor,
	claudePrimitiveEnable,
	claudePrimitiveEnableFor,
	claudePrimitiveMove,
	claudePrimitiveRemove,
	claudeStoreImport,
	claudeStoreList,
	ngwaCrossEngineCopy,
	obaAutoUpdateAll,
	obaBackfillRegistry,
	obaCheckUpdate,
	obaDependents,
	obaForget,
	obaInstallGit,
	obaInstallNpx,
	obaInstallWithDeps,
	obaRelinkDependents,
	obaSafeDelete,
	obaSetAutoUpdate,
	obaUnlinkOne,
	obaUpdate,
	type ClaudeAgent,
	type ClaudeCommand,
	type ClaudeConfig,
	type ClaudeHook,
	type ClaudeMcp,
	type ClaudeSkill,
	type ClaudeStoreEntry,
	type ClaudeStoreKind,
	type ClaudeStoreMutation,
	type ClaudeStoreScope,
	type ConfigFormat,
	type EngineId,
	type KindStatus,
	type NgwaCopyBatchResult,
	type NgwaCopyDestination,
	type NgwaHookFile,
	type NgwaStoreError,
	type NgwaTranscodeMode,
	type AutoUpdateSummary,
	type InstallWithDepsResult,
	type ObaRelinkRow,
	type SafeDeleteOutcome,
	type UpdateStatus,
} from '@/lib/tauri-cmd';
import type { PrimitiveCatalogEntry } from '@/lib/registry/primitives';
import { queryKeys } from '@/lib/query-keys';

// Re-export the scan-result entry types plus the Ngwa Phase-2 cross-system
// enums (WP-19) so the engine-grouped facet (WP-20) imports its vocabulary from
// the query module rather than reaching into tauri-cmd. Every entry now carries
// optional `system` / `format` / `status` — consumers treat absent `system` as
// `"claude"` and absent `status` as `"active"`.
export type {
	ClaudeAgent,
	ClaudeCommand,
	ClaudeConfig,
	ClaudeHook,
	ClaudeMcp,
	ClaudeSkill,
	ClaudeStoreEntry,
	ClaudeStoreKind,
	ClaudeStoreMutation,
	ClaudeStoreScope,
	ConfigFormat,
	EngineId,
	KindStatus,
	NgwaCopyBatchResult,
	NgwaCopyDestination,
	NgwaHookFile,
	NgwaStoreError,
	NgwaTranscodeMode,
};

// The query layer is engine-agnostic: `claudeConfigLoad` returns the same
// `ClaudeConfig` shape whether it resolves the live Rust scan or the WP-19
// multi-engine dev mock (gated by `NGWA_SCAN_MOCK` in tauri-cmd.ts). The new
// per-entry `system` / `format` / `status` fields flow straight through this
// query into WP-20's engine-grouped facet — no extra mapping here.
export function claudeConfigQueryOptions(projectRoots: readonly string[]) {
	const roots = [...projectRoots];
	return queryOptions({
		queryKey: queryKeys.claudeConfig.load(roots),
		queryFn: () => claudeConfigLoad(roots),
		// The watcher invalidates on change; idle staleness is fine.
		staleTime: 60_000,
	});
}

/** Spin up a watcher for the supplied project roots and personal `~/.claude/`,
 *  invalidating the matching query on any FS change. Releases watchers and
 *  the listener on unmount.
 */
export function useClaudeConfigWatch(projectRoots: readonly string[], enabled = true) {
	const queryClient = useQueryClient();
	// Stable stringified key — re-attach when the root set changes.
	const key = [...projectRoots].sort().join('|');
	useEffect(() => {
		if (!enabled) return;
		let cancelled = false;
		let unlisten: (() => void) | null = null;
		let watcherIds: string[] = [];
		const debounce = makeDebounce(150);
		void (async () => {
			try {
				const ids = await claudeConfigWatch([...projectRoots]);
				if (cancelled) {
					await claudeConfigUnwatch(ids);
					return;
				}
				watcherIds = Array.isArray(ids) ? ids : [];
				unlisten = await claudeConfigListen(watcherIds, () => {
					debounce(() => {
						queryClient.invalidateQueries({ queryKey: queryKeys.claudeConfig.all });
					});
				});
			} catch (e) {
				// eslint-disable-next-line no-console
				console.warn('[claude-config] watch failed', e);
			}
		})();
		return () => {
			cancelled = true;
			if (unlisten) unlisten();
			if (watcherIds?.length) {
				void claudeConfigUnwatch(watcherIds);
			}
		};
		// eslint-disable-next-line react-hooks/exhaustive-deps
	}, [key, enabled]);
}

function makeDebounce(ms: number) {
	let t: ReturnType<typeof setTimeout> | null = null;
	return (fn: () => void) => {
		if (t) clearTimeout(t);
		t = setTimeout(fn, ms);
	};
}

// ─── Ngwa central store (Ọba) — catalog query + mutation hooks ───────────────
//
// Mirrors the `secrets.ts` mutation→invalidate pattern: each mutation calls its
// `tauri-cmd` wrapper and, onSuccess, invalidates the affected query keys. Every
// store mutation invalidates BOTH the store catalog (`claudeStore.all`, drives
// the per-scope `enabledIn` badges) AND the on-disk scan (`claudeConfig.all`,
// which reflects the new symlink / merged-fragment state and otherwise only
// refreshes on the FS watcher's `claude-config:changed` tick).
//
// Until WP-02/03 register the Rust commands, the wrappers resolve typed canned
// data via the dev-flag mock in `tauri-cmd.ts` (see `NGWA_STORE_MOCK`).

/** Query the central-store catalog, optionally filtered by primitive kind. */
export function claudeStoreQueryOptions(kind?: ClaudeStoreKind | null) {
	return queryOptions({
		queryKey: queryKeys.claudeStore.list(kind ?? null),
		queryFn: () => claudeStoreList(kind ?? null),
		staleTime: 30_000,
	});
}

/**
 * Live dependents of a primitive's canonical master (Ọba registry, WP-04) —
 * symlinks across scopes/engines that resolve into it. Loaded on demand when
 * the detail-pane safe-delete guard opens. Short stale time: dependents reflect
 * live filesystem state and can change out-of-band.
 */
export function obaDependentsQueryOptions(kind: ClaudeStoreKind, name: string) {
	return queryOptions({
		queryKey: [...queryKeys.claudeStore.all, 'dependents', kind, name] as const,
		queryFn: () => obaDependents(kind, name),
		staleTime: 5_000,
	});
}

/**
 * Guarded delete of a primitive's canonical master (WP-04). Returns the
 * `SafeDeleteOutcome` so the caller can branch on the verdict (a refusal is a
 * normal outcome, not an error — the UI renders the relink chooser). Invalidates
 * the store + scan on any outcome that touched disk.
 */
export function useSafeDelete() {
	const invalidate = useInvalidateClaudeStore();
	return useMutation<SafeDeleteOutcome, Error, { kind: ClaudeStoreKind; name: string }>({
		mutationFn: ({ kind, name }) => obaSafeDelete(kind, name),
		onSuccess: (outcome) => {
			if (outcome.removed) invalidate();
		},
	});
}

/** Relink-all: re-point dependent symlinks at a new master before forgetting it. */
export function useRelinkDependents() {
	const invalidate = useInvalidateClaudeStore();
	return useMutation<ObaRelinkRow[], Error, { dependents: string[]; newMaster: string }>({
		mutationFn: ({ dependents, newMaster }) => obaRelinkDependents(dependents, newMaster),
		onSuccess: invalidate,
	});
}

/** Unlink one dependent placement (a symlink) by absolute path (D-01). */
export function useUnlinkOne() {
	const invalidate = useInvalidateClaudeStore();
	return useMutation<boolean, Error, { path: string }>({
		mutationFn: ({ path }) => obaUnlinkOne(path),
		onSuccess: invalidate,
	});
}

/** Forget a primitive from the registry — provenance only, no files touched (D-01). */
export function useForgetFromRegistry() {
	const invalidate = useInvalidateClaudeStore();
	return useMutation<boolean, Error, { kind: ClaudeStoreKind; name: string }>({
		mutationFn: ({ kind, name }) => obaForget(kind, name),
		onSuccess: invalidate,
	});
}

/** Back-fill the registry with external masters discovered in the live farm. */
export function useBackfillRegistry() {
	const invalidate = useInvalidateClaudeStore();
	return useMutation<number, Error, void>({
		mutationFn: () => obaBackfillRegistry(),
		onSuccess: invalidate,
	});
}

/**
 * Install a catalog primitive into the vault (Ọba Phase 2 · WP-10c). Routes to
 * `oba_install_git` / `oba_install_npx` by the entry's `source`. Invalidating the
 * store list re-tallies the Installed/Available/Updatable chips (the merged view
 * recomputes from the new store), so the catalog query itself need not refetch.
 */
export function useObaInstall() {
	const invalidate = useInvalidateClaudeStore();
	return useMutation<ClaudeStoreEntry, Error, PrimitiveCatalogEntry>({
		// Phase 3: a catalog Install threads `fromCatalog: true` so the recorded
		// provenance carries the discovery origin (orthogonal to the resolved git/
		// npx mechanism) and opts the entry into auto-update.
		mutationFn: (entry) =>
			entry.source === 'npx'
				? obaInstallNpx(entry.kind, entry.name, entry.url, true)
				: obaInstallGit(entry.kind, entry.name, entry.url, null, true),
		onSuccess: invalidate,
	});
}

/**
 * Install a catalog primitive AND its forward-dependency closure (ADR-015 §3b ·
 * WP-14). Threads the full catalog snapshot so the resolver can fetch each
 * `requires` dependency by its source/url; the missing closure auto-installs
 * transactionally (rolled back with the target on any failure). Invalidating the
 * store list re-tallies the chips after the target + closure land.
 */
export function useObaInstallWithDeps() {
	const invalidate = useInvalidateClaudeStore();
	return useMutation<
		InstallWithDepsResult,
		Error,
		{ entry: PrimitiveCatalogEntry; catalog: PrimitiveCatalogEntry[] }
	>({
		mutationFn: ({ entry, catalog }) =>
			obaInstallWithDeps(
				entry.kind,
				entry.name,
				entry.source,
				entry.url,
				catalog.map((c) => ({ kind: c.kind, name: c.name, source: c.source, url: c.url })),
				null,
				true
			),
		onSuccess: invalidate,
	});
}

/** Re-fetch a managed primitive into its canonical in place (Ọba Phase 2 · WP-09). */
export function useObaUpdate() {
	const invalidate = useInvalidateClaudeStore();
	return useMutation<ClaudeStoreEntry, Error, { kind: ClaudeStoreKind; name: string }>({
		mutationFn: ({ kind, name }) => obaUpdate(kind, name),
		onSuccess: invalidate,
	});
}

/**
 * Phase 3 — auto-update trust policy. Runs auto-updates across every
 * `autoUpdate`-opted entry that's behind its remote. FE-driven: call this on the
 * Ọba/catalog surface mount (see `useObaAutoUpdateOnMount`). Per-entry errors are
 * collected by the backend and never abort the batch. Invalidates the store +
 * scan when anything actually updated so the surface re-renders the new
 * versions.
 */
export function useObaAutoUpdateAll() {
	const invalidate = useInvalidateClaudeStore();
	return useMutation<AutoUpdateSummary, Error, void>({
		mutationFn: () => obaAutoUpdateAll(),
		onSuccess: (summary) => {
			if (summary.updated.length > 0) invalidate();
		},
	});
}

/**
 * Fire a one-shot auto-update sweep when the catalog surface mounts (Phase 3,
 * FE-driven — explicitly NOT a background timer/daemon). Guards against double-
 * firing under React StrictMode's double-mount via a ref. Returns the mutation so
 * the surface can show in-flight / result state.
 */
export function useObaAutoUpdateOnMount(enabled = true) {
	const sweep = useObaAutoUpdateAll();
	const fired = useRef(false);
	useEffect(() => {
		if (!enabled || fired.current) return;
		fired.current = true;
		sweep.mutate();
		// eslint-disable-next-line react-hooks/exhaustive-deps
	}, [enabled]);
	return sweep;
}

/** Phase 3 — toggle the per-entry auto-update opt-in (persists to the registry).
 *  Optimistic: the cached store entry flips immediately so the toggle repaints on
 *  click instead of waiting for the mutation→invalidate→refetch round-trip. Rolls
 *  back on error; `onSettled` reconciles with server truth either way. */
export function useObaSetAutoUpdate() {
	const qc = useQueryClient();
	const invalidate = useInvalidateClaudeStore();
	return useMutation<
		boolean,
		Error,
		{ kind: ClaudeStoreKind; name: string; enabled: boolean },
		{ prev: [QueryKey, ClaudeStoreEntry[] | undefined][] }
	>({
		mutationFn: ({ kind, name, enabled }) => obaSetAutoUpdate(kind, name, enabled),
		onMutate: async ({ kind, name, enabled }) => {
			// Patch only the store-LIST query family (`['claude_store','list',*]`);
			// the dependents / update-check queries don't match this prefix so their
			// (differently-shaped) data is untouched. The detail pane's `entry` is
			// derived from this cache, so it repaints synchronously.
			await qc.cancelQueries({ queryKey: queryKeys.claudeStore.all });
			const prev = qc.getQueriesData<ClaudeStoreEntry[]>({ queryKey: ['claude_store', 'list'] });
			for (const [key, data] of prev) {
				if (!data) continue;
				qc.setQueryData<ClaudeStoreEntry[]>(
					key,
					data.map((e) => (e.kind === kind && e.name === name ? { ...e, autoUpdate: enabled } : e))
				);
			}
			return { prev };
		},
		onError: (_err, _vars, ctx) => {
			for (const [key, data] of ctx?.prev ?? []) qc.setQueryData(key, data);
		},
		onSettled: () => invalidate(),
	});
}

/** On-demand update check for a git/npx-installed primitive (read-only). */
export function obaCheckUpdateQueryOptions(kind: ClaudeStoreKind, name: string) {
	return queryOptions({
		queryKey: [...queryKeys.claudeStore.all, 'update-check', kind, name] as const,
		queryFn: (): Promise<UpdateStatus> => obaCheckUpdate(kind, name),
		staleTime: 5 * 60 * 1000,
	});
}

/** Invalidate the store catalog + the on-disk scan after any store mutation. */
function useInvalidateClaudeStore() {
	const qc = useQueryClient();
	return () => {
		qc.invalidateQueries({ queryKey: queryKeys.claudeStore.all });
		qc.invalidateQueries({ queryKey: queryKeys.claudeConfig.all });
	};
}

/** Import an on-disk primitive into the central store. */
export function useImportToStore() {
	const invalidate = useInvalidateClaudeStore();
	return useMutation<
		ClaudeStoreEntry,
		Error,
		{ kind: ClaudeStoreKind; name: string; sourcePath: string }
	>({
		mutationFn: ({ kind, name, sourcePath }) => claudeStoreImport(kind, name, sourcePath),
		onSuccess: invalidate,
	});
}

/** Enable a store catalog entry in a target scope. */
export function useEnablePrimitive() {
	const invalidate = useInvalidateClaudeStore();
	return useMutation<
		ClaudeStoreMutation,
		Error,
		{ kind: ClaudeStoreKind; name: string; scope: ClaudeStoreScope }
	>({
		mutationFn: ({ kind, name, scope }) => claudePrimitiveEnable(kind, name, scope),
		onSuccess: invalidate,
	});
}

/** Disable a store catalog entry in a target scope (inverse of enable). */
export function useDisablePrimitive() {
	const invalidate = useInvalidateClaudeStore();
	return useMutation<void, Error, { kind: ClaudeStoreKind; name: string; scope: ClaudeStoreScope }>(
		{
			mutationFn: ({ kind, name, scope }) => claudePrimitiveDisable(kind, name, scope),
			onSuccess: invalidate,
		}
	);
}

/** Copy a primitive from one scope to another, leaving the source in place. */
export function useCopyPrimitive() {
	const invalidate = useInvalidateClaudeStore();
	return useMutation<
		ClaudeStoreMutation,
		Error,
		{
			kind: ClaudeStoreKind;
			name: string;
			fromScope: ClaudeStoreScope;
			toScope: ClaudeStoreScope;
		}
	>({
		mutationFn: ({ kind, name, fromScope, toScope }) =>
			claudePrimitiveCopy(kind, name, fromScope, toScope),
		onSuccess: invalidate,
	});
}

/** Move a primitive from one scope to another (copy-then-remove-source). */
export function useMovePrimitive() {
	const invalidate = useInvalidateClaudeStore();
	return useMutation<
		ClaudeStoreMutation,
		Error,
		{
			kind: ClaudeStoreKind;
			name: string;
			fromScope: ClaudeStoreScope;
			toScope: ClaudeStoreScope;
		}
	>({
		mutationFn: ({ kind, name, fromScope, toScope }) =>
			claudePrimitiveMove(kind, name, fromScope, toScope),
		onSuccess: invalidate,
	});
}

/** Remove a primitive from a single scope's `.claude/` (scope-local delete). */
export function useRemovePrimitive() {
	const invalidate = useInvalidateClaudeStore();
	return useMutation<void, Error, { kind: ClaudeStoreKind; name: string; scope: ClaudeStoreScope }>(
		{
			mutationFn: ({ kind, name, scope }) => claudePrimitiveRemove(kind, name, scope),
			onSuccess: invalidate,
		}
	);
}

// ─── Ngwa v2b cross-system write hooks (WP-26 / D-09) ────────────────────────
//
// The per-ENGINE enable/disable + the cross-engine batch copy back the D-09
// drawer. They mirror the same invalidate pattern as the Phase-1 store hooks:
// each mutation invalidates the store catalog + the on-disk scan on success, so
// the engine-grouped facet live-refreshes. The per-engine writes target the
// frozen WP-22 `claude_primitive_enable_for` / `disable_for` commands; the batch
// copy targets WP-24's `claude_primitive_copy_batch` (mock-backed in tauri-cmd
// until WP-24 lands — see `NGWA_TRANSCODE_MOCK`).

/** Enable a settings-embedded primitive (hook/mcp) in a scope FOR A SPECIFIC
 *  ENGINE. Gemini strict-key rejection surfaces as an `NgwaStoreError` of kind
 *  `strictKeyRejected` (thrown before any disk write). `engine` defaults to
 *  `'claude'` so Phase-1 behaviour is preserved when omitted. */
export function useEnablePrimitiveFor() {
	const invalidate = useInvalidateClaudeStore();
	return useMutation<
		ClaudeStoreMutation,
		Error,
		{
			engine: EngineId;
			kind: ClaudeStoreKind;
			name: string;
			scope: ClaudeStoreScope;
			hookFile?: NgwaHookFile;
		}
	>({
		mutationFn: ({ engine, kind, name, scope, hookFile }) =>
			claudePrimitiveEnableFor(engine, kind, name, scope, hookFile ?? 'shared'),
		onSuccess: invalidate,
	});
}

/** Disable a settings-embedded primitive (hook/mcp) from a scope for a specific
 *  engine — inverse of {@link useEnablePrimitiveFor}. */
export function useDisablePrimitiveFor() {
	const invalidate = useInvalidateClaudeStore();
	return useMutation<
		void,
		Error,
		{
			engine: EngineId;
			kind: ClaudeStoreKind;
			name: string;
			scope: ClaudeStoreScope;
			hookFile?: NgwaHookFile;
		}
	>({
		mutationFn: ({ engine, kind, name, scope, hookFile }) =>
			claudePrimitiveDisableFor(engine, kind, name, scope, hookFile ?? 'shared'),
		onSuccess: invalidate,
	});
}

/** Cross-engine forward transcode copy/move into N (engine, scope) destinations
 *  in one batch — the D-09 "Copy to N" action. Resolves to a per-row batch
 *  result (`NgwaCopyBatchResult`); the drawer renders partial failures inline.
 *  Invalidates on settle (not just success) so the scan refreshes even when some
 *  rows fail — a partial success still changed the disk for the rows that wrote.
 *
 *  MOCK SEAM: `ngwaCrossEngineCopy` is mock-backed until WP-24 (see tauri-cmd
 *  `NGWA_TRANSCODE_MOCK`); this hook is finalized against the real command then. */
export function useCrossEngineCopy() {
	const invalidate = useInvalidateClaudeStore();
	return useMutation<
		NgwaCopyBatchResult,
		Error,
		{
			fromEngine: EngineId;
			kind: ClaudeStoreKind;
			name: string;
			fromScope: ClaudeStoreScope;
			destinations: NgwaCopyDestination[];
			move?: boolean;
		}
	>({
		mutationFn: ({ fromEngine, kind, name, fromScope, destinations, move }) =>
			ngwaCrossEngineCopy(fromEngine, kind, name, fromScope, destinations, move ?? false),
		onSettled: () => invalidate(),
	});
}
