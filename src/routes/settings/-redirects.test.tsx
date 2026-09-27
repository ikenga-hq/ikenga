// WP-35: the retired settings pages redirect onto the nine-section shell
// (and into Ngwa → Health), through a real router.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, waitFor } from '@testing-library/react';
import type { AnyRoute } from '@tanstack/react-router';
import * as cmd from '@/lib/tauri-cmd';
import { Route as HealthRoute } from '@/routes/ngwa/health';
import { Route as ActivityBarRoute } from './activity-bar';
import { Route as AgentRoute } from './agent';
import { Route as ArtifactGridRoute } from './artifact-grid';
import { Route as BackupRoute } from './backup';
import { Route as DataHealthRoute } from './data-health';
import { Route as OnboardingRoute } from './onboarding';
import { Route as PackagesRoute } from './packages';
import { Route as PkgAuditRoute } from './pkg-audit';
import { Route as PkgHealthRoute } from './pkg-health';
import { Route as TerminalRoute } from './terminal';
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

const LEGACY_REDIRECTS: Array<[string, AnyRoute, string]> = [
	['/settings/activity-bar', ActivityBarRoute, '/settings/workspace'],
	['/settings/agent', AgentRoute, '/settings/engines'],
	['/settings/artifact-grid', ArtifactGridRoute, '/settings/workspace'],
	['/settings/backup', BackupRoute, '/settings/storage'],
	['/settings/onboarding', OnboardingRoute, '/settings/workspace'],
	['/settings/packages', PackagesRoute, '/ngwa/store'],
	['/settings/terminal', TerminalRoute, '/settings/engines'],
];

describe('retired settings routes redirect', () => {
	for (const [from, route, to] of LEGACY_REDIRECTS) {
		it(`${from} → ${to}`, async () => {
			const { router } = mountRoutes([{ route, path: from }], from);
			await waitFor(() => expect(router.state.location.pathname).toBe(to));
		});
	}

	it('/settings/pkg-audit → /ngwa/health?section=violations', async () => {
		const { router } = mountRoutes(
			[
				{ route: HealthRoute, path: '/ngwa/health' },
				{ route: PkgAuditRoute, path: '/settings/pkg-audit' },
			],
			'/settings/pkg-audit'
		);
		await waitFor(() => expect(router.state.location.pathname).toBe('/ngwa/health'));
		expect(router.state.location.search).toEqual({ section: 'violations' });
		await waitFor(() =>
			expect(document.activeElement?.getAttribute('data-panel')).toBe('violations')
		);
	});

	it('/settings/pkg-health → /ngwa/health?section=violations', async () => {
		const { router } = mountRoutes(
			[
				{ route: HealthRoute, path: '/ngwa/health' },
				{ route: PkgHealthRoute, path: '/settings/pkg-health' },
			],
			'/settings/pkg-health'
		);
		await waitFor(() => expect(router.state.location.pathname).toBe('/ngwa/health'));
		expect(router.state.location.search).toEqual({ section: 'violations' });
		await waitFor(() =>
			expect(document.activeElement?.getAttribute('data-panel')).toBe('violations')
		);
	});

	it('/settings/data-health → /ngwa/health?section=data', async () => {
		const { router } = mountRoutes(
			[
				{ route: HealthRoute, path: '/ngwa/health' },
				{ route: DataHealthRoute, path: '/settings/data-health' },
			],
			'/settings/data-health'
		);
		await waitFor(() => expect(router.state.location.pathname).toBe('/ngwa/health'));
		expect(router.state.location.search).toEqual({ section: 'data' });
		await waitFor(() => expect(document.activeElement?.getAttribute('data-panel')).toBe('data'));
	});
});
