import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ClipboardUnavailableError, readClipboardText, writeClipboardText } from './shims';

function setClipboard(value: unknown) {
	Object.defineProperty(navigator, 'clipboard', { value, configurable: true });
}

function setExecCommand(fn: ((cmd: string) => boolean) | undefined) {
	Object.defineProperty(document, 'execCommand', { value: fn, configurable: true, writable: true });
}

describe('clipboard shims in a browser session', () => {
	beforeEach(() => {
		vi.spyOn(console, 'warn').mockImplementation(() => {});
	});
	afterEach(() => {
		setClipboard(undefined);
		setExecCommand(undefined);
		vi.restoreAllMocks();
	});

	it('writes through navigator.clipboard when it exists', async () => {
		const writeText = vi.fn().mockResolvedValue(undefined);
		setClipboard({ writeText });
		const exec = vi.fn();
		setExecCommand(exec);
		await writeClipboardText('hello');
		expect(writeText).toHaveBeenCalledWith('hello');
		expect(exec).not.toHaveBeenCalled();
	});

	it('falls back to execCommand("copy") when navigator.clipboard is missing (insecure origin)', async () => {
		setClipboard(undefined);
		let copied: string | null = null;
		setExecCommand((cmd) => {
			const el = document.activeElement as HTMLTextAreaElement;
			if (cmd === 'copy' && el instanceof HTMLTextAreaElement) copied = el.value;
			return true;
		});
		await writeClipboardText('over http');
		expect(copied).toBe('over http');
		// The scratch textarea is removed again.
		expect(document.querySelector('textarea')).toBeNull();
	});

	it('falls back to execCommand when navigator.clipboard.writeText rejects', async () => {
		setClipboard({ writeText: vi.fn().mockRejectedValue(new Error('NotAllowedError')) });
		const exec = vi.fn().mockReturnValue(true);
		setExecCommand(exec);
		await writeClipboardText('x');
		expect(exec).toHaveBeenCalledWith('copy');
	});

	it('returns focus to the element that had it', async () => {
		const input = document.createElement('input');
		document.body.appendChild(input);
		input.focus();
		setExecCommand(() => true);
		await writeClipboardText('x');
		expect(document.activeElement).toBe(input);
		input.remove();
	});

	it('throws a typed ClipboardUnavailableError when there is no API and execCommand fails', async () => {
		setClipboard(undefined);
		setExecCommand(() => false);
		const err = await writeClipboardText('x').catch((e) => e);
		expect(err).toBeInstanceOf(ClipboardUnavailableError);
		expect(err.operation).toBe('write');
	});

	it('throws the typed error when execCommand itself is unavailable', async () => {
		setClipboard(undefined);
		setExecCommand(undefined);
		await expect(writeClipboardText('x')).rejects.toBeInstanceOf(ClipboardUnavailableError);
	});

	it('throws the typed error when both the API and execCommand fail', async () => {
		setClipboard({ writeText: vi.fn().mockRejectedValue(new Error('denied')) });
		setExecCommand(() => {
			throw new Error('nope');
		});
		await expect(writeClipboardText('x')).rejects.toBeInstanceOf(ClipboardUnavailableError);
	});

	it('reads through navigator.clipboard when it exists', async () => {
		setClipboard({ readText: vi.fn().mockResolvedValue('pasted') });
		await expect(readClipboardText()).resolves.toBe('pasted');
	});

	it('read throws ClipboardUnavailableError when there is no clipboard API', async () => {
		setClipboard(undefined);
		const err = await readClipboardText().catch((e) => e);
		expect(err).toBeInstanceOf(ClipboardUnavailableError);
		expect(err.operation).toBe('read');
	});

	it('read wraps a refused permission in the typed error', async () => {
		setClipboard({ readText: vi.fn().mockRejectedValue(new Error('NotAllowedError')) });
		await expect(readClipboardText()).rejects.toBeInstanceOf(ClipboardUnavailableError);
	});
});
