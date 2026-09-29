// The Ọba vault as one list (R57): every recorded primitive, hooks and MCP
// fragments included.
//
// `claudeStoreList()` with no kind returns the file-based kinds only (skill /
// agent / command); since N-A the hook and MCP fragments are listed per kind.
// The Store (installed state of catalog rows, Q5 reachability) and the
// Installed tab (a git/npx item's remote record) read the union.

import { useMemo } from 'react';
import { useQueries } from '@tanstack/react-query';
import { claudeStoreQueryOptions } from '@/lib/queries/claude-config';
import type { ClaudeStoreEntry } from '@/lib/tauri-cmd';

export interface UseVaultEntries {
	entries: ClaudeStoreEntry[];
	isLoading: boolean;
	/** The first listing that failed, if any (the others still count). */
	error: Error | null;
}

export function useVaultEntries(enabled = true): UseVaultEntries {
	const results = useQueries({
		queries: [null, 'hook', 'mcp'].map((kind) => ({
			...claudeStoreQueryOptions(kind as 'hook' | 'mcp' | null),
			enabled,
			retry: false,
		})),
	});
	const data0 = results[0]?.data;
	const data1 = results[1]?.data;
	const data2 = results[2]?.data;
	const entries = useMemo(() => {
		const seen = new Set<string>();
		const out: ClaudeStoreEntry[] = [];
		for (const list of [data0, data1, data2]) {
			for (const e of list ?? []) {
				const k = `${e.kind}:${e.name}`;
				if (seen.has(k)) continue;
				seen.add(k);
				out.push(e);
			}
		}
		return out;
	}, [data0, data1, data2]);
	return {
		entries,
		isLoading: enabled && results.some((r) => r.isLoading),
		error: (results.find((r) => r.error)?.error as Error | undefined) ?? null,
	};
}
