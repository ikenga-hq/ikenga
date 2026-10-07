// `availablePkgIds` — the set the rail uses to hide pins into a pkg that is on
// disk but failed to register (so they never open "No such package route").

import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

const handlers = vi.hoisted(() => new Map<string, (ev: { payload: unknown }) => void>());

vi.mock('@/lib/transport', () => ({
	listen: vi.fn(async (name: string, cb: (ev: { payload: unknown }) => void) => {
		handlers.set(name, cb);
		return () => handlers.delete(name);
	}),
}));

vi.mock('@/lib/tauri-cmd', () => ({
	pkgKernelStatus: vi.fn(),
	isRemoteWebSession: () => false,
	activityPinsList: vi.fn(async () => []),
	activityPinsAdd: vi.fn(),
}));

import * as cmd from '@/lib/tauri-cmd';
import { availablePkgIdsOf, usePkgActivityBarEntries } from './use-activity-bar-entries';

const m = vi.mocked(cmd);

function status(pkgIds: string[], extra: Partial<{ installed: string[] }> = {}) {
	return {
		installed: (extra.installed ?? []).map((id) => ({ id })),
		api_version: 5,
		registries: {
			views: {
				entries: pkgIds.map((id) => ({
					pkg_id: id,
					pkg_name: id,
					qualified_id: `${id}:main`,
					id: 'main',
					title: id,
					route: '/main',
					pane_route: `/pkg/${id}/main`,
					pin_on_install: false,
				})),
			},
			activity_bar: { entries: [] },
			sidecar_supervisor: { entries: [] },
		},
	} as unknown as cmd.PkgKernelStatus;
}

beforeEach(() => {
	handlers.clear();
	m.pkgKernelStatus.mockReset();
});

describe('availablePkgIdsOf', () => {
	it('unions registered views / rail entries / ui routes with install records', () => {
		const ids = availablePkgIdsOf({
			installed: [{ id: 'com.x.parked' }],
			registries: {
				views: { entries: [{ pkg_id: 'com.x.views' }] },
				activity_bar: { entries: [{ pkg_id: 'com.x.rail' }] },
				ui_routes: { entries: [{ pkg_id: 'com.x.routes' }] },
				cron: { entries: [{ pkg_id: 'com.x.cron-only' }] },
			},
		});
		expect([...ids].sort()).toEqual(['com.x.parked', 'com.x.rail', 'com.x.routes', 'com.x.views']);
	});
});

describe('usePkgActivityBarEntries().availablePkgIds', () => {
	it('is null until the snapshot lands, and on a failed snapshot (nothing is hidden)', async () => {
		m.pkgKernelStatus.mockRejectedValueOnce(new Error('kernel down'));
		const { result } = renderHook(() => usePkgActivityBarEntries());
		expect(result.current.availablePkgIds).toBeNull();
		await waitFor(() => expect(result.current.loaded).toBe(true));
		expect(result.current.availablePkgIds).toBeNull();
	});

	it('omits a pkg that failed to register, and gains it once its view registers', async () => {
		// Boot: meetings is on disk but its manifest failed to load — no views,
		// no install record.
		m.pkgKernelStatus.mockResolvedValueOnce(status(['com.ikenga.studio']));
		const { result } = renderHook(() => usePkgActivityBarEntries());
		await waitFor(() => expect(result.current.availablePkgIds).not.toBeNull());
		expect(result.current.availablePkgIds?.has('com.ikenga.meetings')).toBe(false);
		expect(result.current.availablePkgIds?.has('com.ikenga.studio')).toBe(true);

		// Reinstall from the registry → the kernel emits pkg-installed → refetch.
		m.pkgKernelStatus.mockResolvedValue(
			status(['com.ikenga.studio', 'com.ikenga.meetings'], { installed: ['com.ikenga.meetings'] })
		);
		await waitFor(() => expect(handlers.has('pkg-installed')).toBe(true));
		await act(async () => {
			handlers.get('pkg-installed')?.({
				payload: { pkg_id: 'com.ikenga.meetings', version: '0.2.1', installed_at: 1 },
			});
		});
		await waitFor(() =>
			expect(result.current.availablePkgIds?.has('com.ikenga.meetings')).toBe(true)
		);
	});
});
