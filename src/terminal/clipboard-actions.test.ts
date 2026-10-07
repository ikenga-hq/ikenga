import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const writeClipboardText = vi.fn();
vi.mock('@/lib/transport/shims', () => ({
	writeClipboardText: (t: string) => writeClipboardText(t),
}));

import { useToastStore } from '@/lib/toast';
import { handleOsc52, handleTerminalCopyKey, openTerminalUrl } from './clipboard-actions';

const ev = () =>
	new KeyboardEvent('keydown', { key: 'C', ctrlKey: true, shiftKey: true, cancelable: true });
const b64 = (s: string) => btoa(String.fromCharCode(...new TextEncoder().encode(s)));

describe('handleTerminalCopyKey', () => {
	it('copies the selection and preventDefaults on Windows/Linux (Chrome inspector)', () => {
		const e = ev();
		const copy = vi.fn();
		expect(handleTerminalCopyKey(e, { selection: 'sel', mac: false, copy })).toBe(false);
		expect(copy).toHaveBeenCalledWith('sel');
		expect(e.defaultPrevented).toBe(true);
	});

	it('preventDefaults on Windows/Linux even with no selection', () => {
		const e = ev();
		const copy = vi.fn();
		expect(handleTerminalCopyKey(e, { selection: '', mac: false, copy })).toBe(false);
		expect(copy).not.toHaveBeenCalled();
		expect(e.defaultPrevented).toBe(true);
	});

	it('on macOS with no selection falls through to the PTY (SIGINT) untouched', () => {
		const e = ev();
		expect(handleTerminalCopyKey(e, { selection: '', mac: true, copy: vi.fn() })).toBe(true);
		expect(e.defaultPrevented).toBe(false);
	});

	it('on macOS with a selection copies and swallows without preventDefault', () => {
		const e = ev();
		const copy = vi.fn();
		expect(handleTerminalCopyKey(e, { selection: 'x', mac: true, copy })).toBe(false);
		expect(copy).toHaveBeenCalledWith('x');
		expect(e.defaultPrevented).toBe(false);
	});
});

describe('handleOsc52', () => {
	beforeEach(() => {
		useToastStore.setState({ queue: [] });
		writeClipboardText.mockReset();
	});
	afterEach(() => vi.restoreAllMocks());

	it('writes the decoded text', async () => {
		const write = vi.fn().mockResolvedValue(undefined);
		handleOsc52(`c;${b64('héllo')}`, { write });
		await Promise.resolve();
		expect(write).toHaveBeenCalledWith('héllo');
		expect(useToastStore.getState().queue).toHaveLength(0);
	});

	it('ignores read queries and empty/malformed payloads', () => {
		const write = vi.fn().mockResolvedValue(undefined);
		handleOsc52('c;?', { write });
		handleOsc52('c;', { write });
		handleOsc52('c;***not base64***', { write });
		expect(write).not.toHaveBeenCalled();
	});

	it('on failure raises a toast whose Copy action retries inside a real gesture', async () => {
		vi.spyOn(console, 'warn').mockImplementation(() => {});
		const write = vi.fn().mockRejectedValue(new Error('NotAllowedError'));
		handleOsc52(`c;${b64('secret text')}`, { write });
		await new Promise((r) => setTimeout(r, 0));
		const [t] = useToastStore.getState().queue;
		expect(t.action?.label).toBe('Copy');
		expect(t.label).toMatch(/blocked/);

		writeClipboardText.mockResolvedValue(undefined);
		await t.action?.run();
		expect(writeClipboardText).toHaveBeenCalledWith('secret text');
	});
});

describe('openTerminalUrl', () => {
	it('opens with noopener,noreferrer', () => {
		const open = vi.spyOn(window, 'open').mockImplementation(() => null);
		openTerminalUrl('https://example.com/x');
		expect(open).toHaveBeenCalledWith('https://example.com/x', '_blank', 'noopener,noreferrer');
		open.mockRestore();
	});
});
