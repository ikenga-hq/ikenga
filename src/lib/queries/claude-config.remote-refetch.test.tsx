// Gap audit rank 24 stopgap — a browser session has no claude-config watcher
// events, so the config / store / Ngwa-snapshot queries refetch when the tab
// regains focus. The desktop leaves it to the FS watcher.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { renderHook } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({ remote: false }));

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	isRemoteWebSession: () => h.remote,
}));

import { useRemoteConfigFocusRefetch } from './claude-config';

function setup() {
	const qc = new QueryClient();
	const spy = vi.spyOn(qc, 'refetchQueries').mockResolvedValue(undefined);
	const wrapper = ({ children }: { children: ReactNode }) => (
		<QueryClientProvider client={qc}>{children}</QueryClientProvider>
	);
	const hook = renderHook(() => useRemoteConfigFocusRefetch(), { wrapper });
	return { spy, hook };
}

afterEach(() => {
	h.remote = false;
});

describe('useRemoteConfigFocusRefetch', () => {
	it('refetches the config, store and snapshot queries on window focus when remote', () => {
		h.remote = true;
		const { spy, hook } = setup();
		window.dispatchEvent(new Event('focus'));
		const keys = spy.mock.calls.map((c) => (c[0] as { queryKey: readonly string[] }).queryKey[0]);
		expect(keys).toEqual(['claude_config', 'claude_store', 'ngwa']);
		expect(spy.mock.calls[0][0]).toMatchObject({ stale: true, type: 'active' });
		hook.unmount();
		spy.mockClear();
		window.dispatchEvent(new Event('focus'));
		expect(spy).not.toHaveBeenCalled();
	});

	it('does nothing on the desktop', () => {
		const { spy } = setup();
		window.dispatchEvent(new Event('focus'));
		expect(spy).not.toHaveBeenCalled();
	});
});
