// WP-77 review m-4: G-ACCESS §6.7 — phones below `full` can't open the
// Audit tab, and it is disabled in the People tab strip with that reason.

import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({ accessStatus: vi.fn() }));

vi.mock('@/lib/access/client', async (orig) => ({
	...(await orig<typeof import('@/lib/access/client')>()),
	accessStatus: mocks.accessStatus,
}));
// The router-driven strip, reduced to plain links (no router in this test).
vi.mock('@/components/ui/segmented', () => ({
	SegmentedLinks: ({
		items,
		ariaLabel,
	}: {
		items: { to: string; label: string }[];
		ariaLabel: string;
	}) => (
		<nav aria-label={ariaLabel}>
			{items.map((i) => (
				<a key={i.to} href={i.to}>
					{i.label}
				</a>
			))}
		</nav>
	),
}));

import type { AccessStatus } from '@/lib/access/client';

import { PeopleHeader } from './frame';

function status(tier: 'view' | 'full', caps: AccessStatus['caps']): AccessStatus {
	return {
		tier: 't0',
		store: 'ok',
		principal: {
			principalId: '01890a5d-ac96-774b-bcce-b302099a8057',
			username: 'ned',
			isAdmin: false,
		},
		credential: { via: 'device', deviceId: 'd1', tier },
		caps,
		adminStrength: false,
		publicUrl: null,
		sharingEnabled: false,
		share: null,
	};
}

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

describe('the People tab strip (§6.7)', () => {
	it('disables Audit with the reason for a phone below full', async () => {
		mocks.accessStatus.mockResolvedValue(status('view', ['files']));
		render(<PeopleHeader tab="devices" />);
		const audit = await screen.findByText('Audit', { selector: '[data-tab-disabled="audit"]' });
		expect((audit as HTMLButtonElement).disabled).toBe(true);
		expect(audit.getAttribute('title')).toMatch(/^Needs settings — this device is /);
		expect(screen.queryByRole('link', { name: 'Audit' })).toBeNull();
		expect(screen.getByRole('link', { name: 'Devices' })).toBeTruthy();
	});

	it('keeps Audit a link for a full credential, and when the status is unknown', async () => {
		mocks.accessStatus.mockResolvedValue(status('full', ['files', 'settings']));
		render(<PeopleHeader tab="devices" />);
		await waitFor(() => expect(mocks.accessStatus).toHaveBeenCalled());
		expect(screen.getByRole('link', { name: 'Audit' })).toBeTruthy();
		cleanup();
		mocks.accessStatus.mockRejectedValue(new Error('store_unavailable'));
		render(<PeopleHeader tab="devices" />);
		await waitFor(() => expect(mocks.accessStatus).toHaveBeenCalledTimes(2));
		expect(screen.getByRole('link', { name: 'Audit' })).toBeTruthy();
	});

	it('takes the reason from the caller without fetching', () => {
		render(<PeopleHeader tab="audit" auditWhy="Needs settings — you have View." />);
		expect(mocks.accessStatus).not.toHaveBeenCalled();
		expect(
			screen.getByText('Audit', { selector: '[data-tab-disabled="audit"]' }).getAttribute('title')
		).toBe('Needs settings — you have View.');
	});
});
