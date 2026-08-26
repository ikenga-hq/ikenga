import { describe, expect, it, vi, beforeEach } from 'vitest';
import { homeDir, appDataDir, join } from './path-shim';
import DatabaseShim from './sql-shim';
import { open, save, confirm, message } from './dialog-shim';
import { useDialogStore } from './dialog-store';
import { useReauthStore } from './reauth-store';
import { WebRemoteTransport, clearAuthToken } from './index';

describe('transport coverage & shims', () => {
	beforeEach(() => {
		useDialogStore.setState({ activeRequest: null });
		useReauthStore.setState({ isOpen: false, tokenInput: '', errorMsg: null });
		clearAuthToken();
	});

	describe('path-shim', () => {
		it('homeDir returns a string in web mode', async () => {
			const res = await homeDir();
			expect(typeof res).toBe('string');
		});

		it('appDataDir returns app data path', async () => {
			const dir = await appDataDir();
			expect(dir).toContain('app.ikenga');
			// With no daemon reachable (as here) `~` is the honest answer;
			// `path-shim.test.ts` covers the case where fs_home resolves.
			expect(dir).toBe('~/.local/share/app.ikenga');
		});

		it('join joins path components', async () => {
			const joined = await join('/tmp', 'foo', 'bar');
			expect(joined).toBe('/tmp/foo/bar');
		});
	});

	describe('sql-shim', () => {
		it('loads Database proxy in web mode', async () => {
			const db = await DatabaseShim.load('sqlite:test.db');
			expect(db).toBeDefined();
			expect(typeof db.select).toBe('function');
			expect(typeof db.execute).toBe('function');
		});
	});

	describe('dialog-shim & dialog-store', () => {
		it('open requests path browser from store in web mode', async () => {
			const openPromise = open({ title: 'Select File' });
			const active = useDialogStore.getState().activeRequest;
			expect(active).not.toBeNull();
			expect(active?.type).toBe('open');
			expect(active?.options.title).toBe('Select File');

			useDialogStore.getState().closeDialog('/path/to/selected.txt');
			const result = await openPromise;
			expect(result).toBe('/path/to/selected.txt');
		});

		it('save requests path browser from store in web mode', async () => {
			const savePromise = save({ title: 'Save File' });
			const active = useDialogStore.getState().activeRequest;
			expect(active?.type).toBe('save');

			useDialogStore.getState().closeDialog('/path/to/saved.txt');
			const result = await savePromise;
			expect(result).toBe('/path/to/saved.txt');
		});

		it('a second request settles the first instead of stranding it', async () => {
			// Only one picker can be on screen. Replacing the request without
			// resolving leaves the original `await open(...)` pending forever,
			// with no UI left that could ever settle it.
			const first = open({ title: 'First' });
			const second = open({ title: 'Second' });

			await expect(first).resolves.toBeNull();
			expect(useDialogStore.getState().activeRequest?.options.title).toBe('Second');

			useDialogStore.getState().closeDialog('/chosen');
			await expect(second).resolves.toBe('/chosen');
		});

		it('confirm returns false when the host cannot show a dialog', async () => {
			// jsdom (and any embedder that blocks modals) returns undefined
			// rather than throwing. That must read as "not confirmed" — a
			// coerced-truthy undefined would be an accidental yes on a
			// destructive prompt.
			const original = window.confirm;
			// @ts-expect-error — deliberately modelling the blocked-modal host
			window.confirm = undefined;
			await expect(confirm('Delete everything?')).resolves.toBe(false);
			window.confirm = original;
		});

		it('confirm passes a real answer through', async () => {
			const original = window.confirm;
			window.confirm = vi.fn().mockReturnValue(true);
			await expect(confirm('Proceed?')).resolves.toBe(true);
			window.confirm = vi.fn().mockReturnValue(false);
			await expect(confirm('Proceed?')).resolves.toBe(false);
			window.confirm = original;
		});

		it('message falls back to the console when alert is unavailable', async () => {
			const original = window.alert;
			const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
			// @ts-expect-error — deliberately modelling the blocked-modal host
			window.alert = undefined;
			await message('Hello world');
			expect(warn).toHaveBeenCalledWith(expect.stringContaining('Hello world'));
			warn.mockRestore();
			window.alert = original;
		});
	});

	describe('WebRemoteTransport & reauth', () => {
		it('shows reauth overlay on 401 HTTP response', async () => {
			const originalFetch = global.fetch;
			global.fetch = vi.fn().mockResolvedValue({
				ok: false,
				status: 401,
				statusText: 'Unauthorized',
			});

			const transport = new WebRemoteTransport();
			await expect(transport.invoke('test_cmd')).rejects.toThrow('HTTP RPC error: 401');

			expect(useReauthStore.getState().isOpen).toBe(true);
			global.fetch = originalFetch;
		});

		it('reconnect validates token via RPC ping', async () => {
			const originalFetch = global.fetch;
			global.fetch = vi.fn().mockResolvedValue({
				ok: true,
				json: async () => ({ ok: true, data: [] }),
			});

			useReauthStore.setState({ isOpen: true, tokenInput: 'valid_token' });
			if (typeof window !== 'undefined') {
				Object.defineProperty(window, 'location', {
					writable: true,
					value: { reload: vi.fn(), href: 'http://localhost/' },
				});
			}

			const success = await useReauthStore.getState().reconnect('valid_token');
			expect(success).toBe(true);

			global.fetch = originalFetch;
		});
	});
});
