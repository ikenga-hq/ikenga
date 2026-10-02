// G-ACCESS §4.5.2 (WP-76): in share mode every RPC carries `X-Ikenga-Share`
// and every WebSocket URL `?share=`; the selector never authenticates.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
	__resetShareModeForTests,
	setShareMode,
	shareQueryParam,
	WebRemoteTransport,
} from './index';

const KEY = '01890a5d-ac96-774b-bcce-b302099a8057/royalti-co';
const fetchMock = vi.fn<typeof fetch>();

beforeEach(() => {
	sessionStorage.clear();
	__resetShareModeForTests();
	fetchMock.mockReset();
	fetchMock.mockImplementation(
		async () => new Response(JSON.stringify({ ok: true, data: 1 }), { status: 200 })
	);
	vi.stubGlobal('fetch', fetchMock);
});

afterEach(() => {
	vi.unstubAllGlobals();
	setShareMode(null);
});

describe('share mode', () => {
	it('adds X-Ikenga-Share to every RPC only while a share is open', async () => {
		const t = new WebRemoteTransport();
		await t.invoke('project_list');
		expect(
			(fetchMock.mock.calls[0]![1]!.headers as Record<string, string>)['X-Ikenga-Share']
		).toBeUndefined();
		setShareMode({
			projectKey: KEY,
			projectId: 'royalti-co',
			projectName: 'royalti-co',
			ownerUsername: 'ada',
			role: 'reviewer',
			scope: 'project',
		});
		await t.invoke('project_list');
		expect((fetchMock.mock.calls[1]![1]!.headers as Record<string, string>)['X-Ikenga-Share']).toBe(
			KEY
		);
		expect(shareQueryParam()).toBe(`share=${encodeURIComponent(KEY)}`);
	});

	it('survives a reload in the same tab (sessionStorage)', async () => {
		setShareMode({
			projectKey: KEY,
			projectId: 'royalti-co',
			projectName: 'royalti-co',
			ownerUsername: null,
			role: 'guest',
			scope: 'artifact',
			artifactPath: 'plans/board.html',
		});
		__resetShareModeForTests();
		const { currentShare } = await import('./index');
		expect(currentShare()?.role).toBe('guest');
	});
});
