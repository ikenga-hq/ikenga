// Gap audit rank 8 — no viewer server exists in a browser session, so it can
// never be "stopped": no probe, no stopped banner.
import { renderHook, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({
	remote: false,
	viewerPort: vi.fn(async () => null as number | null),
}));
vi.mock('@/lib/tauri-cmd', () => ({
	isRemoteWebSession: () => h.remote,
	viewerPort: h.viewerPort,
}));

import { useViewerServerHealth } from './use-viewer-server-health';

afterEach(() => {
	h.remote = false;
	h.viewerPort.mockClear();
});

describe('useViewerServerHealth in a remote session', () => {
	it('never probes and never reports stopped', async () => {
		h.remote = true;
		const { result } = renderHook(() => useViewerServerHealth('/p/a.html'));
		await Promise.resolve();
		expect(result.current.stopped).toBe(false);
		expect(h.viewerPort).not.toHaveBeenCalled();
	});

	it('still reports stopped on the desktop when the port is null', async () => {
		const { result } = renderHook(() => useViewerServerHealth('/p/a.html'));
		await waitFor(() => expect(result.current.stopped).toBe(true));
	});
});
