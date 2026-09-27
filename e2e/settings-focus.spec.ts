// Keyboard focus is visible on Settings controls (WCAG 2.4.7 / 2.4.11).
//
// Tailwind v4's `outline-none` sets `--tw-outline-style: none`, and
// `focus-visible:outline-2` draws with `outline-style: var(--tw-outline-style)`,
// so a control carrying both, and no `focus-visible:outline-solid`, shows no
// focus indicator at all. `e2e/seats.spec.ts` pins the D-05 / D-09 surfaces;
// this spec pins the Settings ones: the Appearance segmented controls and
// theme cards (`routes/settings/appearance.tsx`).

import { expect, type Locator, type Page, test } from '@playwright/test';
import { installTauriMock } from './fixtures/tauri-mock';

async function boot(page: Page): Promise<string[]> {
	const pageErrors: string[] = [];
	page.on('pageerror', (err) => pageErrors.push(err.stack ?? err.message));
	// The Chi / Ngwa gloss would otherwise sit over the rail on a first run.
	await page.addInitScript(() => {
		localStorage.setItem('ikenga.gloss.seen', JSON.stringify(['ngwa', 'chi']));
	});
	await installTauriMock(page);
	await page.goto('/', { waitUntil: 'domcontentloaded' });
	await expect(page.getByRole('main')).toBeVisible({ timeout: 60_000 });
	return pageErrors;
}

async function openSettings(page: Page, path: string) {
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

async function outline(el: Locator) {
	return el.evaluate((node) => {
		const cs = getComputedStyle(node);
		return { style: cs.outlineStyle, width: cs.outlineWidth };
	});
}

test.describe('Settings controls show keyboard focus', () => {
	test('Appearance: a segmented control and a theme card draw a solid 2px outline', async ({ page }) => {
		const pageErrors = await boot(page);
		await openSettings(page, '/settings/appearance');

		const density = page.getByRole('group', { name: 'Density' });
		await expect(density).toBeVisible();
		const densityOption = density.locator('button[aria-pressed="true"]');
		await tabTo(page, densityOption);
		expect(await outline(densityOption)).toEqual({ style: 'solid', width: '2px' });

		const themeCard = page.locator('button[aria-pressed="true"]:has(> div[aria-hidden="true"])').first();
		await expect(themeCard).toBeVisible();
		await tabTo(page, themeCard);
		expect(await outline(themeCard)).toEqual({ style: 'solid', width: '2px' });

		expect(pageErrors).toEqual([]);
	});
});
