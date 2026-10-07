// Settings → Storage → "Folders you can open": the caller's own fs allowlist
// with add / remove / reset, read-only with who-to-ask when the caller can't
// change it, and an admin's view of someone else's list on a T1 server.

import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const cmd = vi.hoisted(() => ({
	fsRootsList: vi.fn(),
	fsRootsAdd: vi.fn(),
	fsRootsRemove: vi.fn(),
	fsRootsReset: vi.fn(),
}));
const access = vi.hoisted(() => ({ accessStatus: vi.fn() }));

vi.mock('@/lib/tauri-cmd', () => cmd);
vi.mock('@/lib/access/client', () => access);

import { FsAllowlistSectionBody } from './fs-allowlist';

const ALL = ['files', 'sessions', 'dispatch', 'approve', 'install', 'settings', 'secrets'];

function status(over: Record<string, unknown> = {}) {
	return {
		tier: 't1',
		store: 'ok',
		principal: { principalId: 'p-ada', username: 'ada', isAdmin: false },
		credential: { via: 'session', deviceId: null, tier: 'full' },
		caps: ALL,
		adminStrength: true,
		publicUrl: null,
		sharingEnabled: true,
		share: null,
		...over,
	};
}

/** An in-memory server: one list per principal (`undefined` = the caller). */
function server(lists: Record<string, string[]>) {
	const key = (p?: string) => p ?? 'self';
	cmd.fsRootsList.mockImplementation(async (p?: string) => [...(lists[key(p)] ?? [])]);
	cmd.fsRootsAdd.mockImplementation(async (path: string, p?: string) => {
		lists[key(p)] = [...(lists[key(p)] ?? []), path];
		return [...lists[key(p)]];
	});
	cmd.fsRootsRemove.mockImplementation(async (path: string, p?: string) => {
		lists[key(p)] = (lists[key(p)] ?? []).filter((r) => r !== path);
		return [...lists[key(p)]];
	});
	cmd.fsRootsReset.mockImplementation(async (p?: string) => {
		lists[key(p)] = ['/srv/home'];
		return [...lists[key(p)]];
	});
}

const roots = () => screen.queryAllByTestId('fs-allowlist-root').map((r) => r.textContent);

describe('FsAllowlistSectionBody', () => {
	afterEach(() => cleanup());
	beforeEach(() => {
		for (const f of Object.values(cmd)) f.mockReset();
		access.accessStatus.mockReset();
	});

	it("edits the caller's own list", async () => {
		server({ self: ['/srv/ada'] });
		access.accessStatus.mockResolvedValue(status());
		render(<FsAllowlistSectionBody />);
		await waitFor(() => expect(roots()).toEqual(['/srv/ada']));
		expect(screen.getByText(/started with your home folder/)).toBeTruthy();

		await userEvent.type(screen.getByLabelText('Folder to add'), '/srv/shared');
		await userEvent.click(screen.getByRole('button', { name: /Add folder/ }));
		await waitFor(() => expect(roots()).toEqual(['/srv/ada', '/srv/shared']));
		expect(cmd.fsRootsAdd).toHaveBeenCalledWith('/srv/shared', undefined);

		await userEvent.click(screen.getByRole('button', { name: 'Remove /srv/ada' }));
		await waitFor(() => expect(roots()).toEqual(['/srv/shared']));
		await userEvent.click(screen.getByRole('button', { name: 'Remove /srv/shared' }));
		await screen.findByTestId('fs-allowlist-empty');

		await userEvent.click(screen.getByRole('button', { name: /Reset/ }));
		await waitFor(() => expect(roots()).toEqual(['/srv/home']));
		// Not an admin: no one else's list.
		expect(screen.queryByTestId('fs-allowlist-admin')).toBeNull();
	});

	it('refuses a relative path without asking the server, and shows a server error', async () => {
		server({ self: [] });
		access.accessStatus.mockResolvedValue(status());
		render(<FsAllowlistSectionBody />);
		await screen.findByTestId('fs-allowlist-empty');
		await userEvent.type(screen.getByLabelText('Folder to add'), 'work');
		await userEvent.click(screen.getByRole('button', { name: /Add folder/ }));
		expect((await screen.findByRole('alert')).textContent).toContain('full path');
		expect(cmd.fsRootsAdd).not.toHaveBeenCalled();

		cmd.fsRootsAdd.mockRejectedValueOnce(new Error('fs_roots_add: /nope does not exist'));
		await userEvent.clear(screen.getByLabelText('Folder to add'));
		await userEvent.type(screen.getByLabelText('Folder to add'), '/nope');
		await userEvent.click(screen.getByRole('button', { name: /Add folder/ }));
		await waitFor(() =>
			expect(screen.getByRole('alert').textContent).toContain('/nope does not exist')
		);
	});

	it('is read-only, saying who to ask, when the caller cannot change it', async () => {
		server({ self: ['/srv/bob'] });
		access.accessStatus.mockResolvedValue(
			status({
				caps: ['files', 'sessions'],
				credential: { via: 'device', deviceId: 'd1', tier: 'view' },
				adminStrength: false,
			})
		);
		render(<FsAllowlistSectionBody />);
		await waitFor(() => expect(roots()).toEqual(['/srv/bob']));
		expect(screen.getByTestId('fs-allowlist-ask').textContent).toContain(
			'Ask an admin of this server'
		);
		expect(screen.queryByLabelText('Folder to add')).toBeNull();
		expect(screen.queryByRole('button', { name: 'Remove /srv/bob' })).toBeNull();
	});

	it('turns read-only when the server refuses an edit', async () => {
		server({ self: ['/srv/ada'] });
		access.accessStatus.mockRejectedValue(new Error('no access status'));
		cmd.fsRootsRemove.mockRejectedValue(new Error('forbidden: missing=settings'));
		render(<FsAllowlistSectionBody />);
		await waitFor(() => expect(roots()).toEqual(['/srv/ada']));
		await userEvent.click(screen.getByRole('button', { name: 'Remove /srv/ada' }));
		const ask = await screen.findByTestId('fs-allowlist-ask');
		expect(ask.textContent).toContain('missing=settings');
	});

	it("lets an admin manage someone else's list by username", async () => {
		server({ self: ['/srv/ada'], bob: ['/srv/bob'] });
		access.accessStatus.mockResolvedValue(
			status({ principal: { principalId: 'p-ada', username: 'ada', isAdmin: true } })
		);
		render(<FsAllowlistSectionBody />);
		const admin = await screen.findByTestId('fs-allowlist-admin');
		await userEvent.type(screen.getByLabelText('Username'), 'bob');
		await userEvent.click(screen.getByRole('button', { name: 'Show folders' }));
		await waitFor(() => expect(roots()).toEqual(['/srv/ada', '/srv/bob']));
		expect(cmd.fsRootsList).toHaveBeenCalledWith('bob');

		const inputs = screen.getAllByLabelText('Folder to add');
		await userEvent.type(inputs[1], '/srv/team');
		const adds = screen.getAllByRole('button', { name: /Add folder/ });
		await userEvent.click(adds[1]);
		await waitFor(() => expect(roots()).toEqual(['/srv/ada', '/srv/bob', '/srv/team']));
		expect(cmd.fsRootsAdd).toHaveBeenCalledWith('/srv/team', 'bob');
		expect(admin).toBeTruthy();
	});
});
