// Frame chrome — WP-09. Title row, status bar, the one banner slot, and the
// palette's Shortcuts view, driven in browser mode against the mocked host.
//
// Screenshots (DoD T7): light + dark crops of the title row and the status
// bar are written to `$WP09_SCREENSHOT_DIR` when set (the PR attaches them),
// else to this test's output dir.

import { expect, type Page, test, type TestInfo } from '@playwright/test';
import path from 'node:path';
import {
	installTauriMock,
	MOCK_PROJECTS,
	MOCK_SETTINGS,
	type MockResponses,
} from './fixtures/tauri-mock';

const ACTIVE = MOCK_PROJECTS[1]!; // Label Ops — has a root_path, so git applies.

/** What the git pkg's `repo.snapshot` sidecar method prints. */
const GIT_SNAPSHOT_STDOUT = `${JSON.stringify({
	jsonrpc: '2.0',
	id: 1,
	result: {
		ok: true,
		snapshot: {
			branch: 'feat/frame-chrome',
			detached: false,
			headSha: '87ac712aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
			staged: 1,
			unstaged: 2,
			untracked: 0,
			conflicted: 0,
		},
	},
})}\n`;

const RESPONSES: MockResponses = {
	project_get_active: ACTIVE,
	pkg_sidecar_call: { ok: true, stdout: GIT_SNAPSHOT_STDOUT, stderr: '', code: 0 },
	// Two drafts parked at the approve gate → "2 permissions pending".
	pa_actions_list: [{ id: 'draft-1' }, { id: 'draft-2' }],
	// One recorded permission violation for the demo pkg → "1 violation".
	pkg_permission_violations_list: [
		{
			id: 1,
			pkg_id: 'com.e2e.demo',
			kind: 'fs.read',
			detail: 'e2e',
			created_at: 1_760_000_000_000,
		},
	],
	pkg_trust_list: [],
	// A pkg parked for capability review → the trust-review banner is eligible.
	pkg_trust_list_pending: [
		{
			pkg_id: 'com.e2e.demo',
			manifest_version: '0.0.2',
			old_capabilities: '{}',
			new_capabilities: '{"net":true}',
			prior_approved_at_ms: 1_760_000_000_000,
		},
	],
	settings_get_all: { ...MOCK_SETTINGS, 'agent.defaultEngineId': JSON.stringify('claude-code') },
};

function trackPageErrors(page: Page): string[] {
	const errors: string[] = [];
	page.on('pageerror', (err) => errors.push(err.stack ?? err.message));
	return errors;
}

async function boot(page: Page, colorScheme: 'light' | 'dark' = 'dark') {
	await page.emulateMedia({ colorScheme });
	await installTauriMock(page, { responses: RESPONSES });
	await page.goto('/', { waitUntil: 'domcontentloaded' });
	await expect(page.getByRole('toolbar', { name: 'Title row' })).toBeVisible({ timeout: 60_000 });
	await expect(page.getByRole('toolbar', { name: 'Status bar' })).toBeVisible();
}

function shotPath(testInfo: TestInfo, name: string): string {
	const dir = process.env.WP09_SCREENSHOT_DIR;
	return dir ? path.join(dir, name) : testInfo.outputPath(name);
}

test.describe('frame chrome (WP-09)', () => {
	test('title row: project chip + branch chip, plus the ≡ menu off macOS (T1, D-08 native-menu-win)', async ({ page }) => {
		const errors = trackPageErrors(page);
		await boot(page);
		const row = page.getByRole('toolbar', { name: 'Title row' });
		await expect(row.getByTestId('title-branch-chip')).toContainText('feat/frame-chrome');
		await expect(row.getByTestId('title-project-chip')).toContainText(ACTIVE.display_name);
		// Every focusable thing in the row. macOS has the native menu bar, so
		// only the two chips; Windows/Linux add WP-46's ≡ cascade at the far left.
		const isMac = await page.evaluate(() => /Mac/i.test(navigator.platform));
		await expect(
			row.locator('button, a[href], input, select, textarea, [tabindex]:not([tabindex="-1"])')
		).toHaveCount(isMac ? 2 : 3);
		expect(errors, errors.join('\n\n')).toEqual([]);
	});

	test('status bar: counts, zero-hiding, and the approvals deep link (T2, T3)', async ({
		page,
	}) => {
		const errors = trackPageErrors(page);
		await boot(page);
		const bar = page.getByRole('toolbar', { name: 'Status bar' });
		await expect(bar.locator('[data-seg="branch"]')).toContainText('feat/frame-chrome');
		await expect(bar.locator('[data-seg="modified"]')).toHaveText('3 modified');
		await expect(bar.locator('[data-seg="project"]')).toContainText(ACTIVE.display_name);
		await expect(bar.locator('[data-seg="ngwa-installed"]')).toHaveText('1 installed');
		await expect(bar.locator('[data-seg="ngwa-violations"]')).toHaveText('1 violation');
		// Zero segments are not rendered at all.
		await expect(bar.locator('[data-seg="ngwa-updates"]')).toHaveCount(0);
		await expect(bar.locator('[data-seg="runs"]')).toHaveCount(0);
		await expect(bar.locator('[data-seg="cost"]')).toHaveCount(0);
		await expect(bar.locator('[data-seg="engine"]')).toContainText('claude-code');
		await expect(bar.locator('[data-slot="notifications-bell"]')).toHaveCount(1);

		const perm = bar.locator('[data-seg="permissions"]');
		await expect(perm).toContainText('2 permissions pending');
		await perm.click();
		// The focused pane now shows the approvals route.
		await expect
			.poll(async () =>
				page.evaluate(() => {
					// Address bar of the focused pane: text or input value.
					const el = document.querySelector('[data-pane-id][data-focused="true"]');
					if (!el) return '';
					const inputs = Array.from(el.querySelectorAll('input')).map((i) => i.value);
					return `${el.textContent ?? ''} ${inputs.join(' ')}`;
				})
			)
			.toContain('/outbox/approvals');
		expect(errors, errors.join('\n\n')).toEqual([]);
	});

	test('banners: one slot, one banner at a time', async ({ page }) => {
		await boot(page);
		const slot = page.getByTestId('banner-slot');
		await expect(slot.locator('[data-banner="trust-review"]')).toBeVisible();
		await expect(slot.locator('[data-banner="trust-review"]')).toContainText('capability review');
		// Exactly one non-empty, visible banner wrapper.
		const shown = await slot
			.locator('[data-banner]')
			.evaluateAll(
				(els) => els.filter((el) => !(el as HTMLElement).hidden && el.childNodes.length > 0).length
			);
		expect(shown).toBe(1);
	});

	test('`?` and Ctrl+/ open the grouped Shortcuts view (T5)', async ({ page }) => {
		await boot(page);
		await page.locator('body').click({ position: { x: 5, y: 450 } });
		await page.keyboard.press('Shift+?');
		const dialog = page.getByRole('dialog', { name: 'Keyboard shortcuts' });
		await expect(dialog).toBeVisible();
		for (const heading of ['Rail', 'Command palette', 'Explorer', 'Panes and tabs', 'Help']) {
			await expect(dialog.getByRole('heading', { name: heading })).toBeVisible();
		}
		await expect(dialog.locator('[data-command="shortcuts.open"]')).toContainText('Ctrl+/');
		await page.keyboard.press('Escape');
		await expect(dialog).toHaveCount(0);

		await page.keyboard.press('Control+/');
		await expect(dialog).toBeVisible();
		await page.keyboard.type('split');
		await expect(dialog.locator('[data-command]')).toHaveCount(2);
		await page.keyboard.press('Escape');

		// The status-bar shortcuts item opens the same view.
		await page.getByRole('button', { name: /^Keyboard shortcuts/ }).click();
		await expect(dialog).toBeVisible();
	});

	test('keyboard: title chips and the status bar are reachable and operable (T6)', async ({
		page,
	}) => {
		await boot(page);
		const chip = page.getByTestId('title-project-chip');
		await chip.focus();
		await page.keyboard.press('Tab');
		await expect(page.getByTestId('title-branch-chip')).toBeFocused();
		await page.keyboard.press('Shift+Tab');
		await expect(chip).toBeFocused();
		await page.keyboard.press('Enter');
		await expect(page.getByText('Switch project')).toBeVisible();
		await page.keyboard.press('Escape');

		const bar = page.getByRole('toolbar', { name: 'Status bar' });
		// One tab stop for the whole bar.
		await expect(bar.locator('button[tabindex="0"]')).toHaveCount(1);
		await bar.locator('button[tabindex="0"]').focus();
		await expect(bar.locator('[data-seg="branch"]')).toBeFocused();
		await page.keyboard.press('ArrowRight');
		await expect(bar.locator('[data-seg="modified"]')).toBeFocused();
		await page.keyboard.press('End');
		await expect(bar.locator('[data-seg="shortcuts"]')).toBeFocused();
		await page.keyboard.press('Enter');
		await expect(page.getByRole('dialog', { name: 'Keyboard shortcuts' })).toBeVisible();
	});

	test('theme toggle is a palette action; light + dark screenshots (T7, T8)', async ({
		page,
	}, testInfo) => {
		await boot(page, 'light');
		const html = page.locator('html');

		// Dark (the store's default mode).
		await expect(html).toHaveAttribute('data-mode', 'dark');
		await page
			.getByRole('toolbar', { name: 'Title row' })
			.screenshot({ path: shotPath(testInfo, 'title-row-dark.png') });
		await page
			.getByRole('toolbar', { name: 'Status bar' })
			.screenshot({ path: shotPath(testInfo, 'status-bar-dark.png') });
		await page.screenshot({ path: shotPath(testInfo, 'frame-dark.png') });

		// ⌘K → "Toggle theme" → Dark → System, which resolves to light here.
		await page.locator('body').click({ position: { x: 5, y: 450 } });
		await page.keyboard.press('Control+k');
		const palette = page.getByRole('dialog', { name: 'Command palette' });
		await expect(palette).toBeVisible();
		await page.keyboard.type('toggle theme');
		const row = palette.getByRole('option', { name: /Toggle theme: Dark → System/ });
		await expect(row).toBeVisible();
		await page.keyboard.press('Enter');
		await expect(
			palette.getByRole('option', { name: /Toggle theme: System → Light/ })
		).toBeVisible();
		await page.keyboard.press('Escape');
		await expect(html).toHaveAttribute('data-mode-source', 'system');
		await expect(html).toHaveAttribute('data-mode', 'light');

		await page
			.getByRole('toolbar', { name: 'Title row' })
			.screenshot({ path: shotPath(testInfo, 'title-row-light.png') });
		await page
			.getByRole('toolbar', { name: 'Status bar' })
			.screenshot({ path: shotPath(testInfo, 'status-bar-light.png') });
		await page.screenshot({ path: shotPath(testInfo, 'frame-light.png') });
	});
});
