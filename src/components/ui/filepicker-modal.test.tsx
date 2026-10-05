// On a headless daemon the fs allowlist starts empty and `.` is the daemon's working
// directory, so a picker that opened at `.` opened on "path outside allowlist" with no way
// out. It must start in an allowed folder, and on an allowlist error offer the allowed ones.

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

	it('falls back to "." when the backend has no roots (desktop behaviour unchanged)', async () => {
		invoke.mockImplementation(async (cmd: string) => (cmd === 'fs_roots_list' ? [] : []));
		render(<FilepickerModal />);
		open();
		await waitFor(() =>
			expect(invoke.mock.calls.some((c) => c[0] === 'fs_list' && c[1].path === '.')).toBe(true)
		);
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
