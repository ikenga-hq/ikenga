// WP-71c — Chi seats in the frame (D-09 `designs/seats-companion.html`, the
// Phase 7 Part A close). Browser-mode checks of the seven D-09 states, each
// through its `data-state` root, in dark and light, plus the Part A
// checklist lines a mocked host can show (01-plan.md §Phase 7 verification):
//
//   roster    four seats in order; selecting one targets it (the chip) and
//             scopes the status bar's cost (G-93: never "— — ctx"); every
//             `seat:` literal on screen is `seat:<project>/<name>` (G-95)
//   empty     no seats: the one sentence and two actions
//   create    live validation ("review is already a seat"), gemini disabled,
//             the canonical scratchpad preview; Esc cancels
//   vacant    @docs: Resume / Fill / Clear, in that order; Clear keeps the
//             pad and is undoable
//   popout    @review's live terminal (rehydrated from the mocked host) is
//             popped out from its menu: the rail reads `seats-popout`, the
//             row's Window 2 signal pulses, the notice says the address is
//             unchanged (bottom-right, 06 §5.5), and the address itself
//             does not change
//   dispatch  the picker lists seats first
//   rest      the 36 px strip: one monogram per seat, the run pulse on
//             @nightly
//
// Plus: Remove seat… confirms, can be kept, and is undone without a host
// call (G-102); ↑/↓ rove with a visible outline; switching project switches
// the roster (DEC-68); the D-05 local states (Profile, Devices, Locked) and
// the app lock's behaviour: it locks when the host reports idle and on Lock
// now (Ctrl+Shift+L), and unlocks by PIN (a wrong one keeps it locked).
// Keeping focus across OS focus changes needs a real OS: owed, not here.
//
// Screenshots go to `$IKENGA_E2E_SHOTS` when set, else to the test output
// dir (gitignored) — the design sweep compares them against the locked
// mockups at the same `?state=` and `&m=light`.

import { expect, type Page, type TestInfo, test } from '@playwright/test';
import {
	emitHostEvent,
	installTauriMock,
	invokedCommands,
	MOCK_APP_LOCK_LOCKED,
	MOCK_APP_LOCK_UNLOCKED,
	MOCK_SEAT_PROJECT,
	type MockResponses,
	seatResponses,
	seatTerminalResponses,
	setMockResponse,
} from './fixtures/tauri-mock';

/** §3.1: `seat:<project>/<name>`, both halves `[a-z0-9-]`. */
const SEAT_SCOPE = /^seat:[a-z0-9][a-z0-9-]*\/[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/;

type Mode = 'dark' | 'light';

function trackPageErrors(page: Page): string[] {
	const errors: string[] = [];
	page.on('pageerror', (err) => errors.push(err.stack ?? err.message));
	return errors;
}

function shotPath(testInfo: TestInfo, name: string): string {
	const dir = process.env.IKENGA_E2E_SHOTS;
	return dir ? `${dir.replace(/[\\/]$/, '')}/${name}` : testInfo.outputPath(name);
}

/**
 * Before any app script: the Chi gloss is marked seen (it would sit over the
 * rail on a first run), and the appearance mode is seeded where the theme
 * store keeps its instant-paint copy (`ikenga.theme`, v2). With no settings
 * file in the mocked host, `hydrateAppearanceFromRust` keeps that copy.
 */
async function seed(page: Page, mode: Mode) {
	await page.emulateMedia({ colorScheme: mode });
	await page.addInitScript((m) => {
		localStorage.setItem('ikenga.gloss.seen', JSON.stringify(['ngwa', 'chi']));
		localStorage.setItem(
			'ikenga.theme',
			JSON.stringify({
				state: { theme: 'A', mode: m, density: 'comfortable', tintStrength: 'subtle', workspace: 'project' },
				version: 2,
			})
		);
	}, mode);
}

async function boot(page: Page, opts: { mode?: Mode; seats?: unknown[]; responses?: MockResponses } = {}) {
	const mode = opts.mode ?? 'dark';
	const pageErrors = trackPageErrors(page);
	await seed(page, mode);
	await installTauriMock(page, { responses: { ...seatResponses(opts.seats), ...(opts.responses ?? {}) } });
	await page.goto('/', { waitUntil: 'domcontentloaded' });
	await expect(page.getByRole('main')).toBeVisible({ timeout: 60_000 });
	await expect(page.locator('html')).toHaveAttribute('data-mode', mode);
	return pageErrors;
}

/** The Companion rests at its strip on boot (§5.1); open it. */
async function expandCompanion(page: Page) {
	const strip = page.locator('aside[data-state="seats-rest"]');
	await expect(strip).toBeVisible();
	await strip.locator('button[aria-expanded="false"]').click();
	await expect(page.getByRole('textbox', { name: 'Dispatch an instruction' })).toBeVisible();
}

function rail(page: Page) {
	return page.getByRole('listbox', { name: 'Seats and unseated sessions' });
}

function seatRow(page: Page, name: string) {
	return rail(page).getByRole('option', { name: new RegExp(`^@${name},`) });
}

/** Click a row on its name, clear of the row's own buttons. */
async function selectRow(page: Page, name: string) {
	await seatRow(page, name).click({ position: { x: 24, y: 14 } });
	await expect(seatRow(page, name)).toHaveAttribute('aria-selected', 'true');
}

/** Every `seat:` literal the Companion shows, one text node at a time (so
 *  adjacent spans can't run together). */
async function seatLiterals(page: Page): Promise<string[]> {
	return page.locator('aside[aria-label="Chi companion"]').evaluate((root) => {
		const out: string[] = [];
		const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
		for (let n = walker.nextNode(); n; n = walker.nextNode()) {
			for (const m of (n.textContent ?? '').matchAll(/\bseat:[^\s"“”·,)]+/g)) out.push(m[0]);
		}
		return out;
	});
}

for (const mode of ['dark', 'light'] as const) {
	test.describe(`D-09 seat rail — ${mode}`, () => {
		test(`rest: the strip keeps a monogram per seat and the run pulse (${mode})`, async ({ page }, testInfo) => {
			const pageErrors = await boot(page, { mode });
			const strip = page.locator('aside[data-state="seats-rest"]');
			await expect(strip).toBeVisible();
			await expect(strip.locator('[data-mono]')).toHaveText(['le', 're', 'ni', 'do']);
			// The run pulse on @nightly survives at rest (D-09 `rest`).
			const pulse = strip.locator('[data-mono="nightly"] [data-dot="run"]');
			await expect(pulse).toHaveCount(1);
			expect(await pulse.evaluate((el) => getComputedStyle(el).animationName)).not.toBe('none');
			// `.reststrip .vert`: mono, uppercase.
			const label = strip.locator('[data-strip-label]');
			expect(await label.evaluate((el) => getComputedStyle(el).textTransform)).toBe('uppercase');
			await page.screenshot({ path: shotPath(testInfo, `seats-rest-${mode}.png`) });
			expect(pageErrors).toEqual([]);
		});

		test(`roster: seats in order; selection targets and scopes; seat: literals are canonical (${mode})`, async ({
			page,
		}, testInfo) => {
			const pageErrors = await boot(page, { mode });
			await expandCompanion(page);
			await expect(page.locator('[data-state="seats-roster"]')).toBeVisible();
			expect(await rail(page).locator('[data-seat]').evaluateAll((els) => els.map((e) => e.getAttribute('data-seat')))).toEqual([
				'seat-lead',
				'seat-review',
				'seat-nightly',
				'seat-docs',
			]);
			await expect(rail(page).getByText('Unseated', { exact: true })).toBeVisible();

			// Selecting @review targets it (the chip speaks in the seat) …
			await selectRow(page, 'review');
			await expect(page.getByRole('button', { name: /^Dispatch target: @review · codex · session \d+\./ })).toBeVisible();
			// … and scopes the status bar's cost: codex reported nothing, so one
			// "—" with the tooltip — never G-93's "session — — ctx".
			const cost = page.getByRole('toolbar', { name: 'Status bar' }).locator('[data-seg="cost"]');
			await expect(cost).toHaveText(/^session\s*—$/);
			await expect(cost).toHaveAttribute('title', /not reported by this engine yet/);

			// The selected row shows its scratchpad by its canonical scope (G-95).
			await expect(seatRow(page, 'review')).toContainText(`seat:${MOCK_SEAT_PROJECT.id}/review`);
			const literals = await seatLiterals(page);
			expect(literals.length).toBeGreaterThan(0);
			for (const lit of literals) expect(lit).toMatch(SEAT_SCOPE);
			// The iyke line addresses the seat by name (valid CLI grammar, §6a).
			await expect(page.locator('[data-iyke-line]')).toHaveText('terminal-send --seat review "…"');

			await page.screenshot({ path: shotPath(testInfo, `seats-roster-${mode}.png`) });
			expect(pageErrors).toEqual([]);
		});

		test(`vacant: @docs offers Resume, Fill, Clear — Clear keeps the pad and undoes (${mode})`, async ({
			page,
		}, testInfo) => {
			const pageErrors = await boot(page, { mode });
			await expandCompanion(page);
			await selectRow(page, 'docs');
			const panel = page.locator('[data-state="seats-vacant"]');
			await expect(panel).toBeVisible();
			await expect(panel).toContainText(`seat:${MOCK_SEAT_PROJECT.id}/docs`);
			await expect(panel).toContainText('STATUS.md pass half done');
			const actions = panel.getByRole('button', { name: /^(Resume session \d+|Fill with a new session|Clear seat)$/ });
			await expect(actions).toHaveText([/^Resume session \d+$/, 'Fill with a new session', 'Clear seat']);
			await expect(actions.first()).toBeEnabled();
			await page.screenshot({ path: shotPath(testInfo, `seats-vacant-${mode}.png`) });

			// Clear (DEC-69b): history goes, the scratchpad stays, 8 s Undo.
			await panel.getByRole('button', { name: 'Clear seat' }).click();
			await expect(
				page.getByText(`Cleared docs — session history forgotten; scratchpad seat:${MOCK_SEAT_PROJECT.id}/docs kept`)
			).toBeVisible();
			await page.getByRole('button', { name: 'Undo', exact: true }).click();
			await expect(panel.getByRole('button', { name: /^Resume session \d+$/ })).toBeVisible();
			expect((await invokedCommands(page)).map((c) => c.cmd)).not.toContain('seats_clear');
			expect(pageErrors).toEqual([]);
		});

		test(`create: live validation, engines, canonical scratchpad; Esc cancels (${mode})`, async ({ page }, testInfo) => {
			const pageErrors = await boot(page, { mode });
			await expandCompanion(page);
			await page.getByRole('button', { name: 'New seat', exact: true }).click();
			const form = page.locator('[data-state="seats-create"]');
			await expect(form).toBeVisible();
			const name = form.locator('#seat-form-name');
			await name.fill('review');
			await expect(form.getByText('review is already a seat')).toBeVisible();
			await expect(form.getByRole('button', { name: 'Create seat', exact: true })).toBeDisabled();
			await expect(form.getByRole('radio', { name: 'claude-code', exact: true })).toHaveAttribute('aria-checked', 'true');
			await expect(form.getByRole('radio', { name: 'gemini', exact: true })).toBeDisabled();
			await expect(form.locator('[data-seat-pad-preview]')).toHaveText(`seat:${MOCK_SEAT_PROJECT.id}/review`);
			await page.screenshot({ path: shotPath(testInfo, `seats-create-${mode}.png`) });

			await name.fill('scribe');
			await expect(form.locator('[data-seat-pad-preview]')).toHaveText(`seat:${MOCK_SEAT_PROJECT.id}/scribe`);
			await expect(form.locator('[data-seat-form-iyke]')).toContainText('seat create scribe --engine claude-code');
			await name.press('Escape');
			await expect(form).toHaveCount(0);
			await expect(page.locator('[data-state="seats-roster"]')).toBeVisible();
			expect(pageErrors).toEqual([]);
		});

		test(`dispatch: the picker lists seats first (${mode})`, async ({ page }, testInfo) => {
			const pageErrors = await boot(page, { mode });
			await expandCompanion(page);
			// A seat target resolves without an engine probe, so the input is live.
			await selectRow(page, 'lead');
			await page.getByRole('textbox', { name: 'Dispatch an instruction' }).fill('run the release-status check');
			await page.getByRole('button', { name: /^Dispatch target: @lead/ }).click();
			const menu = page.locator('[data-state="seats-dispatch"]');
			await expect(menu).toBeVisible();
			await expect(menu.getByRole('group').first()).toHaveAttribute('aria-label', 'Seats');
			await expect(menu.getByRole('group', { name: 'Seats' }).getByRole('menuitemradio')).toHaveText([
				/^@lead/,
				/^@review/,
				/^@nightly/,
				/^@docs/,
			]);
			await expect(menu.getByRole('group', { name: 'Seats' }).getByRole('menuitemradio').first()).toHaveAttribute(
				'aria-checked',
				'true'
			);
			await page.screenshot({ path: shotPath(testInfo, `seats-dispatch-${mode}.png`) });
			await page.keyboard.press('Escape');
			await expect(menu).toHaveCount(0);
			expect(pageErrors).toEqual([]);
		});

		test(`popout: Pop out moves @review to Window 2; the address is unchanged (${mode})`, async ({
			page,
		}, testInfo) => {
			const pageErrors = await boot(page, { mode, responses: seatTerminalResponses() });
			await expandCompanion(page);
			await selectRow(page, 'review');
			const row = seatRow(page, 'review');
			// In no pane yet, and a first sighting is never "moved".
			await expect(row.locator('[data-signal="window"]')).toHaveCount(0);
			await expect(page.locator('[data-state="seats-roster"]')).toBeVisible();

			await row.click({ button: 'right', position: { x: 24, y: 14 } });
			const menu = page.getByRole('menu', { name: 'Seat actions for @review' });
			await menu.getByRole('menuitem', { name: /^Pop out/ }).click();

			await expect(page.locator('[data-state="seats-popout"]')).toBeVisible();
			const signal = row.locator('[data-signal="window"]');
			await expect(signal).toHaveText('Window 2');
			await expect(signal).toHaveAttribute('data-moved', 'true');
			const toast = page.getByText('review moved to Window 2 — its address is unchanged');
			await expect(toast).toBeVisible();
			// 06 §5.5 / D-09: toasts sit bottom-right, above the status bar.
			const box = await toast.boundingBox();
			const statusBar = await page.getByRole('toolbar', { name: 'Status bar' }).boundingBox();
			expect(box && statusBar && box.y + box.height <= statusBar.y).toBe(true);
			expect(box && box.x > 1440 / 2).toBe(true);
			// The address is not the mount (D-09 rule 2): same row, same scope.
			await expect(row).toContainText(`seat:${MOCK_SEAT_PROJECT.id}/review`);
			expect((await invokedCommands(page)).map((c) => c.cmd)).not.toContain('seats_move');
			await page.screenshot({ path: shotPath(testInfo, `seats-popout-${mode}.png`) });

			// The highlight settles (RAIL_MOVED_MS); the rail reads roster again.
			await expect(page.locator('[data-state="seats-roster"]')).toBeVisible({ timeout: 10_000 });
			await expect(signal).not.toHaveAttribute('data-moved', 'true');
			expect(pageErrors).toEqual([]);
		});

		test(`empty: one sentence, New seat and Seat this session… (${mode})`, async ({ page }, testInfo) => {
			const pageErrors = await boot(page, { mode, seats: [] });
			await expandCompanion(page);
			const empty = page.locator('[data-state="seats-empty"]');
			await expect(empty).toBeVisible();
			await expect(empty.getByText('A seat keeps an agent’s name when its pane moves or its session ends.')).toBeVisible();
			await expect(empty.getByRole('button', { name: 'New seat', exact: true })).toBeVisible();
			// No open session to seat in this host, so it says why.
			await expect(empty.getByRole('button', { name: 'Seat this session…' })).toBeDisabled();
			await page.screenshot({ path: shotPath(testInfo, `seats-empty-${mode}.png`) });
			expect(pageErrors).toEqual([]);
		});
	});
}

test.describe('D-09 seat rail — behaviour', () => {
	test('Remove seat… confirms, Keep it keeps it, Undo restores it with no host call (G-102)', async ({ page }) => {
		const pageErrors = await boot(page);
		await expandCompanion(page);

		const openRemove = async () => {
			await seatRow(page, 'docs').click({ button: 'right', position: { x: 24, y: 14 } });
			const menu = page.getByRole('menu', { name: 'Seat actions for @docs' });
			await expect(menu).toBeVisible();
			// D-09 order: … End session, then Remove seat… last.
			await expect(menu.getByRole('menuitem').last()).toHaveText(/^Remove seat…/);
			await menu.getByRole('menuitem', { name: /^Remove seat…/ }).click();
			const dialog = page.getByRole('dialog', { name: 'Remove seat @docs' });
			await expect(dialog).toBeVisible();
			await expect(dialog).toContainText(`seat:${MOCK_SEAT_PROJECT.id}/docs`);
			return dialog;
		};

		let dialog = await openRemove();
		await dialog.getByRole('button', { name: 'Keep it' }).click();
		await expect(dialog).toHaveCount(0);
		await expect(seatRow(page, 'docs')).toBeVisible();

		dialog = await openRemove();
		await dialog.getByRole('button', { name: 'Remove seat', exact: true }).click();
		await expect(seatRow(page, 'docs')).toHaveCount(0);
		await expect(page.getByText('Removed seat docs')).toBeVisible();
		await page.getByRole('button', { name: 'Undo', exact: true }).click();
		await expect(seatRow(page, 'docs')).toBeVisible();
		expect((await invokedCommands(page)).map((c) => c.cmd)).not.toContain('seats_remove');
		expect(pageErrors).toEqual([]);
	});

	test('↑/↓ rove and select (one tab stop) with a visible outline on the focused row', async ({ page }) => {
		const pageErrors = await boot(page);
		await expandCompanion(page);
		await selectRow(page, 'lead');
		await seatRow(page, 'lead').focus();
		await page.keyboard.press('ArrowDown');
		const review = seatRow(page, 'review');
		await expect(review).toHaveAttribute('aria-selected', 'true');
		await expect(review).toBeFocused();
		await expect(rail(page).locator('[role="option"][tabindex="0"]')).toHaveCount(1);
		const outline = await review.evaluate((el) => {
			const cs = getComputedStyle(el);
			return { style: cs.outlineStyle, width: cs.outlineWidth };
		});
		expect(outline).toEqual({ style: 'solid', width: '2px' });
		expect(pageErrors).toEqual([]);
	});

	test('switching project switches the roster (DEC-68)', async ({ page }) => {
		const pageErrors = await boot(page);
		await expandCompanion(page);
		await expect(seatRow(page, 'lead')).toBeVisible();
		const explorer = page.getByRole('navigation', { name: 'Explorer sidebar' });
		await explorer.getByTestId('explorer-project-chip').click();
		const popover = page.locator('[role="dialog"], [data-radix-popper-content-wrapper]').first();
		await expect(popover).toBeVisible();
		await popover.getByText('Label Ops', { exact: true }).click();
		// `label-ops` has no seats in this host: the rail follows the project.
		await expect(page.locator('[data-state="seats-empty"]')).toBeVisible();
		await expect(seatRow(page, 'lead')).toHaveCount(0);
		expect(pageErrors).toEqual([]);
	});
});

for (const mode of ['dark', 'light'] as const) {
	test.describe(`D-05 local states — ${mode}`, () => {
		test(`profile: Local profile and App lock; no Account block (G-98) (${mode})`, async ({ page }, testInfo) => {
			const pageErrors = await boot(page, { mode });
			const address = page.getByRole('textbox', { name: 'Address' });
			await address.fill('/settings/profile');
			await address.press('Enter');
			const profile = page.locator('[data-state="profile"]');
			await expect(profile).toBeVisible();
			await expect(profile.getByRole('heading', { name: 'People, devices and access' })).toBeVisible();
			await expect(profile.getByRole('heading', { name: 'Local profile' })).toBeVisible();
			await expect(profile.getByRole('heading', { name: 'App lock' })).toBeVisible();
			await expect(profile.getByRole('heading', { name: 'Account' })).toHaveCount(0);
			// The App lock block, with a host that has the lock commands.
			await expect(profile.getByRole('switch', { name: 'Lock when idle' })).toBeVisible();
			await expect(profile.getByRole('button', { name: /^Lock now/ })).toBeVisible();
			// The People tabs show keyboard focus as a solid outline.
			const tab = page.getByRole('navigation', { name: 'People sections' }).getByRole('link').first();
			await tab.focus();
			await page.keyboard.press('Shift+Tab');
			await page.keyboard.press('Tab');
			await expect(tab).toBeFocused();
			expect(await tab.evaluate((el) => getComputedStyle(el).outlineStyle)).toBe('solid');
			await page.screenshot({ path: shotPath(testInfo, `people-profile-${mode}.png`) });
			expect(pageErrors).toEqual([]);
		});

		test(`devices: paired-devices table, no shared-token note (${mode})`, async ({ page }, testInfo) => {
			const pageErrors = await boot(page, {
				mode,
				responses: {
					access_devices_list: [
						{
							deviceId: 'host',
							kind: 'host',
							name: 'This Mac',
							platform: 'macOS',
							tier: 'full',
							pairedAt: 0,
							lastSeenAt: null,
							lastSeenAddr: null,
							liveSockets: 0,
							thisDevice: true,
						},
					],
					// WP-75 (G-ACCESS §5.1): the "Permission requests" control.
					access_status: {
						tier: 't0',
						store: 'ok',
						principal: { principalId: 'p', username: 'ned', isAdmin: false },
						credential: { via: 'operator', deviceId: 'host', tier: 'full' },
						caps: ['files', 'sessions', 'dispatch', 'approve', 'install', 'settings', 'secrets'],
						adminStrength: true,
						publicUrl: null,
						sharingEnabled: false,
						share: null,
					},
					access_routing_get: { mode: 'any_approve', deviceId: null, deviceName: null },
				},
			});
			const address = page.getByRole('textbox', { name: 'Address' });
			await address.fill('/settings/devices');
			await address.press('Enter');
			const devices = page.locator('[data-state="devices"]');
			await expect(devices).toBeVisible();
			// WP-74b (G-ACCESS §15 N-7): each remote device holds its own grant, so the
			// WP-72 shared-bearer "Not a security boundary" note is gone.
			await expect(devices.getByRole('note', { name: 'Not a security boundary' })).toHaveCount(0);
			await expect(devices.getByText('Permission requests')).toBeVisible();
			const routing = devices.locator('[data-routing="any_approve"]');
			await expect(routing).toBeVisible();
			await expect(routing.getByRole('tab', { name: 'Any paired device' })).toHaveAttribute(
				'aria-selected',
				'true'
			);
			await expect(routing.getByRole('tab', { name: 'This device only' })).toBeEnabled();
			// D-05 approveNote: the host is not a paired device, and with no
			// paired approver there is nobody to name.
			const note = routing.locator('[data-note="any"]');
			await expect(note).toContainText('Any device with approve may answer.');
			await expect(note).not.toContainText('Right now that is');
			await expect(devices.getByText('Set by how the server was started.')).toBeVisible();
			await expect(devices.getByText(/No paired devices\. Pair a phone/)).toBeVisible();
			await page.screenshot({ path: shotPath(testInfo, `people-devices-${mode}.png`) });
			expect(pageErrors).toEqual([]);
		});

		test(`locked: the lock covers the app and keeps focus off it (${mode})`, async ({ page }, testInfo) => {
			const pageErrors = trackPageErrors(page);
			await seed(page, mode);
			await installTauriMock(page, {
				responses: {
					...seatResponses(),
					...seatTerminalResponses(),
					chi_list: [
						{ run_id: 'e2e-run-nightly', engine_id: 'claude-code', status: 'running', owner: 'ui' },
						{ run_id: 'e2e-run-old', engine_id: 'claude-code', status: 'done', owner: 'ui' },
					],
					app_lock_status: MOCK_APP_LOCK_LOCKED,
				},
			});
			await page.goto('/', { waitUntil: 'domcontentloaded' });
			const lock = page.locator('[data-state="locked"]');
			await expect(lock).toBeVisible({ timeout: 60_000 });
			await expect(page.locator('html')).toHaveAttribute('data-mode', mode);
			await expect(lock.getByRole('heading', { name: 'Locked' })).toBeVisible();
			// D-05's fine print counts what keeps going, app-wide.
			await expect(lock).toContainText('2 sessions and 1 run are still going underneath.');
			const field = lock.getByRole('textbox', { name: 'PIN or passphrase' });
			await expect(field).toBeFocused();
			// Unlock is next, and shows where focus is (D-05 `.btn:focus-visible`):
			// a solid 2 px outline, not one `outline-none` cancels.
			await page.keyboard.press('Tab');
			const unlock = lock.getByRole('button', { name: 'Unlock', exact: true });
			await expect(unlock).toBeFocused();
			expect(
				await unlock.evaluate((el) => {
					const cs = getComputedStyle(el);
					return { style: cs.outlineStyle, width: cs.outlineWidth };
				})
			).toEqual({ style: 'solid', width: '2px' });
			await field.focus();
			// Focus never lands in the app underneath (it is inert): Tab walks
			// the lock's own controls, and past the last one the page itself
			// (the browser chrome) — never a frame control.
			for (let i = 0; i < 6; i++) {
				await page.keyboard.press('Tab');
				const where = await page.evaluate(() => {
					const el = document.activeElement;
					if (!el || el === document.body) return 'page';
					return el.closest('[data-state="locked"]') ? 'lock' : 'app';
				});
				expect(where).not.toBe('app');
			}
			await page.screenshot({ path: shotPath(testInfo, `people-locked-${mode}.png`) });
			expect(pageErrors).toEqual([]);
		});
	});
}

test.describe('D-05 app lock — behaviour', () => {
	test('locks when the host reports idle, and on Lock now (Ctrl+Shift+L)', async ({ page }) => {
		const pageErrors = await boot(page);
		const lock = page.locator('[data-state="locked"]');
		await expect(lock).toHaveCount(0);

		// Idle: Rust's ticker locks and emits `app-lock://changed`; the frame
		// refetches and covers itself.
		await setMockResponse(page, 'app_lock_status', MOCK_APP_LOCK_LOCKED);
		await emitHostEvent(page, 'app-lock://changed', null);
		await expect(lock).toBeVisible();
		await expect(lock).toContainText('ned-desktop · locked after 15 min idle');

		// Back to unlocked, then Lock now from the keyboard, outside any field.
		await setMockResponse(page, 'app_lock_status', MOCK_APP_LOCK_UNLOCKED);
		await emitHostEvent(page, 'app-lock://changed', null);
		await expect(lock).toHaveCount(0);
		await setMockResponse(page, 'app_lock_lock', { ...MOCK_APP_LOCK_LOCKED, reason: 'manual' });
		await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
		await page.keyboard.press('Control+Shift+L');
		await expect(lock).toBeVisible();
		await expect(lock).toContainText('locked with Lock now');
		expect((await invokedCommands(page)).map((c) => c.cmd)).toContain('app_lock_lock');
		expect(pageErrors).toEqual([]);
	});

	test('unlocks with the PIN; a wrong one keeps it locked', async ({ page }) => {
		const pageErrors = trackPageErrors(page);
		await seed(page, 'dark');
		await installTauriMock(page, {
			responses: {
				...seatResponses(),
				app_lock_status: MOCK_APP_LOCK_LOCKED,
				app_lock_unlock: {
					ok: false,
					error: 'Wrong PIN. Four attempts left.',
					status: { ...MOCK_APP_LOCK_LOCKED, attemptsLeft: 4 },
				},
			},
		});
		await page.goto('/', { waitUntil: 'domcontentloaded' });
		const lock = page.locator('[data-state="locked"]');
		await expect(lock).toBeVisible({ timeout: 60_000 });
		const field = lock.getByRole('textbox', { name: 'PIN or passphrase' });

		await field.fill('0000');
		await field.press('Enter');
		await expect(lock.getByRole('alert')).toContainText('Wrong PIN');
		await expect(lock).toBeVisible();

		await setMockResponse(page, 'app_lock_unlock', { ok: true, error: null, status: MOCK_APP_LOCK_UNLOCKED });
		await setMockResponse(page, 'app_lock_status', MOCK_APP_LOCK_UNLOCKED);
		await field.fill('2468');
		await field.press('Enter');
		await expect(lock).toHaveCount(0);
		await expect(page.getByRole('main')).toBeVisible();
		const unlocks = (await invokedCommands(page)).filter((c) => c.cmd === 'app_lock_unlock');
		expect(unlocks.map((c) => (c.args as { secret: string }).secret)).toEqual(['0000', '2468']);
		expect(pageErrors).toEqual([]);
	});
});
