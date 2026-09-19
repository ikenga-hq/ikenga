// Frame smoke — WP-19, rail inventory updated by WP-03. Pins the frame as it
// stands on feat/phase-1-frame: the rail (Project · Chi · Ngwa · pins ·
// Settings), a sidebar region, the pane tree, Settings opening the settings
// nav, and no uncaught errors. No mode-specific sidebar titles (WP-04 owns
// the sidebar). Rail behaviour — keyboard, a11y, gloss, pin menu, the
// data-workspace hand-off — is the second describe below.

import { expect, type Page, type TestInfo, test } from '@playwright/test';
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

		// The three nouns and Settings (WP-03, src/shell/activity-bar.tsx).
		for (const label of ['Project', 'Chi', 'Ngwa', 'Settings']) {
			await expect(rail.getByRole('button', { name: label, exact: true })).toBeVisible();
		}
		// The pre-v16 modes and per-package rail buttons are gone: a package's
		// rail presence is a pin now (WP-22 seeds one per former rail icon).
		for (const label of ['App', 'Files', 'Sessions', 'Artifact grid', 'Packages', 'Demo Pkg']) {
			await expect(rail.getByRole('button', { name: label, exact: true })).toHaveCount(0);
		}

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
		// (g-state.md); the other modes' sidebar bodies are interim until WP-04.
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

// ── Rail (WP-03) ────────────────────────────────────────────────────────────
// Browser-mode proofs for the rail DoD: inventory + screenshots (R1), keyboard
// reachability with a visible focus ring (R2), the first-contact gloss (R3),
// the data-workspace hand-off iframe pkgs observe (R5) and the a11y line (R7:
// pin Move up / Move down, target size, hoverable tooltips, measured
// contrast). Screenshots land in $IKENGA_E2E_SHOTS when set, else in the
// test's output dir.

/** Three pins: the fixture's sectioned pin plus two section-less ones, the
 *  second shaped like a WP-22 seed (a package route, manifest icon name). */
const RAIL_PINS = [
	...MOCK_PINS,
	{
		...MOCK_PINS[0]!,
		id: 'pin-wiki',
		target: '/pkg/com.e2e.wiki',
		label: 'Wikipedia',
		iconLucide: 'book-open',
		sectionId: null,
		sortOrder: 0,
	},
	{
		...MOCK_PINS[0]!,
		id: 'pin-demo',
		target: '/pkg/com.e2e.demo',
		label: 'Demo Pkg',
		iconLucide: 'Box',
		sectionId: null,
		sortOrder: 1,
	},
];

async function bootRail(page: Page, opts: { glossSeen?: boolean } = {}) {
	const pageErrors = trackPageErrors(page);
	if (opts.glossSeen !== false) {
		await page.addInitScript(() => {
			localStorage.setItem('ikenga.gloss.seen', JSON.stringify(['ngwa', 'chi']));
		});
	}
	await installTauriMock(page, { responses: { activity_pins_list: RAIL_PINS } });
	await page.goto('/', { waitUntil: 'domcontentloaded' });
	const rail = page.getByRole('navigation', { name: 'Activity bar' });
	await expect(rail).toBeVisible({ timeout: 60_000 });
	await expect(rail.locator('[data-pin-id]')).toHaveCount(RAIL_PINS.length);
	return { rail, pageErrors };
}

function shotPath(testInfo: TestInfo, name: string): string {
	const dir = process.env.IKENGA_E2E_SHOTS;
	return dir ? `${dir.replace(/[\\/]$/, '')}/${name}` : testInfo.outputPath(name);
}

/** WCAG relative-luminance contrast between two computed CSS colours
 *  (`rgb(...)` / `rgba(...)`; a translucent colour is composited on `under`). */
async function contrastIn(
	page: Page,
	pairs: Array<{ fg: string; bg: string; under?: string }>
): Promise<number[]> {
	return page.evaluate((list) => {
		const parse = (c: string) => {
			const m = c.match(/rgba?\(([^)]+)\)/);
			if (!m) throw new Error(`not an rgb colour: ${c}`);
			const [r, g, b, a = '1'] = m[1]!.split(/[ ,/]+/).filter(Boolean);
			return [Number(r), Number(g), Number(b), Number(a)];
		};
		const over = (top: number[], under: number[]) => {
			const a = top[3]!;
			return [0, 1, 2].map((i) => top[i]! * a + under[i]! * (1 - a)).concat(1);
		};
		const lum = (rgb: number[]) => {
			const [r, g, b] = rgb.slice(0, 3).map((v) => {
				const s = v / 255;
				return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
			});
			return 0.2126 * r! + 0.7152 * g! + 0.0722 * b!;
		};
		return list.map(({ fg, bg, under }) => {
			const base = parse(under ?? bg);
			const b = over(parse(bg), base);
			const f = over(parse(fg), b);
			const [l1, l2] = [lum(f), lum(b)].sort((x, y) => y - x);
			return Math.round(((l1! + 0.05) / (l2! + 0.05)) * 100) / 100;
		});
	}, pairs);
}

async function tabToRailItem(page: Page, id: string): Promise<boolean> {
	for (let i = 0; i < 40; i++) {
		await page.keyboard.press('Tab');
		const at = await page.evaluate(
			() => (document.activeElement as HTMLElement | null)?.dataset.railItem ?? null
		);
		if (at === id) return true;
	}
	return false;
}

test.describe('rail (WP-03)', () => {
	test('R1 inventory: exactly Project · Chi · Ngwa · pins · Settings', async ({
		page,
	}, testInfo) => {
		const { rail, pageErrors } = await bootRail(page);
		const items = await rail
			.locator('[data-rail-item]')
			.evaluateAll((els) => els.map((el) => (el as HTMLElement).dataset.railItem));
		expect(items.filter((id) => id !== 'project-switcher')).toEqual([
			'project',
			'chi',
			'ngwa',
			'pin:pin-todos',
			'pin:pin-wiki',
			'pin:pin-demo',
			'settings',
		]);
		await expect(rail.getByRole('button', { name: 'Project', exact: true })).toHaveAttribute(
			'aria-current',
			'page'
		);
		// A seeded package pin renders a glyph even with a PascalCase icon name.
		await expect(rail.locator('[data-pin-id="pin-demo"] svg')).toHaveCount(1);
		await rail.screenshot({ path: shotPath(testInfo, 'rail.png') });
		await page.screenshot({ path: shotPath(testInfo, 'frame.png') });
		expect(pageErrors).toEqual([]);
	});

	test('R2 every rail item is reachable by keyboard with a visible focus ring', async ({
		page,
	}, testInfo) => {
		const { rail } = await bootRail(page);
		const count = await rail.locator('[data-rail-item]').count();

		// Tab into the rail: one tab stop, landing on the active key.
		expect(await tabToRailItem(page, 'project')).toBe(true);

		const seen: string[] = [];
		for (let i = 0; i < count; i++) {
			const state = await page.evaluate(() => {
				const el = document.activeElement as HTMLElement;
				return {
					id: el.dataset.railItem ?? '',
					focusVisible: el.matches(':focus-visible'),
					shadow: getComputedStyle(el).boxShadow,
				};
			});
			expect(state.focusVisible, state.id).toBe(true);
			expect(state.shadow, state.id).toMatch(/2px inset/);
			seen.push(state.id);
			if (i === 0) await rail.screenshot({ path: shotPath(testInfo, 'rail-focus.png') });
			await page.keyboard.press('ArrowDown');
		}
		expect(new Set(seen).size).toBe(count);

		// The rail is a single tab stop: the next Tab leaves it.
		await page.keyboard.press('Home');
		await page.keyboard.press('Tab');
		expect(
			await page.evaluate(
				() => (document.activeElement as HTMLElement | null)?.dataset.railItem ?? null
			)
		).toBeNull();
	});

	test('R3 the gloss shows once and dismisses itself', async ({ page }, testInfo) => {
		const { rail } = await bootRail(page, { glossSeen: false });
		const gloss = rail.locator('#rail-gloss');
		await expect(gloss).toBeVisible();
		await expect(gloss).toHaveText(/^Ngwa — your equipment/);
		await expect(gloss).toHaveCSS('pointer-events', 'none');
		// Page clip, not the rail's own box: the gloss sits beside the rail.
		await page.screenshot({
			path: shotPath(testInfo, 'rail-gloss.png'),
			clip: { x: 0, y: 0, width: 320, height: 260 },
		});
		await expect(gloss).toHaveCount(0, { timeout: 6_000 });
		expect(await page.evaluate(() => localStorage.getItem('ikenga.gloss.seen'))).toBe('["ngwa"]');
	});

	test('R5 a rail click changes <html data-workspace>; the iframe-host observer fires', async ({
		page,
	}) => {
		const { rail } = await bootRail(page);
		await expect(page.locator('html')).toHaveAttribute('data-workspace', 'project');
		// The exact observer pkg-iframe-host.tsx installs to re-push the theme
		// into every mounted iframe pkg.
		await page.evaluate(() => {
			const w = window as unknown as { __wsRecords: string[] };
			w.__wsRecords = [];
			new MutationObserver((recs) => {
				for (const r of recs) {
					if (r.attributeName === 'data-workspace') {
						w.__wsRecords.push(document.documentElement.getAttribute('data-workspace') ?? '');
					}
				}
			}).observe(document.documentElement, {
				attributes: true,
				attributeFilter: ['data-mode', 'data-theme', 'data-tint-strength', 'data-workspace'],
			});
		});
		for (const [label, ws] of [
			['Chi', 'chi'],
			['Ngwa', 'ngwa'],
			['Settings', 'settings'],
			['Project', 'project'],
		] as const) {
			await rail.getByRole('button', { name: label, exact: true }).click();
			await expect(page.locator('html')).toHaveAttribute('data-workspace', ws);
		}
		expect(
			await page.evaluate(() => (window as unknown as { __wsRecords: string[] }).__wsRecords)
		).toEqual(['chi', 'ngwa', 'settings', 'project']);
	});

	test('R7 pin menu has Move up / Move down (WCAG 2.5.7) and they reorder', async ({
		page,
	}, testInfo) => {
		const { rail } = await bootRail(page);
		const loose = () =>
			rail
				.locator('[data-section="__none"] [data-pin-id]')
				.evaluateAll((els) => els.map((el) => (el as HTMLElement).dataset.pinId));
		expect(await loose()).toEqual(['pin-wiki', 'pin-demo']);

		await rail.locator('[data-pin-id="pin-wiki"]').click({ button: 'right' });
		const menu = page.getByRole('menu');
		await expect(menu.getByRole('menuitem', { name: 'Move up' })).toHaveAttribute(
			'aria-disabled',
			'true'
		);
		await page.screenshot({ path: shotPath(testInfo, 'rail-pin-menu.png') });
		await menu.getByRole('menuitem', { name: 'Move down' }).click();
		await expect.poll(loose).toEqual(['pin-demo', 'pin-wiki']);

		await rail.locator('[data-pin-id="pin-wiki"]').click({ button: 'right' });
		await page.getByRole('menu').getByRole('menuitem', { name: 'Move up' }).click();
		await expect.poll(loose).toEqual(['pin-wiki', 'pin-demo']);
	});

	test('R7 targets ≥ 24×24 CSS px (WCAG 2.5.8)', async ({ page }) => {
		const { rail } = await bootRail(page);
		const sizes = await rail.locator('[data-rail-item]').evaluateAll((els) =>
			els.map((el) => {
				const r = el.getBoundingClientRect();
				return { id: (el as HTMLElement).dataset.railItem, w: r.width, h: r.height };
			})
		);
		console.log('[rail target sizes]', JSON.stringify(sizes));
		for (const s of sizes) {
			expect(s.w, s.id).toBeGreaterThanOrEqual(24);
			expect(s.h, s.id).toBeGreaterThanOrEqual(24);
		}
	});

	test('R7 tooltips are hoverable, persistent and dismissible (WCAG 1.4.13)', async ({ page }) => {
		const { rail } = await bootRail(page);
		await rail.getByRole('button', { name: 'Chi', exact: true }).hover();
		const tip = page.locator('[data-rail-tooltip][data-state]');
		await expect(tip).toBeVisible();
		await expect(tip).toContainText('Chi');
		// The key hint comes from the keymap registry, as a <kbd>.
		await expect(tip.locator('kbd').first()).toHaveText(/^(⌘2|Ctrl\+2)$/);
		// Hoverable: moving the pointer onto the tooltip keeps it open.
		const box = (await tip.boundingBox())!;
		await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2, { steps: 4 });
		await page.waitForTimeout(500);
		await expect(tip).toBeVisible();
		// Dismissible without moving the pointer.
		await page.keyboard.press('Escape');
		await expect(tip).toHaveCount(0);
	});

	test('R7 rest / hover / active / focus contrast ≥ 3:1 (WCAG 1.4.11)', async ({ page }) => {
		const { rail } = await bootRail(page);
		const railBg = await rail.evaluate((el) => getComputedStyle(el).backgroundColor);
		const style = (label: string) =>
			rail.getByRole('button', { name: label, exact: true }).evaluate((el) => {
				const cs = getComputedStyle(el);
				return { color: cs.color, bg: cs.backgroundColor, shadow: cs.boxShadow };
			});

		// Rest (Chi is inactive): glyph on the rail.
		const rest = await style('Chi');
		// Active (Project): tinted glyph on the tint background.
		const active = await style('Project');
		// Hover.
		await rail.getByRole('button', { name: 'Ngwa', exact: true }).hover();
		await page.waitForTimeout(250);
		const hover = await style('Ngwa');
		// Focus ring against the active tint, then against the rail.
		await page.mouse.move(600, 400);
		expect(await tabToRailItem(page, 'project')).toBe(true);
		const focused = await style('Project');
		const ring = focused.shadow.match(/rgba?\([^)]+\)/)?.[0] ?? '';
		await page.keyboard.press('ArrowDown');
		const focusedRest = await style('Chi');
		const ringRest = focusedRest.shadow.match(/rgba?\([^)]+\)/)?.[0] ?? '';

		const [restC, activeC, hoverC, ringOnActive, ringOnRail] = await contrastIn(page, [
			{ fg: rest.color, bg: rest.bg, under: railBg },
			{ fg: active.color, bg: active.bg, under: railBg },
			{ fg: hover.color, bg: hover.bg, under: railBg },
			{ fg: ring, bg: active.bg, under: railBg },
			{ fg: ringRest, bg: focusedRest.bg, under: railBg },
		]);
		const measured = { restC, activeC, hoverC, ringOnActive, ringOnRail };
		console.log('[rail contrast]', JSON.stringify({ measured, railBg, rest, active, hover, ring }));
		for (const [name, c] of Object.entries(measured)) {
			expect(c, name).toBeGreaterThanOrEqual(3);
		}
	});
});
