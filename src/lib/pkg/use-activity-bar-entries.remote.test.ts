// Gap audit rank 20 (UI half): native webviews are desktop-only forever, so a
// browser session must not offer a webview-kind pkg in the rail or Views list.

import { renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({ remote: false }));

vi.mock('@/lib/transport', () => ({
	listen: vi.fn(async () => () => {}),
}));

vi.mock('@/lib/tauri-cmd', () => ({
	pkgKernelStatus: vi.fn(),
	isRemoteWebSession: () => h.remote,
}));

import * as cmd from '@/lib/tauri-cmd';
import { usePkgActivityBarEntries, webviewOnlyPkgIdsOf } from './use-activity-bar-entries';

const m = vi.mocked(cmd);

function view(id: string) {
	return {
		pkg_id: id,
		pkg_name: id,
		qualified_id: `${id}:main`,
		id: 'main',
		title: id,
		route: '/main',
		pane_route: `/pkg/${id}/main`,
		pin_on_install: false,
	};
}

const STATUS = {
	installed: [],
	api_version: 5,
	registries: {
		views: { entries: [view('com.x.web'), view('com.x.frame'), view('com.x.mixed')] },
		activity_bar: { entries: [] },
		sidecar_supervisor: { entries: [] },
		ui_routes: {
			entries: [
				{ pkg_id: 'com.x.web', kind: 'webview' },
				{ pkg_id: 'com.x.frame', kind: 'iframe' },
				{ pkg_id: 'com.x.mixed', kind: 'webview' },
				{ pkg_id: 'com.x.mixed', kind: 'iframe' },
			],
		},
	},
} as unknown as cmd.PkgKernelStatus;

beforeEach(() => {
	h.remote = false;
	m.pkgKernelStatus.mockReset();
	m.pkgKernelStatus.mockResolvedValue(STATUS);
});

describe('webviewOnlyPkgIdsOf', () => {
	it('keeps a pkg that also has an iframe route', () => {
		expect([...webviewOnlyPkgIdsOf(STATUS)]).toEqual(['com.x.web']);
	});
});

describe('usePkgActivityBarEntries in a remote session', () => {
	it('drops webview-only pkgs from the rail and the Views list', async () => {
		h.remote = true;
		const { result } = renderHook(() => usePkgActivityBarEntries());
		await waitFor(() => expect(result.current.loaded).toBe(true));
		expect(result.current.entries.map((e) => e.pkg_id).sort()).toEqual([
			'com.x.frame',
			'com.x.mixed',
		]);
		expect(result.current.views.map((v) => v.pkg_id).sort()).toEqual([
			'com.x.frame',
			'com.x.mixed',
		]);
	});

	it('keeps them on the desktop', async () => {
		const { result } = renderHook(() => usePkgActivityBarEntries());
		await waitFor(() => expect(result.current.loaded).toBe(true));
		expect(result.current.entries.map((e) => e.pkg_id)).toContain('com.x.web');
	});
});
