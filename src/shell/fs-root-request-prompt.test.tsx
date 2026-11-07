// The Allow / Deny prompt behind `POST /iyke/fs-roots/add`: a folder is only
// added when the user clicks Allow; every other way out answers Deny.

import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { FsRootAddRequest } from '@/lib/tauri-cmd';

const bridge = vi.hoisted(() => ({
	handler: null as null | ((e: { payload: FsRootAddRequest }) => void),
	unlisten: vi.fn(),
	done: vi.fn(async () => undefined),
	tauri: true,
}));

vi.mock('@/lib/tauri-cmd', () => ({
	listen: vi.fn(async (event: string, cb: (e: { payload: FsRootAddRequest }) => void) => {
		if (event === 'iyke://fs-roots-add-request') bridge.handler = cb;
		return bridge.unlisten;
	}),
	iykeFsRootsRequestDone: (...a: unknown[]) =>
		(bridge.done as (...x: unknown[]) => Promise<void>)(...a),
}));
vi.mock('@/lib/transport', () => ({ isTauri: () => bridge.tauri }));

import { FsRootRequestPrompt } from './fs-root-request-prompt';

function ask(over: Partial<FsRootAddRequest> = {}) {
	act(() => {
		bridge.handler?.({
			payload: {
				request_id: 'r1',
				path: '~/Documents/artifacts',
				resolved: 'C:\\Users\\ada\\Documents\\artifacts',
				reason: 'open the akuko test artifact',
				...over,
			},
		});
	});
}

describe('FsRootRequestPrompt', () => {
	beforeEach(() => {
		bridge.handler = null;
		bridge.tauri = true;
		bridge.done.mockClear();
		bridge.unlisten.mockClear();
	});
	afterEach(() => cleanup());

	it('shows the resolved folder and reason, and Allow answers true', async () => {
		render(<FsRootRequestPrompt />);
		await waitFor(() => expect(bridge.handler).not.toBeNull());
		expect(screen.queryByTestId('fs-root-request')).toBeNull();

		ask();
		expect(screen.getByTestId('fs-root-request-folder').textContent).toBe(
			'C:\\Users\\ada\\Documents\\artifacts'
		);
		expect(screen.getByText(/Asked for as/).textContent).toContain('~/Documents/artifacts');
		expect(screen.getByTestId('fs-root-request-reason').textContent).toContain('akuko');

		await userEvent.click(screen.getByRole('button', { name: 'Allow' }));
		expect(bridge.done).toHaveBeenCalledWith('r1', true);
		await waitFor(() => expect(screen.queryByTestId('fs-root-request')).toBeNull());
	});

	it('Deny and Escape both answer false; queued requests take their turn', async () => {
		render(<FsRootRequestPrompt />);
		await waitFor(() => expect(bridge.handler).not.toBeNull());
		ask({ request_id: 'r1', path: 'D:\\media', resolved: 'D:\\media', reason: null });
		ask({ request_id: 'r2', resolved: 'E:\\other', path: 'E:\\other' });
		expect(screen.getByText(/1 more request waiting/)).toBeTruthy();
		expect(screen.queryByText(/Asked for as/)).toBeNull();
		expect(screen.queryByTestId('fs-root-request-reason')).toBeNull();

		await userEvent.click(screen.getByRole('button', { name: 'Deny' }));
		expect(bridge.done).toHaveBeenLastCalledWith('r1', false);
		await waitFor(() =>
			expect(screen.getByTestId('fs-root-request-folder').textContent).toBe('E:\\other')
		);

		await userEvent.keyboard('{Escape}');
		await waitFor(() => expect(bridge.done).toHaveBeenLastCalledWith('r2', false));
		expect(bridge.done).toHaveBeenCalledTimes(2);
	});

	it('does not listen outside the desktop app', async () => {
		bridge.tauri = false;
		render(<FsRootRequestPrompt />);
		await new Promise((r) => setTimeout(r, 20));
		expect(bridge.handler).toBeNull();
	});
});
