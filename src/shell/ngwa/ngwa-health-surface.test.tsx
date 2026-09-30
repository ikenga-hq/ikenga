// Ngwa Health surface tests (WP-16a). The panels are covered route-level in
// src/routes/ngwa/-health-route.test.tsx; this file holds the helpers and the
// "one engine signal" check across both screens (must-fix 3).

import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import * as cmd from '@/lib/tauri-cmd';
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

describe('a pkg on disk that failed to register (Bug 2)', () => {
	const MEETINGS: cmd.PkgHealthIssue = {
		id: 'com.ikenga.meetings',
		install_path: 'C:/pkgs/com.ikenga.meetings',
		enabled: false,
		issue: { kind: 'pkgs_dir_unloadable' },
		detail:
			'on disk but failed to load: `ui.nav` was removed in manifest v5 (G-MANIFEST-V5 §4 / DEC-37) — declare `ui.views[]` instead',
	};

	function mount(props: { canReinstall?: (id: string) => boolean; onReinstall?: (id: string) => void }) {
		vi.mocked(cmd.pkgHealthScan).mockResolvedValueOnce([MEETINGS]);
		const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
		return render(
			<QueryClientProvider client={qc}>
				<NgwaHealthSurface
					items={[]}
					snapshot={mkSnapshot([])}
					onOpenBackup={() => {}}
					onOpenStore={() => {}}
					{...props}
				/>
			</QueryClientProvider>
		);
	}

	it('labels the new kinds', () => {
		expect(issueLabel({ kind: 'pkgs_dir_unloadable' })).toBe('failed to load');
		expect(issueLabel({ kind: 'register_failed' })).toBe('not registered');
	});

	it('lists the broken pkg with its parse error and "Reinstall from registry" when the registry has it', async () => {
		const onReinstall = vi.fn();
		const { container } = mount({ canReinstall: (id) => id === 'com.ikenga.meetings', onReinstall });
		const row = await waitFor(() => {
			const el = container.querySelector('[data-install="com.ikenga.meetings"]');
			if (!el) throw new Error('row not rendered yet');
			return el as HTMLElement;
		});
		expect(row.querySelector('[data-issue="pkgs_dir_unloadable"]')?.textContent).toBe('failed to load');
		expect(row.textContent).toContain('ui.nav');
		// Reinstall replaces Remove — the fix, not the delete.
		expect(row.querySelector('[data-remove]')).toBeNull();
		fireEvent.click(screen.getByRole('button', { name: 'Reinstall from registry' }));
		expect(onReinstall).toHaveBeenCalledWith('com.ikenga.meetings');
	});

	it('offers Remove (deleting the folder) when the registry does not list it', async () => {
		const { container } = mount({ canReinstall: () => false, onReinstall: vi.fn() });
		const remove = await waitFor(() => {
			const el = container.querySelector('[data-remove="com.ikenga.meetings"]');
			if (!el) throw new Error('row not rendered yet');
			return el as HTMLElement;
		});
		expect(container.querySelector('[data-reinstall]')).toBeNull();
		fireEvent.click(remove);
		expect(await screen.findByText(/deletes its folder/)).toBeTruthy();
	});
});
