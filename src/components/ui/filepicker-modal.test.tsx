// On a headless daemon the fs allowlist starts empty and `.` is the daemon's working
// directory, so a picker that opened at `.` opened on "path outside allowlist" with no way
// out. It must start in an allowed folder, and on an allowlist error offer the allowed ones.
// With no allowed folder at all it must never fall back to `.` (which it then committed as a
// project): it offers to add one, or says who can.

import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const invoke = vi.fn();
vi.mock('@/lib/transport', () => ({
	getTransport: () => ({ invoke: (...a: unknown[]) => invoke(...a) }),
}));

import { useDialogStore } from '@/lib/transport/dialog-store';
import { FilepickerModal } from './filepicker-modal';

const ROOTS = ['/home/rex/projects', '/home/rex/royalti-agents'];

function backend(roots: string[]) {
	invoke.mockImplementation(async (cmd: string, args: { path?: string }) => {
		if (cmd === 'fs_roots_list') return roots;
		if (cmd === 'fs_list') {
			const p = args.path ?? '';
			if (roots.some((r) => p === r || p.startsWith(`${r}/`))) {
				return [{ name: 'nso-to', is_dir: true, path: `${p}/nso-to` }];
			}
			throw new Error(`path outside allowlist: ${p === '.' ? '/opt/ikenga' : p}`);
		}
		return null;
	});
}

function open(options: { defaultPath?: string } = {}) {
	act(() => {
		void useDialogStore.getState().requestOpen({ directory: true, ...options });
	});
}

describe('FilepickerModal start folder', () => {
	// No vitest globals here, so RTL's automatic cleanup is not registered: without this,
	// earlier renders stay mounted, all read the same dialog store, and every test sees them.
	afterEach(() => cleanup());

	beforeEach(() => {
		invoke.mockReset();
		useDialogStore.setState({ activeRequest: null });
	});

	it('starts in the first allowed folder, never on the daemon cwd', async () => {
		backend(ROOTS);
		render(<FilepickerModal />);
		open();
		await waitFor(() => expect(screen.getByText('nso-to')).toBeTruthy());
		const listed = invoke.mock.calls.filter((c) => c[0] === 'fs_list').map((c) => c[1].path);
		expect(listed[0]).toBe('/home/rex/projects');
		expect(listed).not.toContain('.');
		expect(screen.queryByTestId('filepicker-error')).toBeNull();
	});

	it('never falls back to "." with no roots: it offers to add a folder instead', async () => {
		let roots: string[] = [];
		invoke.mockImplementation(async (cmd: string, args: { path?: string }) => {
			if (cmd === 'fs_roots_list') return roots;
			if (cmd === 'access_status') throw new Error('no access status');
			if (cmd === 'fs_roots_add') {
				roots = ['/srv/ada/work'];
				return roots;
			}
			if (cmd === 'fs_list' && args.path === '/srv/ada/work') {
				return [{ name: 'album', is_dir: true, path: '/srv/ada/work/album' }];
			}
			throw new Error(`path outside allowlist: ${args.path}`);
		});
		render(<FilepickerModal />);
		open();
		await screen.findByTestId('filepicker-add-folder');
		expect(invoke.mock.calls.some((c) => c[0] === 'fs_list')).toBe(false);
		// "." is never committed: the confirm button is disabled and Enter does nothing.
		const select = screen.getByRole('button', { name: 'Select Folder' }) as HTMLButtonElement;
		expect(select.disabled).toBe(true);
		await userEvent.click(select);
		expect(useDialogStore.getState().activeRequest).not.toBeNull();

		await userEvent.type(screen.getByLabelText('Folder to add'), '/srv/ada/../ada/work');
		await userEvent.click(screen.getByRole('button', { name: 'Add folder' }));
		await waitFor(() => expect(screen.getByText('album')).toBeTruthy());
		expect(invoke.mock.calls.find((c) => c[0] === 'fs_roots_add')?.[1]).toEqual({
			path: '/srv/ada/../ada/work',
		});
		// The canonical folder the server stored is the one opened.
		const listed = invoke.mock.calls.filter((c) => c[0] === 'fs_list').map((c) => c[1].path);
		expect(listed).toEqual(['/srv/ada/work']);
		expect(
			(screen.getByRole('button', { name: 'Select Folder' }) as HTMLButtonElement).disabled
		).toBe(false);
	});

	it('refuses a relative path without asking the server', async () => {
		invoke.mockImplementation(async (cmd: string) => {
			if (cmd === 'fs_roots_list') return [];
			if (cmd === 'access_status') throw new Error('no access status');
			return null;
		});
		render(<FilepickerModal />);
		open();
		await userEvent.type(await screen.findByLabelText('Folder to add'), 'work');
		await userEvent.click(screen.getByRole('button', { name: 'Add folder' }));
		expect((await screen.findByRole('alert')).textContent).toContain('full path');
		expect(invoke.mock.calls.some((c) => c[0] === 'fs_roots_add')).toBe(false);
	});

	it('says who to ask when the caller cannot change the folder list', async () => {
		invoke.mockImplementation(async (cmd: string) => {
			if (cmd === 'fs_roots_list') return [];
			if (cmd === 'access_status') {
				return {
					tier: 't1',
					caps: ['files', 'sessions'],
					credential: { via: 'device', deviceId: 'd1', tier: 'view' },
					share: null,
					principal: { principalId: 'p', username: 'bob', isAdmin: false },
				};
			}
			return null;
		});
		render(<FilepickerModal />);
		open();
		const ask = await screen.findByTestId('filepicker-ask');
		expect(ask.textContent).toContain('Ask an admin of this server');
		expect(screen.queryByLabelText('Folder to add')).toBeNull();
	});

	it('switches to "ask" when the server refuses the add', async () => {
		invoke.mockImplementation(async (cmd: string) => {
			if (cmd === 'fs_roots_list') return [];
			if (cmd === 'access_status') throw new Error('no access status');
			if (cmd === 'fs_roots_add') throw new Error('forbidden: missing=settings');
			return null;
		});
		render(<FilepickerModal />);
		open();
		await userEvent.type(await screen.findByLabelText('Folder to add'), '/srv/x');
		await userEvent.click(screen.getByRole('button', { name: 'Add folder' }));
		const ask = await screen.findByTestId('filepicker-ask');
		expect(ask.textContent).toContain('missing=settings');
	});

	it('honours an explicit defaultPath and does not ask for roots first', async () => {
		backend(ROOTS);
		render(<FilepickerModal />);
		open({ defaultPath: '/home/rex/royalti-agents' });
		await waitFor(() =>
			expect(
				invoke.mock.calls.some(
					(c) => c[0] === 'fs_list' && c[1].path === '/home/rex/royalti-agents'
				)
			).toBe(true)
		);
	});

	it('offers the allowed folders as buttons when a directory is refused', async () => {
		backend(ROOTS);
		render(<FilepickerModal />);
		open({ defaultPath: '/opt/ikenga' });
		const error = await screen.findByTestId('filepicker-error');
		expect(error.textContent).toContain('outside allowlist');
		const buttons = await screen.findAllByTestId('filepicker-root');
		expect(buttons.map((b) => b.textContent)).toEqual(ROOTS);

		await userEvent.click(buttons[1]);
		await waitFor(() => expect(screen.queryByTestId('filepicker-error')).toBeNull());
		expect(invoke.mock.calls.some((c) => c[0] === 'fs_list' && c[1].path === ROOTS[1])).toBe(true);
	});
});
