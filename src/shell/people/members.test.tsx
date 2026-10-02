// WP-76: D-05 `members` / `members-shared` / `share-kola` / `policies`
// (G-ACCESS §4, §4.5.5, §7.2, §11.2 D-9, D-11, D-12, D-15, D-16).

import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
	accessStatus: vi.fn(),
	accessMembersList: vi.fn(),
	accessMemberSetRole: vi.fn(),
	accessMemberRemove: vi.fn(),
	accessMemberRestore: vi.fn(),
	accessInviteRevoke: vi.fn(),
	accessInviteIssue: vi.fn(),
	accessPolicyGet: vi.fn(),
	accessPolicySetCell: vi.fn(),
	accessPolicySetOwnerApproval: vi.fn(),
	accessSharesList: vi.fn(),
}));

vi.mock('@/lib/access/client', async (orig) => ({
	...(await orig<typeof import('@/lib/access/client')>()),
	...mocks,
}));
vi.mock('./frame', async (orig) => ({
	...(await orig<typeof import('./frame')>()),
	PeopleHeader: () => null,
}));

import type { AccessStatus } from '@/lib/access/client';

import {
	expiryLabel,
	type MembersList,
	MembersTab,
	membersMode,
	personName,
	SOLO_DISABLED_REASON,
} from './members';
import {
	cellEditable,
	defaultMatrix,
	PoliciesTab,
	roleCount,
	SECRETS_STILL_GO_TO_YOU,
} from './policies';
import { issueBlocker, NEEDS_ACCOUNT_COPY, ShareKolaSheet, seeList } from './share-kola-sheet';

const OWNER = '01890a5d-ac96-774b-bcce-b302099a8057';

function t1Status(adminStrength = true): AccessStatus {
	return {
		tier: 't1',
		store: 'ok',
		principal: { principalId: OWNER, username: 'nedjamez', isAdmin: false },
		credential: { via: 'session', deviceId: null, tier: 'full' },
		caps: ['files', 'sessions', 'dispatch', 'approve', 'install', 'settings', 'secrets'],
		adminStrength,
		publicUrl: null,
		sharingEnabled: true,
		share: null,
	};
}

const LIST: MembersList = {
	projectKey: `${OWNER}/default`,
	projectName: 'royalti-co',
	owner: { principalId: OWNER, username: 'nedjamez' },
	counts: { members: 2, pendingInvites: 1 },
	members: [
		{
			principalId: '01890a5d-ac96-774b-bcce-b302099a8001',
			username: 'ada',
			role: 'operator',
			scope: 'project',
			artifactPath: null,
			expiresAt: null,
			addedAt: 1_000,
			lastActiveAt: null,
			weeklySpendCapCents: null,
		},
		{
			principalId: '01890a5d-ac96-774b-bcce-b302099a8002',
			username: null,
			role: 'guest',
			scope: 'artifact',
			artifactPath: 'plans/shell/board.html',
			expiresAt: Date.now() + 4 * 86_400_000,
			addedAt: 2_000,
			lastActiveAt: null,
			weeklySpendCapCents: null,
		},
	],
	invites: [
		{
			inviteId: 'inv-1',
			label: 'tomi@example.com',
			mode: 'email',
			role: 'reviewer',
			scope: 'project',
			artifactPath: null,
			issuedAt: Date.now() - 2 * 86_400_000,
			issuedBy: OWNER,
			expiresAt: Date.now() + 86_400_000,
			memberExpiresAt: null,
			allowNewAccount: false,
			state: 'pending',
		},
	],
};

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

describe('members model', () => {
	it('labels expiries and people', () => {
		expect(expiryLabel(null, 0)).toBeNull();
		expect(expiryLabel(4 * 86_400_000, 0)).toBe('expires in 4 d');
		expect(expiryLabel(90 * 60_000, 0)).toBe('expires in 2 h');
		expect(expiryLabel(10, 20)).toBe('expired');
		expect(personName({ username: null, principalId: OWNER })).toBe('01890a5d…');
	});

	it('is solo on T0 and on T1 with nobody shared', () => {
		expect(membersMode('t0', LIST)).toBe('solo');
		expect(membersMode('t1', { ...LIST, members: [], invites: [] })).toBe('solo');
		expect(membersMode('t1', LIST)).toBe('shared');
	});
});

describe('MembersTab', () => {
	it('T0: "Just you.", Share kola disabled with the T1 reason, the rule box as drawn (§4.5.5)', async () => {
		mocks.accessStatus.mockRejectedValue(new Error('store_unavailable: no store'));
		const { container } = render(<MembersTab />);
		expect(await screen.findByText('Just you.')).toBeTruthy();
		const share = screen.getByRole('button', { name: /Share kola/ });
		expect((share as HTMLButtonElement).disabled).toBe(true);
		expect(screen.getByText(SOLO_DISABLED_REASON)).toBeTruthy();
		expect(screen.getByText(/Sharing needs an account/)).toBeTruthy();
		expect(container.querySelector('[data-state="members"]')).toBeTruthy();
		expect(mocks.accessMembersList).not.toHaveBeenCalled();
		// D-4: the file bar names the access store, with no Open file button.
		expect(container.querySelector('[data-filebar]')?.textContent).toContain(
			'access store · <data-dir>/access.db'
		);
		expect(screen.queryByRole('button', { name: /Open file/ })).toBeNull();
	});

	it('T1: the people table, the fixed Owner row, Remove with Undo, pending invites', async () => {
		mocks.accessStatus.mockResolvedValue(t1Status());
		mocks.accessMembersList.mockResolvedValue(LIST);
		mocks.accessSharesList.mockResolvedValue([]);
		mocks.accessMemberRemove.mockResolvedValue({});
		const { container } = render(<MembersTab />);
		expect(await screen.findByText('ada')).toBeTruthy();
		expect(container.querySelector('[data-state="members-shared"]')).toBeTruthy();
		const owner = container.querySelector('[data-member="owner"]');
		expect(owner?.textContent).toContain('nedjamez');
		expect(owner?.textContent).toContain('you · this device');
		expect(container.querySelector('[data-filebar]')?.textContent).toContain(
			'access store · server operator database'
		);
		expect(screen.getByText('plans/shell/board.html')).toBeTruthy();
		expect(screen.getByText('expires in 4 d')).toBeTruthy();
		expect(screen.getByText('tomi@example.com')).toBeTruthy();
		expect(screen.getByText(/Secrets are not on this list/)).toBeTruthy();

		fireEvent.click(screen.getAllByRole('button', { name: 'Remove' })[0]!);
		const dialog = await screen.findByRole('dialog');
		fireEvent.click(within(dialog).getByRole('button', { name: 'Remove' }));
		await waitFor(() =>
			expect(mocks.accessMemberRemove).toHaveBeenCalledWith('default', LIST.members[0]!.principalId)
		);
		expect(await screen.findByText('Removed ada')).toBeTruthy();
		expect(screen.getByRole('button', { name: /Undo/ })).toBeTruthy();
	});

	it('revokes an invite with no Undo (D-6)', async () => {
		mocks.accessStatus.mockResolvedValue(t1Status());
		mocks.accessMembersList.mockResolvedValue(LIST);
		mocks.accessSharesList.mockResolvedValue([]);
		mocks.accessInviteRevoke.mockResolvedValue({});
		render(<MembersTab />);
		fireEvent.click(await screen.findByRole('button', { name: 'Revoke invite' }));
		await waitFor(() => expect(mocks.accessInviteRevoke).toHaveBeenCalledWith('inv-1'));
		expect(screen.queryByRole('button', { name: /Undo/ })).toBeNull();
	});
});

describe('Share kola', () => {
	it('"What they will be able to see" comes from the role row', () => {
		const reviewer = seeList('reviewer', 'project', ['files', 'sessions']);
		expect(reviewer.find((i) => i.id === 'files')).toMatchObject({
			allowed: true,
			sub: 'Read-only.',
		});
		expect(reviewer.find((i) => i.id === 'sessions')?.sub).toBe(
			'Read-only transcripts. No cost figures.'
		);
		expect(reviewer.find((i) => i.id === 'dispatch')).toMatchObject({
			allowed: false,
			sub: 'They can comment, not instruct.',
		});
		expect(reviewer.find((i) => i.id === 'secrets')).toMatchObject({
			allowed: false,
			sub: 'Never.',
		});
		const operator = seeList('operator', 'project', ['files', 'sessions', 'dispatch', 'approve']);
		expect(operator.find((i) => i.id === 'files')?.sub).toBe('Read and write.');
		const guest = seeList('guest', 'artifact', []);
		expect(guest.find((i) => i.id === 'files')).toMatchObject({
			allowed: true,
			label: 'One artifact',
		});
		expect(guest.filter((i) => i.allowed).map((i) => i.id)).toEqual(['files']);
	});

	it('blocks a bad form', () => {
		const base = {
			mode: 'email' as const,
			who: 'tomi@example.com',
			role: 'reviewer' as const,
			scope: 'project' as const,
			artifactPath: '',
			days: 7,
		};
		expect(issueBlocker(base)).toBeNull();
		expect(issueBlocker({ ...base, who: 'tomi' })).toMatch(/email/);
		expect(
			issueBlocker({ ...base, role: 'guest', scope: 'artifact', artifactPath: 'a.md', days: null })
		).toMatch(/expire/);
		expect(issueBlocker({ ...base, scope: 'artifact', artifactPath: '../x' })).toMatch(/relative/);
		expect(issueBlocker({ ...base, mode: 'link', who: '' })).toBeNull();
	});

	it('creates the invite, shows the link to copy and, without allowNewAccount, the D-16 line', async () => {
		mocks.accessPolicyGet.mockResolvedValue({
			matrix: defaultMatrix(),
			ownerApprovalRequired: true,
		});
		mocks.accessInviteIssue.mockResolvedValue({
			inviteId: 'inv-2',
			url: '/remote/invite#t=iki1.x.y',
			expiresAt: 1,
			allowNewAccount: false,
		});
		const onIssued = vi.fn();
		render(
			<ShareKolaSheet
				open
				onOpenChange={() => {}}
				projectId="default"
				projectName="royalti-co"
				onIssued={onIssued}
			/>
		);
		fireEvent.change(screen.getByLabelText('Who'), { target: { value: 'tomi@example.com' } });
		fireEvent.click(screen.getByRole('button', { name: 'Create invite' }));
		await waitFor(() => expect(mocks.accessInviteIssue).toHaveBeenCalled());
		expect(mocks.accessInviteIssue.mock.calls[0]![0]).toMatchObject({
			projectId: 'default',
			mode: 'email',
			inviteeLabel: 'tomi@example.com',
			role: 'reviewer',
			scope: 'project',
		});
		expect(await screen.findByText(NEEDS_ACCOUNT_COPY)).toBeTruthy();
		expect(screen.getByText(/iki1\.x\.y/)).toBeTruthy();
		expect(
			screen.getByText('Invite created for tomi@example.com — copy the link to send it.')
		).toBeTruthy();
		expect(screen.queryByRole('button', { name: /Send invite/ })).toBeNull();
		expect(onIssued).toHaveBeenCalled();
	});
});

describe('PoliciesTab', () => {
	it('the default matrix: Owner all seven, secrets never for everyone else', () => {
		const m = defaultMatrix();
		expect(Object.values(m.owner).every((v) => v === 'allowed')).toBe(true);
		expect([m.operator.secrets, m.reviewer.secrets, m.guest.secrets]).toEqual([
			'never',
			'never',
			'never',
		]);
		expect(cellEditable('owner', 'files')).toBe(false);
		expect(cellEditable('operator', 'secrets')).toBe(false);
		expect(cellEditable('reviewer', 'dispatch')).toBe(true);
		expect(roleCount(LIST, 'operator')).toBe('1 person');
		expect(roleCount(LIST, 'reviewer')).toBe('0 people');
	});

	it('T0: read-only with the §4.5.5 note and no Require Owner approval', async () => {
		mocks.accessStatus.mockRejectedValue(new Error('store_unavailable: x'));
		mocks.accessPolicyGet.mockResolvedValue({
			matrix: defaultMatrix(),
			ownerApprovalRequired: true,
		});
		render(<PoliciesTab />);
		expect(
			await screen.findByText('Roles apply to people you share a project with on an Ikenga server.')
		).toBeTruthy();
		expect(screen.queryByLabelText('Require Owner approval')).toBeNull();
		expect(screen.queryAllByRole('button', { pressed: true })).toHaveLength(0);
	});

	it('T1 Owner: clicking a cell sets it; turning approval off adds the D-15 line; spend cap is disabled', async () => {
		mocks.accessStatus.mockResolvedValue(t1Status());
		mocks.accessMembersList.mockResolvedValue(LIST);
		mocks.accessPolicyGet.mockResolvedValue({
			matrix: defaultMatrix(),
			ownerApprovalRequired: true,
		});
		const next = defaultMatrix();
		next.reviewer.dispatch = 'allowed';
		mocks.accessPolicySetCell.mockResolvedValue(next);
		mocks.accessPolicySetOwnerApproval.mockResolvedValue({});
		render(<PoliciesTab />);
		const cell = await screen.findByRole('button', {
			name: 'Reviewer · Dispatch to Chi: withheld',
		});
		fireEvent.click(cell);
		await waitFor(() =>
			expect(mocks.accessPolicySetCell).toHaveBeenCalledWith(
				'default',
				'reviewer',
				'dispatch',
				true
			)
		);
		expect(screen.queryByRole('button', { name: /Read secrets/ })).toBeNull();
		fireEvent.click(screen.getByLabelText('Require Owner approval'));
		await waitFor(() =>
			expect(mocks.accessPolicySetOwnerApproval).toHaveBeenCalledWith('default', false)
		);
		expect(screen.getByText(new RegExp(SECRETS_STILL_GO_TO_YOU))).toBeTruthy();
		const cap = await screen.findByLabelText('Weekly spend cap for ada');
		expect((cap as HTMLInputElement).disabled).toBe(true);
		expect(screen.getAllByText('not enforced yet').length).toBeGreaterThan(0);
	});
});
