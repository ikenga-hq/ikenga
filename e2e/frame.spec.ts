// Frame smoke — WP-19. Pins the frame as it stands on feat/phase-1-frame
// (after WP-02's G-STATE v16 store, before WP-03/WP-04 rework the rail and
// sidebar), so the harness itself is proven green before the frame starts
// moving. It asserts only what those WPs are not about to change: the rail
// renders, a sidebar region exists, the pane tree mounts, Settings opens the
// settings nav, and nothing throws. No mode-specific sidebar titles.

import { expect, type Page, test } from '@playwright/test';
import {
	installTauriMock,
	MOCK_PINS,
	MOCK_PROJECTS,
	unmockedCommands,
} from './fixtures/tauri-mock';

/** Collect uncaught exceptions / unhandled rejections from the page. Console
 *  errors are not failures on their own — several boot paths log-and-continue
 *  by design when a host service is absent. */
function trackPageErrors(page: Page): string[] {
	const errors: string[] = [];
	page.on('pageerror', (err) => errors.push(err.stack ?? err.message));
	return errors;
}

test.describe('frame smoke (current frame)', () => {
	test('rail, sidebar and pane tree render with no uncaught errors', async ({ page }, testInfo) => {
		const pageErrors = trackPageErrors(page);
		await installTauriMock(page);
		await page.goto('/', { waitUntil: 'domcontentloaded' });

		// ── Rail ────────────────────────────────────────────────────────────
		const rail = page.getByRole('navigation', { name: 'Activity bar' });
		await expect(rail).toBeVisible({ timeout: 60_000 });

		// Core modes, top and bottom groups (CORE_TOP / CORE_BOTTOM in
		// src/shell/activity-bar.tsx).
		for (const label of [
			'App',
			'Files',
			'Sessions',
			'Artifact grid',
			'Packages',
			'Ngwa',
			'Settings',
		]) {
			await expect(rail.getByRole('button', { name: label, exact: true })).toBeVisible();
		}

		// Pkg entry from the mocked kernel snapshot's activity_bar registry.
		await expect(rail.getByRole('button', { name: 'Demo Pkg' })).toBeVisible();

		// User pin from activity_pins_list, grouped under its section.
		const pin = MOCK_PINS[0]!;
		const pinButton = rail.locator(`[data-pin-id="${pin.id}"]`);
		await expect(pinButton).toBeVisible();
		await expect(pinButton).toHaveAttribute('aria-label', pin.label);
		await expect(rail.locator(`[data-section="${pin.sectionId}"]`)).toBeVisible();

		// Active-project indicator reflects project_list + project_get_active.
		await expect(
			rail.getByRole('button', { name: new RegExp(`^Project: ${MOCK_PROJECTS[0]!.display_name}`) })
		).toBeVisible();

		// ── Sidebar ─────────────────────────────────────────────────────────
		// Region only: its title follows the active mode, which is in flux.
		await expect(page.getByRole('navigation', { name: /sidebar$/ })).toBeVisible();

		// ── Pane tree ───────────────────────────────────────────────────────
		const main = page.getByRole('main');
		await expect(main).toBeVisible();
		const panes = main.locator('[data-pane-id]');
		await expect(panes.first()).toBeVisible();
		await expect(main.locator('[data-pane-id][data-focused="true"]')).toHaveCount(1);
		await expect(panes.first().getByRole('button', { name: 'New tab' })).toBeVisible();

		// Rail → sidebar wiring. Settings is a CoreMode on both sides of v16
		// (g-state.md), so selecting it is stable across WP-03/WP-04; the other
		// rail items' sidebar titles are interim and deliberately not pinned here.
		await rail.getByRole('button', { name: 'Settings', exact: true }).click();
		const settingsNav = page.getByRole('navigation', { name: 'Settings navigation' });
		await expect(settingsNav).toBeVisible();
		await expect(settingsNav.getByText('Appearance', { exact: true })).toBeVisible();

		// Record (not assert) which host commands had no canned answer, so a
		// spec author can see what to add to the fixture when the frame grows.
		const unmocked = await unmockedCommands(page);
		testInfo.annotations.push({ type: 'unmocked-commands', description: unmocked.join(', ') });

		expect(pageErrors, `uncaught page errors:\n${pageErrors.join('\n\n')}`).toEqual([]);
	});
});
