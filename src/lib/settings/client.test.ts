import { afterEach, describe, expect, it, vi } from 'vitest';

const { listenMock } = vi.hoisted(() => ({ listenMock: vi.fn() }));

vi.mock('@/lib/tauri-cmd', () => ({
	listen: listenMock,
	settingsOpenFile: vi.fn(),
	settingsReadFile: vi.fn(),
	settingsWriteField: vi.fn(),
}));

import { watchSettings } from './client';

describe('settings client watcher', () => {
	afterEach(() => {
		vi.useRealTimers();
		listenMock.mockReset();
	});

	it('cancels a pending callback when unsubscribed', async () => {
		vi.useFakeTimers();
		const unlisten = vi.fn();
		let handler: (() => void) | undefined;
		listenMock.mockImplementation(async (_event: string, callback: () => void) => {
			handler = callback;
			return unlisten;
		});
		const onChange = vi.fn();
		const stop = await watchSettings(onChange);
		handler?.();
		stop();
		vi.advanceTimersByTime(100);
		expect(onChange).not.toHaveBeenCalled();
		expect(unlisten).toHaveBeenCalledOnce();
	});
});
