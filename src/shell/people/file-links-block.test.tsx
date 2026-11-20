// WP-17: the Links panel is hidden unless file links are on, lists the
// caller's links with Revoke, and shows the admin view to admins only.

import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
	accessStatus: vi.fn(),
	accessFileLinksList: vi.fn(),
	accessFileLinkRevoke: vi.fn(),
	accessFileLinksAdminList: vi.fn(),
	accessFileLinkAdminRevoke: vi.fn(),
}));

vi.mock('@/lib/access/client', async (orig) => ({
	...(await orig<typeof import('@/lib/access/client')>()),
	...mocks,
}));

import type { AccessStatus, FileLinkView } from '@/lib/access/client';

import { FileLinksBlock } from './file-links-block';

const status = (over: Partial<AccessStatus> = {}, admin = false): AccessStatus =>
	({
		tier: 't1',
		store: 'ok',
		principal: { principalId: 'p', username: 'ada', isAdmin: admin },
		credential: { via: 'session', deviceId: null, tier: 'full' },
		caps: [],
		adminStrength: true,
		publicUrl: 'https://box.example',
		sharingEnabled: true,
		fileLinksEnabled: true,
		share: null,
		...over,
	}) as AccessStatus;

const link = (over: Partial<FileLinkView> = {}): FileLinkView => ({
	linkId: 'l1',
	name: 'deck.pdf',
	path: '/home/ada/deck.pdf',
	previewKind: 'pdf',
	createdAt: 1,
	expiresAt: null,
	revokedAt: null,
	revokedReason: null,
	hasPasscode: false,
	lockedUntil: null,
	openCount: 3,
	downloadCount: 1,
	firstOpenedAt: 2,
	lastOpenedAt: 3,
	state: 'live',
	...over,
});

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

describe('FileLinksBlock', () => {
	it('renders nothing while file links are off', async () => {
		mocks.accessStatus.mockResolvedValue(status({ fileLinksEnabled: false }));
		const { container } = render(<FileLinksBlock />);
		await waitFor(() => expect(mocks.accessStatus).toHaveBeenCalled());
		expect(container.querySelector('[data-state="file-links"]')).toBeNull();
		expect(mocks.accessFileLinksList).not.toHaveBeenCalled();
	});

	it('lists my links and revokes one after a confirm', async () => {
		mocks.accessStatus.mockResolvedValue(status());
		mocks.accessFileLinksList.mockResolvedValue({ links: [link()] });
		mocks.accessFileLinkRevoke.mockResolvedValue({ revoked: true });
		render(<FileLinksBlock />);
		expect(await screen.findByText('deck.pdf')).toBeTruthy();
		fireEvent.click(screen.getByRole('button', { name: 'Revoke' }));
		fireEvent.click(screen.getByRole('button', { name: 'Revoke now' }));
		await waitFor(() => expect(mocks.accessFileLinkRevoke).toHaveBeenCalledWith('l1'));
		expect(mocks.accessFileLinksAdminList).not.toHaveBeenCalled();
	});

	it('shows no Revoke for an ended link', async () => {
		mocks.accessStatus.mockResolvedValue(status());
		mocks.accessFileLinksList.mockResolvedValue({
			links: [link({ state: 'revoked', revokedAt: 9, revokedReason: 'owner' })],
		});
		render(<FileLinksBlock />);
		await screen.findByText('deck.pdf');
		expect(screen.queryByRole('button', { name: 'Revoke' })).toBeNull();
	});

	it('shows All links to an admin only', async () => {
		mocks.accessFileLinksList.mockResolvedValue({ links: [] });
		mocks.accessFileLinksAdminList.mockResolvedValue({ links: [] });
		mocks.accessStatus.mockResolvedValue(status({}, false));
		const first = render(<FileLinksBlock />);
		await screen.findByText("You haven't made any links.");
		expect(first.container.querySelector('[data-panel="all-links"]')).toBeNull();
		cleanup();
		mocks.accessStatus.mockResolvedValue(status({}, true));
		const second = render(<FileLinksBlock />);
		await waitFor(() => expect(second.container.querySelector('[data-panel="all-links"]')).not.toBeNull());
		expect(mocks.accessFileLinksAdminList).toHaveBeenCalled();
	});
});
