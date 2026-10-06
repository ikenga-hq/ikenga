// Audit 2026-10-06 rank 5: in a browser tab the iyke instrumentation pushed
// its console/network buffers to `iyke_*_push`, which the daemon does not
// serve. The fetch shim then recorded that failing `/api/rpc` POST, the batch
// was requeued, and the loop ran at ~3.4 requests a second on every screen.
// The instrumentation is desktop-only now, and the bridge never records its
// own transport traffic.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const transport = vi.hoisted(() => ({
	tauri: false,
	invoke: vi.fn(async (..._args: unknown[]) => undefined),
}));

vi.mock('@/lib/transport', async (importOriginal) => {
	const actual = await importOriginal<typeof import('@/lib/transport')>();
	return {
		...actual,
		isTauri: () => transport.tauri,
		invoke: transport.invoke,
	};
});

const PATCH_FLAG = Symbol.for('@royalti/iyke-bridge/patched');
const realFetch = globalThis.fetch;
const realConsole = { ...console };

function pushCalls(): string[] {
	return transport.invoke.mock.calls
		.map((c) => c[0] as string)
		.filter((cmd) => cmd === 'iyke_log_push' || cmd === 'iyke_network_push');
}

beforeEach(() => {
	vi.useFakeTimers();
	vi.resetModules();
	transport.invoke.mockClear();
	delete (globalThis as Record<symbol, unknown>)[PATCH_FLAG];
	window.fetch = vi.fn(async () => new Response('{}', { status: 401 })) as typeof fetch;
});

afterEach(() => {
	vi.useRealTimers();
	window.fetch = realFetch;
	Object.assign(console, realConsole);
	delete (globalThis as Record<symbol, unknown>)[PATCH_FLAG];
});

describe('iyke instrumentation in a remote (browser) session', () => {
	it('makes zero iyke_*_push calls', async () => {
		transport.tauri = false;
		const stubbedFetch = window.fetch;
		const bridge = await import('./bridge');
		bridge.installInstrumentation();

		expect(window.fetch).toBe(stubbedFetch);
		console.log('hello from a browser tab');
		console.error('and an error');
		await window.fetch('/api/rpc', { method: 'POST' });
		await window.fetch('/elsewhere');
		await vi.advanceTimersByTimeAsync(5_000);

		expect(pushCalls()).toEqual([]);
	});

	it('does not forward iframe logs or network entries', async () => {
		transport.tauri = false;
		const { installIykeIframeMessageListener } = await import('./iframe-registry');
		installIykeIframeMessageListener();
		for (const kind of ['logs', 'network']) {
			window.dispatchEvent(
				new MessageEvent('message', {
					data: { __iyke: true, kind, payload: [{ ts: 1, level: 'log', message: 'x' }] },
				})
			);
		}
		await vi.advanceTimersByTimeAsync(1_000);
		expect(pushCalls()).toEqual([]);
	});
});

describe('iyke instrumentation on the desktop', () => {
	it('records ordinary fetches but never the transport endpoint itself', async () => {
		transport.tauri = true;
		const bridge = await import('./bridge');
		bridge.installInstrumentation();

		await window.fetch('/api/rpc', { method: 'POST' });
		await window.fetch('https://example.test/data');
		await vi.advanceTimersByTimeAsync(1_000);

		const netCalls = transport.invoke.mock.calls.filter((c) => c[0] === 'iyke_network_push');
		const urls = netCalls.flatMap((c) =>
			(c[1] as { entries: Array<{ url: string }> }).entries.map((e) => e.url)
		);
		expect(urls).toEqual(['https://example.test/data']);
	});

	it('treats /api/rpc as bridge traffic', async () => {
		const { isIykeIpc } = await import('./bridge');
		expect(isIykeIpc('/api/rpc')).toBe(true);
		expect(isIykeIpc('http://host:7777/api/rpc')).toBe(true);
		expect(isIykeIpc('https://example.test/data')).toBe(false);
	});
});
