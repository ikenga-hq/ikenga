// Ngwa Health surface tests (WP-16a). The panels are covered route-level in
// src/routes/ngwa/-health-route.test.tsx; this file holds the helpers and the
// "one engine signal" check across both screens (must-fix 3).

import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { NgwaHealthSurface, fmtBytes, fmtTime, issueLabel } from './ngwa-health-surface';
import { NgwaScopesSurface, type NgwaScopeActions } from './ngwa-scopes-surface';
import { engineItems, mkItem, mkPlacement, mkSnapshot } from '@/routes/ngwa/-ngwa-test-fixtures';

vi.mock('@/lib/tauri-cmd', async (orig) => {
	const actual = await orig<typeof import('@/lib/tauri-cmd')>();
	const never = () => new Promise(() => {});
	return {
		...actual,
		pkgPermissionViolationsList: vi.fn(never),
		pkgHealthScan: vi.fn(never),
		pkgKernelStatus: vi.fn(never),
		agentOpsListJobs: vi.fn(never),
		backupList: vi.fn(never),
		detectAgent: vi.fn(never),
	};
});

afterEach(() => cleanup());

const noop = () => Promise.resolve();
const actions: NgwaScopeActions = {
	enable: noop,
	disable: noop,
	copy: noop,
	move: noop,
	remove: noop,
	enableFor: noop,
	disableFor: noop,
	pkgSetEnabled: noop,
	pkgUninstall: noop,
	openStore: () => {},
};

describe('health helpers', () => {
	it('never turns an unmeasured value into a number', () => {
		expect(fmtTime(null)).toBe('—');
		expect(fmtBytes(null)).toBe('absent');
		expect(fmtBytes(0)).toBe('0 B');
	});
	it('labels an orphan row by its table, not as a broken install', () => {
		expect(issueLabel({ kind: 'orphan_row', table: 'pkg_settings' })).toBe('orphan: pkg_settings');
		expect(issueLabel({ kind: 'manifest_missing' })).toBe('missing manifest');
	});
});

describe('one engine signal for both screens', () => {
	for (const installed of [
		['claude'] as const,
		['claude', 'gemini'] as const,
		['claude', 'codex', 'gemini'] as const,
	]) {
		it(`agrees for engine pkgs: ${installed.join(', ')}`, async () => {
			// codex placements exist even when the codex engine pkg does not.
			const items = [
				...engineItems([...installed]),
				mkItem({
					id: 'skill:personal:s',
					kind: 'skill',
					name: 's',
					placements: [
						mkPlacement({ path: '/c/s' }),
						mkPlacement({ engine: 'codex', path: '/x/s' }),
						mkPlacement({ engine: 'gemini', path: '/g/s' }),
					],
				}),
			];
			const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
			const { container } = render(
				<QueryClientProvider client={qc}>
					<NgwaScopesSurface
						items={items}
						scopes={[{ key: 'personal', label: 'Personal', sub: '~/.claude', active: false }]}
						actions={actions}
					/>
					<NgwaHealthSurface
						items={items}
						snapshot={mkSnapshot(items)}
						onOpenBackup={() => {}}
						onOpenStore={() => {}}
					/>
				</QueryClientProvider>
			);
			const cols = [...container.querySelectorAll('thead th.eng')].map((th) => th.getAttribute('data-col'));
			const healthInstalled = ['claude', 'codex', 'gemini'].filter(
				(e) => !container.querySelector(`[data-engine="${e}"] [data-act="installengine"]`)
			);
			expect(cols).toEqual([...installed].sort((a, b) => ['claude', 'codex', 'gemini'].indexOf(a) - ['claude', 'codex', 'gemini'].indexOf(b)));
			await waitFor(() => expect(healthInstalled).toEqual(cols));
			expect(container.querySelector('[data-enginen]')?.textContent).toBe(`${installed.length} of 3`);
		});
	}
});
