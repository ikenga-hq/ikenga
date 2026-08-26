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
		it('homeDir returns ~ in web mode', async () => {
			const res = await homeDir();
			expect(typeof res).toBe('string');
		});

		it('appDataDir returns app data path', async () => {
			const dir = await appDataDir();
			expect(dir).toContain('app.ikenga');
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

		it('confirm falls back gracefully', async () => {
			const res = await confirm('Are you sure?');
			expect(typeof res).toBe('boolean');
		});

		it('message executes without error', async () => {
			await message('Hello world');
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
