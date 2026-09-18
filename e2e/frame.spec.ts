// Frame smoke — WP-19. Pins TODAY's frame (before any Phase 1 change) so the
// harness itself is proven green before the frame starts moving. Later WPs
// add their own specs next to this one; WP-20's no-op slot refactor must keep
// this one green unchanged.

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
		await expect(page.getByRole('navigation', { name: /sidebar$/ })).toBeVisible();

		// ── Pane tree ───────────────────────────────────────────────────────
		const main = page.getByRole('main');
		await expect(main).toBeVisible();
		const panes = main.locator('[data-pane-id]');
		await expect(panes.first()).toBeVisible();
		await expect(main.locator('[data-pane-id][data-focused="true"]')).toHaveCount(1);
		await expect(panes.first().getByRole('button', { name: 'New tab' })).toBeVisible();

		// Rail → sidebar wiring: picking another mode retitles the sidebar
		// (CORE_TITLES in src/shell/sidebar.tsx).
		await rail.getByRole('button', { name: 'Files', exact: true }).click();
		await expect(page.getByRole('navigation', { name: 'Files sidebar' })).toBeVisible();

		// Record (not assert) which host commands had no canned answer, so a
		// spec author can see what to add to the fixture when the frame grows.
		const unmocked = await unmockedCommands(page);
		testInfo.annotations.push({ type: 'unmocked-commands', description: unmocked.join(', ') });

		expect(pageErrors, `uncaught page errors:\n${pageErrors.join('\n\n')}`).toEqual([]);
	});
});
