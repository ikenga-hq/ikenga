// WP-78b — D-05 People, devices and access (`designs/people.html`), the
// Phase 7 Part B design sweep (01-plan.md §Phase 7 verification, Part B:
// "design-verify passes for D-05 in both modes"). Every D-05 state renders
// here through its `data-state` root, in dark and light, and is screenshotted
// for the comparison against the mockup at the same `?state=` / `&m=light`
// (G-ACCESS §11.1; deviations are checked against §11.2 D-1..D-16, not the
// mockup text). `e2e/seats.spec.ts` keeps the WP-72 local states (profile,
// devices with no paired device, audit, locked); this file adds the rest:
//
//   desktop (Tauri mock)   devices (paired rows, cap menu, revoke confirm),
//                          pair, pair-confirm, members (solo), policies (T0),
//                          the header scope switch on every tab
//   T1 browser (/api/rpc)  sign-in, profile-account (+ sign-out confirm),
//                          members-shared (role menu, remove + Undo, invite
//                          revoke), share-kola, policies
//   paired phone           remote-client (a `Dispatch + approve` device)
//
// Keyboard focus: every control a state offers draws D-05's focus ring, a
// solid 2 px outline (`people.html` `:focus-visible`).
//
// Screenshots go to `$IKENGA_E2E_SHOTS` when set, else to the test output dir.

import { expect, type Locator, type Page, type TestInfo, test } from '@playwright/test';

import { installRemoteMock } from './fixtures/remote-mock';
import {
	installTauriMock,
	invokedCommands,
	type MockResponses,
	seatResponses,
	setMockResponse,
} from './fixtures/tauri-mock';

type Mode = 'dark' | 'light';

const MIN = 60_000;
const NOW = Date.now();

function trackPageErrors(page: Page): string[] {
	const errors: string[] = [];
	page.on('pageerror', (err) => errors.push(err.stack ?? err.message));
	return errors;
}

function shotPath(testInfo: TestInfo, name: string): string {
	const dir = process.env.IKENGA_E2E_SHOTS;
	return dir ? `${dir.replace(/[\\/]$/, '')}/${name}` : testInfo.outputPath(name);
}

async function seed(page: Page, mode: Mode) {
	await page.emulateMedia({ colorScheme: mode });
	await page.addInitScript((m) => {
		localStorage.setItem('ikenga.gloss.seen', JSON.stringify(['ngwa', 'chi']));
		localStorage.setItem(
			'ikenga.theme',
			JSON.stringify({
				state: {
					theme: 'A',
					mode: m,
					density: 'comfortable',
					tintStrength: 'subtle',
					workspace: 'project',
				},
				version: 2,
			})
		);
	}, mode);
}

async function go(page: Page, path: string) {
	const address = page.getByRole('textbox', { name: 'Address' });
	await address.fill(path);
	await address.press('Enter');
}

/** Reach `el` from the keyboard (a Tab lands there), so `:focus-visible` holds. */
async function tabTo(page: Page, el: Locator) {
	await el.focus();
	await page.keyboard.press('Shift+Tab');
	await page.keyboard.press('Tab');
	await expect(el).toBeFocused();
}

/** D-05 `:focus-visible`: 2px solid. Polled: the shadcn buttons'
 *  `transition-all` animates the outline in. `clear` (a primary-filled
 *  control, whose fill is the ring's colour) also wants the design's
 *  `outline-offset: 1px` — flush, the ring vanishes into the fill. */
async function expectFocusRing(
	page: Page,
	el: Locator,
	opts: { clear?: boolean; unclipped?: boolean } = {}
) {
	await tabTo(page, el);
	await expect
		.poll(
			() =>
				el.evaluate((node) => {
					const cs = getComputedStyle(node);
					return { style: cs.outlineStyle, width: cs.outlineWidth };
				}),
			{ message: await el.evaluate((node) => node.outerHTML.slice(0, 120)) }
		)
		.toEqual({ style: 'solid', width: '2px' });
	if (opts.clear) {
		const offset = await el.evaluate((node) => getComputedStyle(node).outlineOffset);
		expect(
			Number.parseFloat(offset),
			'a primary-filled control needs an outline offset'
		).toBeGreaterThan(0);
	}
	if (opts.unclipped) {
		// The ring's outer edge (border box grown by offset + width) must sit
		// inside every clipping ancestor's padding box, or part of it is cut.
		const clipped = await el.evaluate((node) => {
			const cs = getComputedStyle(node);
			const grow = Number.parseFloat(cs.outlineOffset) + Number.parseFloat(cs.outlineWidth);
			const r = node.getBoundingClientRect();
			const ring = {
				left: r.left - grow,
				top: r.top - grow,
				right: r.right + grow,
				bottom: r.bottom + grow,
			};
			const cuts: string[] = [];
			for (let a = node.parentElement; a; a = a.parentElement) {
				const acs = getComputedStyle(a);
				if (acs.overflowX === 'visible' && acs.overflowY === 'visible') continue;
				const b = a.getBoundingClientRect();
				const left = b.left + a.clientLeft;
				const top = b.top + a.clientTop;
				const right = left + a.clientWidth;
				const bottom = top + a.clientHeight;
				const eps = 0.5;
				if (
					ring.left < left - eps ||
					ring.top < top - eps ||
					ring.right > right + eps ||
					ring.bottom > bottom + eps
				) {
					cuts.push(`${a.tagName.toLowerCase()}${a.id ? `#${a.id}` : ''}`);
				}
			}
			return cuts;
		});
		expect(clipped, 'the focus ring is clipped by').toEqual([]);
	}
}

// ── sample content (D-05 `people.html` placeholders) ───────────────────────

const STATUS_T0 = {
	tier: 't0',
	store: 'ok',
	principal: { principalId: 'p-ned', username: 'ned', isAdmin: false },
	credential: { via: 'operator', deviceId: 'host', tier: 'full' },
	caps: ['files', 'sessions', 'dispatch', 'approve', 'install', 'settings', 'secrets'],
	adminStrength: true,
	publicUrl: null,
	sharingEnabled: false,
	share: null,
};

const DEVICES = [
	{
		deviceId: 'host',
		kind: 'host',
		name: 'ned-desktop',
		platform: 'Windows 11',
		tier: 'full',
		pairedAt: 0,
		lastSeenAt: null,
		lastSeenAddr: null,
		liveSockets: 2,
		thisDevice: true,
	},
	{
		deviceId: 'd-macbook',
		kind: 'paired',
		name: 'ned-macbook',
		platform: 'macOS',
		tier: 'approve',
		pairedAt: NOW - 20 * 86_400_000,
		lastSeenAt: NOW - 2 * 60 * MIN,
		lastSeenAddr: '100.94.12.7',
		liveSockets: 1,
		thisDevice: false,
	},
	{
		deviceId: 'd-pixel',
		kind: 'paired',
		name: 'Pixel 9 · Chrome',
		platform: 'Android',
		tier: 'dispatch',
		pairedAt: NOW - 14 * 86_400_000,
		lastSeenAt: NOW - 4 * MIN,
		lastSeenAddr: '100.94.12.31',
		liveSockets: 0,
		thisDevice: false,
	},
];

/** A daemon on the tailnet, so the pair sheet draws its QR (D-14). */
const DAEMON = {
	available: true,
	host: '100.94.12.5',
	port: 1430,
	token: 'e2e',
	httpUrl: 'http://100.94.12.5:1430',
	wsUrl: 'ws://100.94.12.5:1430',
	pid: 4242,
	mode: 'persistent',
};

const TICKET = {
	pairingId: 'pair-1',
	code: 'K7P-42Q',
	expiresAt: NOW + 10 * MIN,
	pairUrl: 'http://100.94.12.5:1430/remote/pair#c=K7P-42Q',
	qrPayload: 'http://100.94.12.5:1430/remote/pair#c=K7P-42Q',
	cookieSecure: false,
};

const PAIR_REQUEST = {
	pairingId: 'pair-1',
	deviceName: 'Pixel 9 · Chrome',
	platform: 'Android',
	remoteAddr: '100.94.12.31',
	askedAt: NOW - 12_000,
	code: 'K7P-42Q',
	fingerprint: ['amber', 'otter', 'violin', 'harbor'],
	state: 'awaiting_host',
};

function desktopResponses(over: MockResponses = {}): MockResponses {
	return {
		...seatResponses(),
		access_status: STATUS_T0,
		access_devices_list: DEVICES,
		access_routing_get: { mode: 'any_approve', deviceId: null, deviceName: null },
		access_pair_pending: [],
		pty_daemon_info: DAEMON,
		access_members_list: { __error: 'requires_t1: sharing needs a T1 server' },
		// T0: the daemon answers §4.1's defaults (`access/policy.rs`).
		access_policy_get: POLICY,
		...over,
	};
}

async function bootDesktop(page: Page, mode: Mode, responses: MockResponses = {}) {
	const pageErrors = trackPageErrors(page);
	await seed(page, mode);
	await installTauriMock(page, { responses: desktopResponses(responses) });
	// The tailnet daemon answers its health probe.
	await page.route('http://100.94.12.5:1430/api/health', (route) =>
		route.fulfill({ json: { ok: true } })
	);
	await page.goto('/', { waitUntil: 'domcontentloaded' });
	await expect(page.getByRole('main')).toBeVisible({ timeout: MIN });
	await expect(page.locator('html')).toHaveAttribute('data-mode', mode);
	return pageErrors;
}

// T1: `ned` owns `royalti-co`, shared with three people and one pending invite.
const ME = {
	principal_id: '0b4f2c9e-7d1a-4e3b-9c8f-2a6d5e1f0c37',
	username: 'ned',
	is_admin: true,
};

const STATUS_T1 = {
	...STATUS_T0,
	tier: 't1',
	principal: { principalId: ME.principal_id, username: 'ned', isAdmin: true },
	credential: { via: 'session', deviceId: null, tier: 'full' },
	sharingEnabled: true,
};

const MEMBERS = {
	projectKey: `${ME.principal_id}/royalti-co`,
	projectName: 'royalti-co',
	owner: { principalId: ME.principal_id, username: 'ned' },
	counts: { members: 3, pendingInvites: 1 },
	members: [
		{
			principalId: 'p-ada',
			username: 'ada',
			role: 'operator',
			scope: 'project',
			artifactPath: null,
			expiresAt: null,
			addedAt: NOW - 20 * 86_400_000,
			lastActiveAt: NOW - 3 * MIN,
			weeklySpendCapCents: 2500,
		},
		{
			principalId: 'p-femi',
			username: 'femi',
			role: 'reviewer',
			scope: 'project',
			artifactPath: null,
			expiresAt: null,
			addedAt: NOW - 14 * 86_400_000,
			lastActiveAt: NOW - 26 * 60 * MIN,
			weeklySpendCapCents: null,
		},
		{
			principalId: 'p-tomi',
			username: 'tomi',
			role: 'guest',
			scope: 'artifact',
			artifactPath: 'plans/shell/board.html',
			expiresAt: NOW + 4 * 86_400_000,
			addedAt: NOW - 3 * 86_400_000,
			lastActiveAt: null,
			weeklySpendCapCents: null,
		},
	],
	invites: [
		{
			inviteId: 'inv-1',
			label: 'guest@example.com',
			mode: 'email',
			role: 'reviewer',
			scope: 'project',
			artifactPath: null,
			issuedAt: NOW - 2 * 60 * MIN,
			issuedBy: ME.principal_id,
			expiresAt: NOW + 6 * 86_400_000,
			memberExpiresAt: null,
			allowNewAccount: true,
			state: 'pending',
		},
	],
};

const POLICY = {
	matrix: {
		owner: {
			files: 'allowed',
			sessions: 'allowed',
			dispatch: 'allowed',
			approve: 'allowed',
			install: 'allowed',
			settings: 'allowed',
			secrets: 'allowed',
		},
		operator: {
			files: 'allowed',
			sessions: 'allowed',
			dispatch: 'allowed',
			approve: 'allowed',
			install: 'withheld',
			settings: 'withheld',
			secrets: 'never',
		},
		reviewer: {
			files: 'allowed',
			sessions: 'allowed',
			dispatch: 'withheld',
			approve: 'withheld',
			install: 'withheld',
			settings: 'withheld',
			secrets: 'never',
		},
		guest: {
			files: 'withheld',
			sessions: 'withheld',
			dispatch: 'withheld',
			approve: 'withheld',
			install: 'withheld',
			settings: 'withheld',
			secrets: 'never',
		},
	},
	ownerApprovalRequired: true,
};

async function bootT1(
	page: Page,
	mode: Mode,
	opts: { signedIn?: boolean; responses?: MockResponses } = {}
) {
	const pageErrors = trackPageErrors(page);
	await seed(page, mode);
	const mock = await installRemoteMock(page, {
		tier: 't1',
		me: opts.signedIn === false ? null : ME,
		responses: {
			...seatResponses(),
			access_status: STATUS_T1,
			access_members_list: MEMBERS,
			access_policy_get: POLICY,
			access_shares_list: [],
			access_devices_list: [DEVICES[0]],
			access_routing_get: { mode: 'any_approve', deviceId: null, deviceName: null },
			access_pair_pending: [],
			...(opts.responses ?? {}),
		},
	});
	await page.goto('/', { waitUntil: 'domcontentloaded' });
	return { pageErrors, mock };
}

// ── desktop ────────────────────────────────────────────────────────────────

for (const mode of ['dark', 'light'] as const) {
	test.describe(`D-05 desktop — ${mode}`, () => {
		test(`devices: paired rows, the cap menu and the revoke confirm (no Undo, D-5) (${mode})`, async ({
			page,
		}, testInfo) => {
			const pageErrors = await bootDesktop(page, mode, {
				access_device_set_tier: { ...DEVICES[2], tier: 'approve' },
				access_device_revoke: {},
			});
			await go(page, '/settings/devices');
			const devices = page.locator('[data-state="devices"]');
			await expect(devices).toBeVisible();
			const table = devices.getByRole('table');
			await expect(table.getByRole('row')).toHaveCount(4);
			await expect(devices.getByText('2 paired')).toBeVisible();
			await expect(table.getByRole('row', { name: /ned-desktop/ })).toContainText('this device');
			await expect(table.getByRole('row', { name: /ned-macbook/ })).toContainText('100.94.12.7');
			await page.screenshot({ path: shotPath(testInfo, `people-devices-paired-${mode}.png`) });

			// Cap menu (§1.3): the three remote tiers plus Full, each with its line.
			// WP-78c (78b N1): the #scopeSw segments sit in an overflow-hidden
			// group; the whole ring must still show.
			await expectFocusRing(page, devices.locator('#scopeSw button[data-scope="personal"]'), {
				unclipped: true,
			});
			const cap = devices.getByRole('button', { name: 'What Pixel 9 · Chrome can do' });
			await expectFocusRing(page, cap);
			await cap.click();
			const menu = page.getByRole('menu');
			await expect(menu.getByRole('menuitemradio')).toHaveCount(4);
			await expect(menu.getByRole('menuitemradio', { name: /View \+ dispatch/ })).toHaveAttribute(
				'aria-checked',
				'true'
			);
			await page.screenshot({ path: shotPath(testInfo, `people-devices-capmenu-${mode}.png`) });
			await menu.getByRole('menuitemradio', { name: /Dispatch \+ approve/ }).click();
			await expect
				.poll(async () =>
					(await invokedCommands(page)).filter((c) => c.cmd === 'access_device_set_tier')
				)
				.toEqual([
					{ cmd: 'access_device_set_tier', args: { deviceId: 'd-pixel', tier: 'approve' } },
				]);

			// Revoke (§3.10): a confirm, then immediate — no Undo (D-5).
			const revoke = table
				.getByRole('row', { name: /Pixel 9/ })
				.getByRole('button', { name: 'Revoke' });
			await expectFocusRing(page, revoke);
			await revoke.click();
			const confirm = page.locator('[data-state="devices-revoke"]');
			await expect(
				confirm.getByRole('heading', { name: 'Revoke Pixel 9 · Chrome?' })
			).toBeVisible();
			await expect(confirm.getByRole('button', { name: 'Keep it' })).toBeVisible();
			await page.screenshot({ path: shotPath(testInfo, `people-devices-revoke-${mode}.png`) });
			await confirm.getByRole('button', { name: 'Revoke' }).click();
			await expect(confirm).toHaveCount(0);
			expect((await invokedCommands(page)).map((c) => c.cmd)).toContain('access_device_revoke');
			await expect(page.getByRole('button', { name: 'Undo' })).toHaveCount(0);
			expect(pageErrors).toEqual([]);
		});

		test(`pair → pair-confirm: the code and QR, then the host confirm takes the keyboard (${mode})`, async ({
			page,
		}, testInfo) => {
			const pageErrors = await bootDesktop(page, mode, {
				access_pair_begin: TICKET,
				access_pair_decide: { device: { ...DEVICES[2], tier: 'dispatch' } },
			});
			await go(page, '/settings/devices');
			const devices = page.locator('[data-state="devices"]');
			await expect(devices.getByText('2 paired')).toBeVisible();
			await devices.getByRole('button', { name: 'Pair a device' }).click();

			const sheet = page.locator('[data-state="pair"]');
			await expect(sheet).toHaveAttribute('data-pair', 'code');
			await expect(sheet.locator('[data-pair-code]')).toHaveText('K7P-42Q');
			await expect(sheet.getByRole('img', { name: 'QR code for the pairing link' })).toBeVisible();
			await expect(sheet).toContainText('expires in');
			await expect(sheet.getByRole('button', { name: 'Waiting for the device…' })).toBeDisabled();
			// D-10: no iyke line in the footer.
			await expect(sheet).not.toContainText('iyke');
			await expectFocusRing(page, sheet.getByRole('button', { name: 'New code' }));
			await page.screenshot({ path: shotPath(testInfo, `people-pair-${mode}.png`) });

			// The phone typed the code and confirmed: the sheet hands over.
			await setMockResponse(page, 'access_pair_pending', [PAIR_REQUEST]);
			const confirm = page.locator('[data-state="pair-confirm"]');
			await expect(confirm).toBeVisible();
			await expect(sheet).toHaveCount(0);
			await expect(confirm.getByRole('heading', { name: 'A device wants to pair' })).toBeVisible();
			await expect(confirm.locator('[data-row="fingerprint"]')).toContainText(
				'amber · otter · violin · harbor'
			);

			// WP-74b R1: focus lands on the confirm (not on the frame behind it,
			// not on a button a stray Enter would press) …
			const panel = confirm.locator('[data-pair-panel]');
			await expect(panel).toBeFocused();
			// … the words are announced with it …
			const described = await panel.getAttribute('aria-describedby');
			expect(described).toBeTruthy();
			const text = await page.evaluate(
				(ids) =>
					ids
						.split(' ')
						.map((id) => document.getElementById(id)?.textContent ?? '')
						.join(' '),
				described ?? ''
			);
			expect(text).toContain('amber · otter · violin · harbor');
			// … and Tab walks the tier, Deny and Pair device, and stays inside.
			const tier = confirm.getByRole('tab', { name: 'View + dispatch' });
			await page.keyboard.press('Tab');
			await expect(tier).toBeFocused();
			await page.keyboard.press('ArrowRight');
			await expect(confirm.getByRole('tab', { name: 'Dispatch + approve' })).toBeFocused();
			await page.keyboard.press('ArrowLeft');
			await expect(tier).toBeFocused();
			await page.keyboard.press('Tab');
			const deny = confirm.getByRole('button', { name: 'Deny' });
			await expect(deny).toBeFocused();
			await page.keyboard.press('Tab');
			const allow = confirm.getByRole('button', { name: 'Pair device' });
			await expect(allow).toBeFocused();
			// Pair device is primary-filled: the ring must stand clear of it (F1).
			expect(
				await allow.evaluate((el) => {
					const cs = getComputedStyle(el);
					return {
						style: cs.outlineStyle,
						width: cs.outlineWidth,
						clear: Number.parseFloat(cs.outlineOffset) > 0,
					};
				})
			).toEqual({ style: 'solid', width: '2px', clear: true });
			for (let i = 0; i < 4; i++) {
				await page.keyboard.press('Tab');
				expect(
					await page.evaluate(
						() => !!document.activeElement?.closest('[data-state="pair-confirm"]')
					)
				).toBe(true);
			}
			await page.keyboard.press('Shift+Tab');
			await page.screenshot({ path: shotPath(testInfo, `people-pair-confirm-${mode}.png`) });
			await panel.screenshot({ path: shotPath(testInfo, `people-pair-confirm-panel-${mode}.png`) });

			await allow.focus();
			await page.keyboard.press('Enter');
			await expect(confirm).toHaveCount(0);
			await expect(page.locator('[data-pair-toast="ok"]')).toContainText(
				'Paired Pixel 9 · Chrome · View + dispatch'
			);
			const decide = (await invokedCommands(page)).filter((c) => c.cmd === 'access_pair_decide');
			expect(decide.map((c) => c.args)).toEqual([
				{ pairingId: 'pair-1', decision: 'allow', tier: 'dispatch' },
			]);
			expect(pageErrors).toEqual([]);
		});

		test(`members: the solo state; Share kola disabled with its reason (${mode})`, async ({
			page,
		}, testInfo) => {
			const pageErrors = await bootDesktop(page, mode);
			await go(page, '/settings/members');
			const members = page.locator('[data-state="members"]');
			await expect(members).toBeVisible();
			await expect(members.getByRole('heading', { name: 'Just you.' })).toBeVisible();
			await expect(members.getByRole('button', { name: 'Share kola' })).toBeDisabled();
			await expect(
				members.getByText('Sharing needs an Ikenga server with accounts (T1)')
			).toBeVisible();
			await expect(members.locator('[data-filebar="access-store"]')).toContainText(
				'<data-dir>/access.db'
			);
			await expectFocusRing(
				page,
				members.locator('section').getByRole('link', { name: 'Policies' })
			);
			await page.screenshot({ path: shotPath(testInfo, `people-members-${mode}.png`) });
			expect(pageErrors).toEqual([]);
		});

		test(`policies (T0): the default matrix, read-only (${mode})`, async ({ page }, testInfo) => {
			const pageErrors = await bootDesktop(page, mode);
			await go(page, '/settings/policies');
			const policies = page.locator('[data-state="policies"]');
			await expect(policies).toBeVisible();
			await expect(policies.locator('[data-matrix] [data-cell]')).toHaveCount(28);
			await expect(policies.locator('[data-matrix] button')).toHaveCount(0);
			await expect(
				policies.getByText('Roles apply to people you share a project with on an Ikenga server.')
			).toBeVisible();
			await page.screenshot({ path: shotPath(testInfo, `people-policies-t0-${mode}.png`) });
			expect(pageErrors).toEqual([]);
		});
	});
}

test.describe('D-05 header controls', () => {
	test('#scopeSw per tab; no "Open file", no iyke line (D-4, D-10)', async ({ page }) => {
		const pageErrors = await bootDesktop(page, 'dark', {
			access_audit_list: { rows: [], nextBefore: null },
		});
		const cases = [
			{ tab: 'profile', live: ['personal'], why: 'A profile is yours, not the project’s.' },
			{
				tab: 'devices',
				live: ['personal'],
				why: 'Devices pair to this machine, not to a project.',
			},
			{
				tab: 'members',
				live: ['project'],
				why: 'People are invited to a project, not to a machine.',
			},
			{ tab: 'policies', live: ['project'], why: 'Roles are defined per project.' },
			{ tab: 'audit', live: ['personal', 'project'], why: null },
		] as const;
		for (const c of cases) {
			await go(page, `/settings/${c.tab}`);
			const sw = page.locator('#scopeSw');
			await expect(sw).toHaveAttribute('data-scope', c.live[0]);
			for (const scope of ['personal', 'project'] as const) {
				const b = sw.locator(`button[data-scope="${scope}"]`);
				if ((c.live as readonly string[]).includes(scope)) {
					await expect(b).toBeEnabled();
				} else {
					await expect(b).toBeDisabled();
					await expect(b).toHaveAttribute('title', c.why ?? '');
				}
			}
			// D-4: access data is not a file a principal opens; D-10: no iyke
			// verbs for People yet (§15 N-5).
			await expect(page.getByRole('button', { name: 'Open file' })).toHaveCount(0);
			await expect(page.getByRole('button', { name: 'Copy the iyke command' })).toHaveCount(0);
			await page.getByRole('button', { name: 'Section menu' }).click();
			await expect(page.getByRole('menuitem', { name: /Copy as iyke/ })).toHaveCount(0);
			await expect(page.getByRole('menuitem', { name: /Open file/ })).toHaveCount(0);
			await expect(page.getByRole('menuitem', { name: /Reset section/ })).toBeVisible();
			await page.keyboard.press('Escape');
		}
		// Audit: both scopes live; Project filters to the active project.
		await page.locator('#scopeSw button[data-scope="project"]').click();
		await expect(page.locator('#scopeSw')).toHaveAttribute('data-scope', 'project');
		expect(pageErrors).toEqual([]);
	});
});

// ── T1 browser ─────────────────────────────────────────────────────────────

for (const mode of ['dark', 'light'] as const) {
	test.describe(`D-05 T1 browser — ${mode}`, () => {
		test(`sign-in: username and password; Pair this device with equal weight (D-1) (${mode})`, async ({
			page,
		}, testInfo) => {
			const { pageErrors } = await bootT1(page, mode, { signedIn: false });
			const signIn = page.locator('[data-state="sign-in"]');
			await expect(signIn).toBeVisible({ timeout: MIN });
			await expect(page.locator('html')).toHaveAttribute('data-mode', mode);
			await expect(signIn.getByRole('heading', { name: 'Ikenga' })).toBeVisible();
			await expect(signIn.getByPlaceholder('Username')).toBeVisible();
			await expect(signIn.getByPlaceholder('Password')).toBeVisible();
			// D-1: no email, no "Continue without an account", no sync copy.
			await expect(signIn).not.toContainText('Continue without an account');
			await expect(signIn).not.toContainText('syncs');
			const submit = signIn.getByRole('button', { name: 'Sign in' });
			const pair = signIn.getByRole('button', { name: 'Pair this device with a code' });
			const [a, b] = [await submit.boundingBox(), await pair.boundingBox()];
			expect(
				a &&
					b &&
					Math.round(a.width) === Math.round(b.width) &&
					Math.round(a.height) === Math.round(b.height)
			).toBe(true);
			await expectFocusRing(page, submit, { clear: true });
			await expectFocusRing(page, pair);
			await page.mouse.click(5, 5);
			await page.screenshot({ path: shotPath(testInfo, `people-sign-in-${mode}.png`) });
			expect(pageErrors).toEqual([]);
		});

		test(`profile-account: the account block and the sign-out confirm (D-2) (${mode})`, async ({
			page,
		}, testInfo) => {
			const { pageErrors } = await bootT1(page, mode);
			await expect(page.getByRole('main')).toBeVisible({ timeout: MIN });
			await go(page, '/settings/profile');
			const profile = page.locator('[data-state="profile-account"]');
			await expect(profile).toBeVisible();
			const account = profile.locator('[data-block="account"]');
			await expect(account).toContainText('ned');
			await expect(account).toContainText(ME.principal_id);
			await expect(account.getByText('admin', { exact: true })).toBeVisible();
			// D-2: no Syncs list, no email identity.
			await expect(profile).not.toContainText('Syncs');
			await expect(profile).not.toContainText('@');
			const signOut = account.getByRole('button', { name: 'Sign out' });
			await expectFocusRing(page, signOut);
			await page.mouse.move(0, 0);
			await page.screenshot({ path: shotPath(testInfo, `people-profile-account-${mode}.png`) });
			await signOut.click();
			const confirm = page.locator('[data-state="account-sign-out"]');
			await expect(confirm.getByRole('heading', { name: 'Sign out?' })).toBeVisible();
			await expect(confirm).toContainText('Your paired devices stay paired');
			await page.screenshot({ path: shotPath(testInfo, `people-profile-signout-${mode}.png`) });
			await confirm.getByRole('button', { name: 'Stay signed in' }).click();
			await expect(confirm).toHaveCount(0);
			expect(pageErrors).toEqual([]);
		});

		test(`members-shared: role menu, remove + Undo, invite revoke (${mode})`, async ({
			page,
		}, testInfo) => {
			const { pageErrors, mock } = await bootT1(page, mode, {
				responses: {
					access_member_set_role: {},
					access_member_remove: {},
					access_member_restore: {},
					access_invite_revoke: {},
				},
			});
			await expect(page.getByRole('main')).toBeVisible({ timeout: MIN });
			await go(page, '/settings/members');
			const members = page.locator('[data-state="members-shared"]');
			await expect(members).toBeVisible();
			const table = members.getByRole('table').first();
			await expect(table.locator('[data-member]')).toHaveCount(4);
			await expect(table.locator('[data-member="owner"]')).toContainText('you · this device');
			await expect(table.locator('[data-member="guest"]')).toContainText('plans/shell/board.html');
			await expect(members.locator('[data-invite="pending"]')).toContainText('guest@example.com');
			await expect(members).toContainText('Secrets are not on this list and never will be.');
			await expect(members.locator('[data-filebar="access-store"]')).toContainText(
				'server operator database'
			);
			await page.screenshot({ path: shotPath(testInfo, `people-members-shared-${mode}.png`) });

			// Role menu (§4.2, §4.3): the Owner row is fixed; a member's opens.
			await expect(
				table.locator('[data-member="owner"]').getByRole('button', { name: 'Owner', exact: true })
			).toBeDisabled();
			const role = table.getByRole('button', { name: "femi's role" });
			await expectFocusRing(page, role);
			await role.click();
			const menu = page.getByRole('menu');
			await expect(menu.getByRole('menuitemradio')).toHaveText([/Operator/, /Reviewer/, /Guest/]);
			await page.screenshot({ path: shotPath(testInfo, `people-members-rolemenu-${mode}.png`) });
			await menu.getByRole('menuitemradio', { name: /Operator/ }).click();
			await expect
				.poll(() => mock.calls.filter((c) => c.cmd === 'access_member_set_role').map((c) => c.args))
				.toEqual([{ projectId: 'royalti-co', principalId: 'p-femi', role: 'operator' }]);

			// Remove: a confirm, then a 10 s Undo (P-17).
			await table
				.locator('[data-member="reviewer"]')
				.getByRole('button', { name: 'Remove' })
				.click();
			const remove = page.locator('[data-state="members-remove"]');
			await expect(remove.getByRole('heading', { name: 'Remove femi?' })).toBeVisible();
			await page.screenshot({ path: shotPath(testInfo, `people-members-remove-${mode}.png`) });
			await remove.getByRole('button', { name: 'Remove' }).click();
			const toast = page.locator('[data-member-toast="removed"]');
			await expect(toast).toContainText('Removed femi');
			await page.screenshot({ path: shotPath(testInfo, `people-members-undo-${mode}.png`) });
			await toast.getByRole('button', { name: 'Undo' }).click();
			await expect
				.poll(() => mock.calls.filter((c) => c.cmd === 'access_member_restore').map((c) => c.args))
				.toEqual([{ projectId: 'royalti-co', principalId: 'p-femi' }]);

			// Invite revoke (§7.4): immediate, no Undo (D-6).
			const revoke = members
				.locator('[data-invite="pending"]')
				.getByRole('button', { name: 'Revoke invite' });
			await expectFocusRing(page, revoke);
			await revoke.click();
			await expect
				.poll(() => mock.calls.filter((c) => c.cmd === 'access_invite_revoke').map((c) => c.args))
				.toEqual([{ inviteId: 'inv-1' }]);
			expect(pageErrors).toEqual([]);
		});

		test(`share-kola: how, who, role, scope, expires; what they will see (D-9) (${mode})`, async ({
			page,
		}, testInfo) => {
			const { pageErrors } = await bootT1(page, mode);
			await expect(page.getByRole('main')).toBeVisible({ timeout: MIN });
			await go(page, '/settings/members');
			const members = page.locator('[data-state="members-shared"]');
			await members.getByRole('button', { name: 'Share kola' }).click();
			const sheet = page.locator('[data-state="share-kola"]');
			await expect(sheet.getByRole('heading', { name: /Share kola/ })).toBeVisible();
			await sheet.getByLabel('Who').fill('tomi@example.com');
			await expect(sheet.locator('[data-see-list] li')).toHaveCount(6);
			await expect(sheet.locator('[data-see-list] li[data-allowed="true"]')).toHaveCount(2);
			// D-9: "Create invite", not "Send invite".
			await expect(sheet.getByRole('button', { name: 'Create invite' })).toBeEnabled();
			await expect(sheet).not.toContainText('Send invite');
			await expectFocusRing(page, sheet.getByRole('button', { name: 'Link' }));
			await expectFocusRing(page, sheet.getByRole('button', { name: 'Create invite' }), {
				clear: true,
			});
			await page.mouse.move(0, 0);
			await page.screenshot({ path: shotPath(testInfo, `people-share-kola-${mode}.png`) });
			expect(pageErrors).toEqual([]);
		});

		test(`policies (T1): the matrix, Require Owner approval, spend caps disabled (D-11, D-15) (${mode})`, async ({
			page,
		}, testInfo) => {
			const { pageErrors } = await bootT1(page, mode, {
				responses: { access_policy_set_owner_approval: {} },
			});
			await expect(page.getByRole('main')).toBeVisible({ timeout: MIN });
			await go(page, '/settings/policies');
			const policies = page.locator('[data-state="policies"]');
			await expect(policies).toBeVisible();
			await expect(policies.locator('[data-matrix] button[data-cell]')).toHaveCount(18);
			await expect(policies.locator('[data-matrix] [data-cell="never"]')).toHaveCount(3);
			await expect(policies.locator('[data-spend-cap] input')).toHaveCount(3);
			for (const input of await policies.locator('[data-spend-cap] input').all())
				await expect(input).toBeDisabled();
			await expectFocusRing(
				page,
				policies.getByRole('button', { name: 'Reviewer · Dispatch to Chi: withheld' })
			);
			const approval = policies.getByRole('switch', { name: 'Require Owner approval' });
			await expectFocusRing(page, approval);
			await page.mouse.move(0, 0);
			await page.screenshot({ path: shotPath(testInfo, `people-policies-${mode}.png`) });
			await approval.click();
			await expect(policies.locator('[data-secrets-line]')).toContainText(
				'Asks that touch secrets still go to you.'
			);
			expect(pageErrors).toEqual([]);
		});
	});
}

// ── paired phone ───────────────────────────────────────────────────────────

const STATUS_DEVICE = {
	...STATUS_T0,
	principal: { principalId: 'p-ned', username: 'royalti-co', isAdmin: false },
	credential: { via: 'device', deviceId: 'd-pixel', tier: 'approve' },
	caps: ['files', 'sessions', 'dispatch', 'approve'],
	adminStrength: false,
};

for (const mode of ['dark', 'light'] as const) {
	test(`remote-client: sessions, a live permission card, the dispatch bar (D-7) (${mode})`, async ({
		page,
	}, testInfo) => {
		const pageErrors = trackPageErrors(page);
		await seed(page, mode);
		await installRemoteMock(page, {
			tier: 't0',
			responses: {
				access_status: STATUS_DEVICE,
				access_routing_get: { mode: 'any_approve', deviceId: null, deviceName: null },
				pty_terminal_list: [
					{
						pty_id: 'pty-3',
						label: 'claude · session 3',
						title: '',
						argv: ['claude'],
						status: 'running',
						foreground_command: { name: 'claude' },
					},
				],
				chi_list: [
					{
						run_id: 'run-nightly',
						engine_id: 'claude-code',
						status: 'running',
						brief: 'nightly-pulse',
					},
				],
				notifications_list: [
					{
						id: 41,
						kind: 'permission',
						title: 'claude wants to read royalti-server-v2.6/.env',
						body: 'Read · royalti-server-v2.6/.env',
						action: null,
						source: 'engine.claude-code',
						dedupeKey: 'permission:acp:41',
						count: 1,
						createdAt: NOW - MIN,
						updatedAt: NOW - MIN,
						readAt: null,
						resolvedAt: null,
						can_decide: true,
						waiting_on: null,
						can_allow_always: true,
					},
				],
			},
		});
		await page.goto('/', { waitUntil: 'domcontentloaded' });
		const client = page.locator('[data-state="remote-client"]');
		await expect(client).toBeVisible({ timeout: MIN });
		await expect(page).toHaveURL(/\/remote$/);
		await expect(client.getByRole('region', { name: 'Sessions' })).toContainText(
			'claude · session 3'
		);
		const card = client.locator('[data-card="live"]');
		await expect(card).toContainText('claude wants to read');
		await expect(card.getByRole('button', { name: 'Allow once' })).toBeVisible();
		await expect(card.getByRole('button', { name: 'Always for this project' })).toBeVisible();
		await expect(client.locator('form[data-dispatch="enabled"]')).toBeVisible();
		await expectFocusRing(page, card.getByRole('button', { name: 'Allow once' }), { clear: true });
		await expectFocusRing(page, card.getByRole('button', { name: 'Deny' }));
		await page.mouse.move(0, 0);
		await page.screenshot({ path: shotPath(testInfo, `people-remote-client-${mode}.png`) });
		await client
			.locator('> div')
			.first()
			.screenshot({ path: shotPath(testInfo, `people-remote-client-box-${mode}.png`) });
		expect(pageErrors).toEqual([]);
	});
}
