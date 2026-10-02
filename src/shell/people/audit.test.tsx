// WP-77: D-05 `audit` (G-ACCESS §6.4 degraded banner, §6.5 kinds, §6.7
// visibility, §6.8 export, §11.1 scope switch).

import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
	accessStatus: vi.fn(),
	accessAuditList: vi.fn(),
	accessAuditExport: vi.fn(),
	accessAuditReseal: vi.fn(),
	confirm: vi.fn(),
	save: vi.fn(),
	isTauri: vi.fn(() => false),
}));

vi.mock('@/lib/access/client', async (orig) => ({
	...(await orig<typeof import('@/lib/access/client')>()),
	accessStatus: mocks.accessStatus,
	accessAuditList: mocks.accessAuditList,
	accessAuditExport: mocks.accessAuditExport,
	accessAuditReseal: mocks.accessAuditReseal,
}));
vi.mock('@/lib/transport/dialog-shim', () => ({ confirm: mocks.confirm, save: mocks.save }));
vi.mock('@/lib/transport', async (orig) => ({
	...(await orig<typeof import('@/lib/transport')>()),
	isTauri: mocks.isTauri,
}));
vi.mock('./frame', async (orig) => ({
	...(await orig<typeof import('./frame')>()),
	PeopleHeader: ({ onScope }: { onScope?: (s: 'personal' | 'project') => void }) => (
		<button type="button" onClick={() => onScope?.('project')}>
			project scope
		</button>
	),
}));

import type { AccessStatus } from '@/lib/access/client';

import { AuditTab } from './audit';
import {
	type AuditRow,
	actionLabel,
	auditReadReason,
	brokenBanner,
	canReseal,
	categoryCounts,
	deviceLabel,
	exportFileName,
	exportFilter,
	matchesChips,
	NO_CHIPS,
	scopeFilter,
	targetLabel,
	uaFamily,
	whenLabel,
	whoLabel,
	whoOptions,
} from './audit-model';

const OWNER = '01890a5d-ac96-774b-bcce-b302099a8057';
const ADA = '01890a5d-ac96-774b-bcce-b302099a8001';
const ALL_CAPS = [
	'files',
	'sessions',
	'dispatch',
	'approve',
	'install',
	'settings',
	'secrets',
] as AccessStatus['caps'];

function t0Status(over: Partial<AccessStatus> = {}): AccessStatus {
	return {
		tier: 't0',
		store: 'ok',
		principal: { principalId: OWNER, username: 'nedjamez', isAdmin: false },
		credential: { via: 'operator', deviceId: 'host', tier: 'full' },
		caps: ALL_CAPS,
		adminStrength: true,
		publicUrl: null,
		sharingEnabled: false,
		share: null,
		...over,
	};
}

function row(over: Partial<AuditRow>): AuditRow {
	return {
		seq: 1,
		atMs: Date.now(),
		kind: 'app.locked',
		category: 'access',
		principalId: OWNER,
		actorName: 'nedjamez',
		deviceId: 'host',
		deviceName: 'ned-desktop',
		via: 'operator',
		subjectPrincipalId: null,
		subjectName: null,
		subjectDeviceId: null,
		subjectDeviceName: null,
		projectKey: null,
		target: null,
		remoteAddr: null,
		userAgent: null,
		detail: {},
		...over,
	};
}

const ROWS: AuditRow[] = [
	row({
		seq: 4,
		kind: 'permission.decided',
		category: 'permission',
		target: 'royalti-server-v2.6/.env',
		detail: { decision: 'allow_once' },
	}),
	row({
		seq: 3,
		kind: 'dispatch.sent',
		category: 'dispatch',
		principalId: ADA,
		actorName: 'ada',
		deviceId: 'pixel',
		deviceName: 'Pixel 9 · Chrome',
		via: 'device',
		target: 'pty · 3',
	}),
	row({
		seq: 2,
		kind: 'pair.allowed',
		category: 'pairing',
		target: 'Pixel 9 · Chrome',
		subjectDeviceId: 'pixel',
	}),
	row({ seq: 1, kind: 'vault.locked', category: 'access', target: 'workspace' }),
];

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

describe('audit model', () => {
	it('names actions from the closed kind list, in D-05 words', () => {
		expect(actionLabel(row({ kind: 'pair.allowed' }))).toBe('Paired device');
		expect(actionLabel(row({ kind: 'dispatch.sent' }))).toBe('Dispatched');
		expect(actionLabel(row({ kind: 'member.role_changed' }))).toBe('Changed role');
		expect(actionLabel(row({ kind: 'vault.locked' }))).toBe('Vault locked');
		expect(actionLabel(row({ kind: 'routing.changed' }))).toBe('Changed approval policy');
		expect(actionLabel(row({ kind: 'device.tier_changed' }))).toBe('Changed device capability');
		expect(
			actionLabel(row({ kind: 'permission.decided', detail: { decision: 'allow_once' } }))
		).toBe('Allowed permission');
		expect(actionLabel(row({ kind: 'permission.decided', detail: { decision: 'deny' } }))).toBe(
			'Denied permission'
		);
		expect(actionLabel(row({ kind: 'something.new' }))).toBe('something.new');
	});

	it('renders a T1 password session as "Browser session · UA · session_ref" (C-19)', () => {
		const r = row({
			deviceId: null,
			deviceName: null,
			via: 'session',
			userAgent: 'Mozilla/5.0 (X11; Linux) Gecko/20100101 Firefox/131.0',
			detail: { session_ref: 'a1b2c3d4' },
		});
		expect(deviceLabel(r)).toBe('Browser session · Firefox · a1b2c3d4');
		expect(uaFamily('Mozilla/5.0 Chrome/129 Safari/537')).toBe('Chrome');
		expect(uaFamily(null)).toBe('browser');
		expect(deviceLabel(row({ deviceId: null, deviceName: null, via: 'cli' }))).toBe('server CLI');
		expect(whoLabel(row({ principalId: null, actorName: null, via: 'system' }))).toBe('system');
		expect(whoLabel(row({ principalId: ADA, actorName: null }))).toBe('01890a5d');
		expect(targetLabel(row({ target: null, subjectName: 'ada' }))).toBe('ada');
	});

	it('formats When as D-05 does', () => {
		const now = new Date(2026, 9, 2, 12, 0).getTime();
		expect(whenLabel(new Date(2026, 9, 2, 10, 42).getTime(), now)).toBe('10:42');
		expect(whenLabel(new Date(2026, 9, 1, 14, 5).getTime(), now)).toBe('Yest 14:05');
		expect(whenLabel(new Date(2026, 8, 18, 9, 0).getTime(), now)).toBe('Sep 18 09:00');
		expect(exportFileName(now)).toBe('ikenga-audit-2026-10-02.jsonl');
	});

	it('filters like access_audit_list: who / device are actor or subject', () => {
		expect(ROWS.filter((r) => matchesChips(r, { ...NO_CHIPS, who: ADA }))).toHaveLength(1);
		expect(ROWS.filter((r) => matchesChips(r, { ...NO_CHIPS, device: 'pixel' }))).toHaveLength(2);
		expect(ROWS.filter((r) => matchesChips(r, { ...NO_CHIPS, kind: 'pairing' }))).toHaveLength(1);
		expect(ROWS.filter((r) => matchesChips(r, { ...NO_CHIPS, q: 'VAULT' }))).toHaveLength(1);
		expect(categoryCounts(ROWS)).toEqual({
			permission: 1,
			dispatch: 1,
			access: 1,
			pairing: 1,
			people: 0,
		});
		expect(whoOptions(ROWS).map((o) => [o.label, o.count])).toEqual([
			['nedjamez', 3],
			['ada', 1],
		]);
		expect(
			exportFilter({ projectKey: 'k' }, { who: ADA, device: 'all', kind: 'pairing', q: ' x ' })
		).toEqual({ projectKey: 'k', who: ADA, category: 'pairing', q: 'x' });
	});

	it('maps the scope switch onto the list filter (§11.1)', () => {
		expect(scopeFilter('project', t0Status(), 'royalti-co')).toEqual({
			projectKey: `${OWNER}/royalti-co`,
		});
		expect(scopeFilter('personal', t0Status(), 'p')).toEqual({});
		expect(scopeFilter('personal', t0Status({ tier: 't1' }), 'p')).toEqual({ who: OWNER });
		expect(scopeFilter('personal', null, 'p')).toEqual({});
	});

	it('needs settings to read, and only the desktop operator reseals (§6.7, §6.4)', () => {
		expect(auditReadReason(t0Status())).toBeNull();
		const phone = t0Status({
			credential: { via: 'device', deviceId: 'pixel', tier: 'approve' },
			caps: ['files', 'sessions', 'dispatch', 'approve'],
		});
		expect(auditReadReason(phone)).toMatch(/^Needs settings — this device is Dispatch \+ approve/);
		expect(canReseal(t0Status())).toBe(true);
		expect(canReseal(phone)).toBe(false);
		expect(
			canReseal(
				t0Status({ tier: 't1', credential: { via: 'session', deviceId: null, tier: 'full' } })
			)
		).toBe(false);
		expect(brokenBanner(42)).toBe(
			'The audit chain is broken at #42 — access changes are paused. An operator must reseal it.'
		);
	});
});

describe('AuditTab', () => {
	it('lists rows newest first with D-05 columns and narrows with the chips', async () => {
		mocks.accessStatus.mockResolvedValue(t0Status());
		mocks.accessAuditList.mockResolvedValue({ rows: ROWS, nextBefore: null });
		render(<AuditTab />);
		const view = await screen.findByText('Paired device');
		const root = view.closest('[data-state="audit"]') as HTMLElement;
		expect(root.dataset.audit).toBe('rows');
		expect(root.dataset.store).toBe('ok');
		const table = within(root).getByRole('table');
		expect(
			within(table)
				.getAllByRole('columnheader')
				.map((h) => h.textContent)
		).toEqual(['Who', 'Device', 'Action', 'Target', 'When']);
		expect(within(table).getByText('Allowed permission')).toBeTruthy();
		expect(within(table).getByText('pty · 3')).toBeTruthy();
		// Kind → Pairing.
		fireEvent.click(
			within(screen.getByRole('group', { name: 'Kind' })).getByRole('button', { name: /^Pairing/ })
		);
		expect(within(table).queryByText('Dispatched')).toBeNull();
		expect(within(table).getByText('Paired device')).toBeTruthy();
		// Search with nothing left.
		fireEvent.change(screen.getByLabelText('Search the audit log'), { target: { value: 'zzz' } });
		expect(within(table).getByText('Nothing matches. Clear a filter.')).toBeTruthy();
		expect(screen.getByText(/^Append-only\. Rows are written by the host/)).toBeTruthy();
		// D-4: the access-store file bar, no "Open file".
		expect(root.querySelector('[data-filebar="access-store"]')).toBeTruthy();
		expect(screen.queryByText('Open file')).toBeNull();
		expect(mocks.accessAuditList).toHaveBeenCalledWith({}, undefined, 200);
	});

	it('switches to the project scope with projectKey', async () => {
		mocks.accessStatus.mockResolvedValue(t0Status());
		mocks.accessAuditList.mockResolvedValue({ rows: [], nextBefore: null });
		render(<AuditTab />);
		await screen.findByText('Nothing recorded yet.');
		fireEvent.click(screen.getByText('project scope'));
		await waitFor(() =>
			expect(mocks.accessAuditList).toHaveBeenLastCalledWith(
				{ projectKey: `${OWNER}/default` },
				undefined,
				200
			)
		);
	});

	it('pages older rows with nextBefore', async () => {
		mocks.accessStatus.mockResolvedValue(t0Status());
		mocks.accessAuditList
			.mockResolvedValueOnce({ rows: ROWS.slice(0, 2), nextBefore: 3 })
			.mockResolvedValueOnce({ rows: ROWS.slice(2), nextBefore: null });
		render(<AuditTab />);
		fireEvent.click(await screen.findByRole('button', { name: 'Load older' }));
		await screen.findByText('Vault locked');
		expect(mocks.accessAuditList).toHaveBeenLastCalledWith({}, 3, 200);
		expect(screen.queryByRole('button', { name: 'Load older' })).toBeNull();
	});

	it('says why below settings instead of listing (§6.7)', async () => {
		mocks.accessStatus.mockResolvedValue(
			t0Status({
				credential: { via: 'device', deviceId: 'pixel', tier: 'dispatch' },
				caps: ['files', 'sessions', 'dispatch'],
			})
		);
		render(<AuditTab />);
		await screen.findByText(/^Needs settings — this device is View \+ dispatch/);
		expect(document.querySelector('[data-state="audit"]')?.getAttribute('data-audit')).toBe(
			'forbidden'
		);
		expect(mocks.accessAuditList).not.toHaveBeenCalled();
	});

	it('shows the degraded banner and reseals from the desktop (§6.4)', async () => {
		mocks.accessStatus
			.mockResolvedValueOnce(t0Status({ store: 'degraded', brokenAtSeq: 7 }))
			.mockResolvedValue(t0Status());
		mocks.accessAuditList.mockResolvedValue({ rows: ROWS, nextBefore: null });
		mocks.confirm.mockResolvedValue(true);
		mocks.accessAuditReseal.mockResolvedValue({});
		render(<AuditTab />);
		const banner = await screen.findByRole('alert');
		expect(banner.textContent).toContain(brokenBanner(7));
		expect(document.querySelector('[data-state="audit"]')?.getAttribute('data-store')).toBe(
			'degraded'
		);
		fireEvent.click(within(banner).getByRole('button', { name: 'Reseal…' }));
		await waitFor(() => expect(mocks.accessAuditReseal).toHaveBeenCalledWith(7));
		await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
	});

	it('tells a T1 session to reseal on the server, with no button', async () => {
		mocks.accessStatus.mockResolvedValue(
			t0Status({
				tier: 't1',
				store: 'degraded',
				brokenAtSeq: 3,
				credential: { via: 'session', deviceId: null, tier: 'full' },
			})
		);
		mocks.accessAuditList.mockResolvedValue({ rows: [], nextBefore: null });
		render(<AuditTab />);
		const banner = await screen.findByRole('alert');
		expect(banner.textContent).toContain('ikenga-server audit reseal --ack 3');
		expect(within(banner).queryByRole('button')).toBeNull();
	});

	it('downloads the export in a browser, never naming a path (§6.8)', async () => {
		mocks.accessStatus.mockResolvedValue(
			t0Status({ tier: 't1', credential: { via: 'session', deviceId: null, tier: 'full' } })
		);
		mocks.accessAuditList.mockResolvedValue({ rows: ROWS, nextBefore: null });
		mocks.accessAuditExport.mockResolvedValue({
			jsonl: '{"seq":1}\n{"seq":2}\n{"type":"manifest"}\n',
			truncated: false,
		});
		const createObjectURL = vi.fn(() => 'blob:x');
		Object.assign(URL, { createObjectURL, revokeObjectURL: vi.fn() });
		render(<AuditTab />);
		await screen.findByText('Paired device');
		fireEvent.click(screen.getByRole('button', { name: /Export/ }));
		await waitFor(() => expect(mocks.accessAuditExport).toHaveBeenCalledWith({ who: OWNER }));
		expect(mocks.save).not.toHaveBeenCalled();
		await screen.findByText(/^Exported 2 rows to ikenga-audit-\d{4}-\d{2}-\d{2}\.jsonl$/);
	});

	it('writes the export to a picked file on the desktop', async () => {
		mocks.isTauri.mockReturnValue(true);
		mocks.accessStatus.mockResolvedValue(t0Status());
		mocks.accessAuditList.mockResolvedValue({ rows: ROWS, nextBefore: null });
		mocks.save.mockResolvedValue('/home/ned/ikenga-audit.jsonl');
		mocks.accessAuditExport.mockResolvedValue({ path: '/home/ned/ikenga-audit.jsonl' });
		render(<AuditTab />);
		await screen.findByText('Paired device');
		fireEvent.click(screen.getByRole('button', { name: /Export/ }));
		await waitFor(() =>
			expect(mocks.accessAuditExport).toHaveBeenCalledWith({}, '/home/ned/ikenga-audit.jsonl')
		);
		expect(mocks.save.mock.calls[0][0].defaultPath).toMatch(/^ikenga-audit-.*\.jsonl$/);
		await screen.findByText('Exported 4 rows to /home/ned/ikenga-audit.jsonl');
		mocks.isTauri.mockReturnValue(false);
	});

	it('explains an unavailable store', async () => {
		mocks.accessStatus.mockRejectedValue(new Error('store_unavailable: no daemon'));
		mocks.accessAuditList.mockRejectedValue(new Error('store_unavailable: no daemon'));
		render(<AuditTab />);
		await screen.findByText(/access store, and it isn't available right now/);
		expect(document.querySelector('[data-state="audit"]')?.getAttribute('data-audit')).toBe(
			'unavailable'
		);
	});
});
