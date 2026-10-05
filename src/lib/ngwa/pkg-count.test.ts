// DEC-73 (Round 58) — the status bar's "N pkgs" segment and the Ngwa
// Installed tab's pkg sub-count must read the same number for the same
// install set. `selectPkgCount` is that shared definition; this test checks
// it against both shapes of fixture: the Installed tab's `NgwaItem[]`
// (`-ngwa-test-fixtures.tsx`, same fixture `scopesItems()` the route tests
// use) and a status-bar-shaped kernel row list (`PkgInstalledSummary[]`,
// mirroring `use-derived.test.ts`'s fixtures).
import { describe, expect, it } from 'vitest';
import type { PkgInstalledSummary } from '@/lib/tauri-cmd';
import { engineItems, mkItem, scopesItems } from '@/routes/ngwa/-ngwa-test-fixtures';
import { PKG_NGWA_KINDS, selectPkgCount } from './pkg-count';

/** Minimal status-bar-side fixture: one kernel row per pkg. The status bar
 *  never filters by kind — every kernel row already is a pkg. */
function kernelRows(n: number): PkgInstalledSummary[] {
	return Array.from(
		{ length: n },
		(_, i) => ({ id: `pkg-${i}` }) as unknown as PkgInstalledSummary
	);
}

describe('selectPkgCount (DEC-73)', () => {
	it('counts only the kernel-sourced kinds (app/engine/sidecar, and a pkg-backed tool)', () => {
		const items = scopesItems();
		// 2 engines (claude, codex) + 2 apps (tasks, studio) = 4 pkgs. The
		// `tool:personal:github` row is a hand-configured MCP server picked up
		// by the engine-config scan, not a kernel pkg — `${kind}:${scope}:${name}`
		// id shape, not a manifest id — and must NOT be counted.
		expect(selectPkgCount(items)).toBe(4);
	});

	it("doesn't double-count an installed mcp pkg against its own registered server", () => {
		// The pkg's own kernel row: id IS the manifest id.
		const pkgRow = mkItem({
			id: 'com.ikenga.mcp-iyke',
			kind: 'tool',
			name: 'com.ikenga.mcp-iyke',
			install_path: '/pkgs/mcp-iyke',
		});
		// The SAME pkg's `mcp[]` server, separately picked up by the
		// engine-config scan from `~/.claude.json` (`scan_rows()`, `ngwa.rs`) —
		// synthesized `tool:<scope>:<name>` id, `owner_pkg_id` pointing back at
		// the pkg. This must not be counted a second time.
		const ownedServerRow = mkItem({
			id: 'tool:personal:pkg-com-ikenga-mcp-iyke-mcp',
			kind: 'tool',
			name: 'pkg-com-ikenga-mcp-iyke-mcp',
			owner_pkg_id: 'com.ikenga.mcp-iyke',
		});
		// A genuinely standalone, hand-configured MCP server — not a pkg at all.
		const standaloneServerRow = mkItem({
			id: 'tool:personal:github',
			kind: 'tool',
			name: 'github',
		});
		expect(selectPkgCount([pkgRow, ownedServerRow, standaloneServerRow])).toBe(1);
	});

	it('never counts Ọba/engine-config kinds, even with none installed', () => {
		expect(selectPkgCount([])).toBe(0);
		expect(selectPkgCount(engineItems(['claude']))).toBe(1);
	});

	it('gives the same number for the status-bar fixture and the Installed fixture', () => {
		const installedFixtureCount = selectPkgCount(scopesItems());
		const statusBarFixtureCount = kernelRows(installedFixtureCount).length;
		expect(statusBarFixtureCount).toBe(installedFixtureCount);
	});

	it('PKG_NGWA_KINDS matches the four kernel-producible NgwaKinds', () => {
		expect([...PKG_NGWA_KINDS].sort()).toEqual(['app', 'engine', 'sidecar', 'tool']);
	});
});
