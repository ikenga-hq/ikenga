// WP-16 / WP-16a: the three retired settings pages redirect into /ngwa/health
// with the right ?section=, through a real router.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, waitFor } from '@testing-library/react';
import * as cmd from '@/lib/tauri-cmd';
import { Route as HealthRoute } from '@/routes/ngwa/health';
import { Route as PkgAuditRoute } from './pkg-audit';
import { Route as PkgHealthRoute } from './pkg-health';
import { Route as DataHealthRoute } from './data-health';
import { mkSnapshot, mountRoutes } from '@/routes/ngwa/-ngwa-test-fixtures';

vi.mock('@/lib/registry/use-registry', () => ({
	useRegistryIndex: () => ({ data: undefined, isLoading: false, error: null }),
}));

vi.mock('@/lib/tauri-cmd', async (orig) => {
	const actual = await orig<typeof import('@/lib/tauri-cmd')>();
	const never = () => new Promise(() => {});
	return {
		...actual,
		ngwaSnapshot: vi.fn(),
		pkgPermissionViolationsList: vi.fn(never),
		pkgHealthScan: vi.fn(never),
		pkgKernelStatus: vi.fn(never),
		agentOpsListJobs: vi.fn(never),
		backupList: vi.fn(never),
		detectAgent: vi.fn(never),
	};
});

beforeEach(() => {
	vi.mocked(cmd).ngwaSnapshot.mockResolvedValue(mkSnapshot([]));
});
afterEach(() => cleanup());

function mount(url: string) {
	return mountRoutes(
		[
			{ route: HealthRoute, path: '/ngwa/health' },
			{ route: PkgAuditRoute, path: '/settings/pkg-audit' },
			{ route: PkgHealthRoute, path: '/settings/pkg-health' },
			{ route: DataHealthRoute, path: '/settings/data-health' },
		],
		url
	);
}

describe('retired settings routes redirect into Ngwa → Health', () => {
	for (const [from, section] of [
		['/settings/pkg-audit', 'violations'],
		['/settings/pkg-health', 'violations'],
		['/settings/data-health', 'data'],
	] as const) {
		it(`${from} → /ngwa/health?section=${section}`, async () => {
			const { router } = mount(from);
			await waitFor(() => expect(router.state.location.pathname).toBe('/ngwa/health'));
			expect(router.state.location.search).toEqual({ section });
			await waitFor(() =>
				expect(document.activeElement?.getAttribute('data-panel')).toBe(section)
			);
		});
	}
});
