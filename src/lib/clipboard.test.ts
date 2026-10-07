import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const writeClipboardText = vi.fn();
vi.mock('@/lib/transport/shims', () => ({
	writeClipboardText: (t: string) => writeClipboardText(t),
}));

import { COPY_FAILED_LABEL, copyText } from './clipboard';
import { useToastStore } from './toast';

describe('copyText', () => {
	beforeEach(() => {
		useToastStore.setState({ queue: [] });
		writeClipboardText.mockReset();
		vi.spyOn(console, 'warn').mockImplementation(() => {});
	});
	afterEach(() => vi.restoreAllMocks());

	it('resolves true and stays quiet on success without a label', async () => {
		writeClipboardText.mockResolvedValue(undefined);
		await expect(copyText('abc')).resolves.toBe(true);
		expect(writeClipboardText).toHaveBeenCalledWith('abc');
		expect(useToastStore.getState().queue).toHaveLength(0);
	});

	it('toasts the success label only after the write finished', async () => {
		let release!: () => void;
		writeClipboardText.mockReturnValue(new Promise<void>((res) => (release = res)));
		const p = copyText('abc', { successLabel: 'Copied path' });
		await Promise.resolve();
		expect(useToastStore.getState().queue).toHaveLength(0);
		release();
		await expect(p).resolves.toBe(true);
		expect(useToastStore.getState().queue.map((t) => t.label)).toEqual(['Copied path']);
	});

	it('toasts an error and resolves false when the write fails', async () => {
		writeClipboardText.mockRejectedValue(new Error('denied'));
		await expect(copyText('abc', { successLabel: 'Copied' })).resolves.toBe(false);
		const [t] = useToastStore.getState().queue;
		expect(t.label).toBe(COPY_FAILED_LABEL);
		expect(t.variant).toBe('error');
		expect(useToastStore.getState().queue).toHaveLength(1);
	});

	it('honours a custom failure label', async () => {
		writeClipboardText.mockRejectedValue(new Error('denied'));
		await copyText('abc', { failureLabel: 'Nope' });
		expect(useToastStore.getState().queue[0].label).toBe('Nope');
	});
});
