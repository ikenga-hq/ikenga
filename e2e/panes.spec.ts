// WP-07 pane chrome — browser-mode DoD (P2, P3, P5 per plans/shell-ux-rearchitecture
// 05-tracking.md §WP-07). These need real layout/paint, so they run here rather
// than in vitest (see e2e/README.md's harness split and
// src/shell/panes/pane.merged-row.test.tsx for the component-level half of
// the same DoD lines).

import { expect, type Page, test } from '@playwright/test';
import { installTauriMock } from './fixtures/tauri-mock';

async function boot(page: Page) {
	await installTauriMock(page);
	await page.goto('/', { waitUntil: 'domcontentloaded' });
	const main = page.getByRole('main');
	await expect(main).toBeVisible({ timeout: 60_000 });
	return main;
}

/** Splits the currently-focused pane right via the ⌘\ / Ctrl+\ binding
 *  (`mod = metaKey || ctrlKey`, workspace.tsx — do-not-touch handler). */
async function splitRight(page: Page) {
	await page.keyboard.press('Control+\\');
}

test.describe('pane chrome (§4, §6A.1–6A.3)', () => {
	test('merged row at 1 tab, tab strip returns at 2 tabs', async ({ page }) => {
		const main = await boot(page);
		const pane = main.locator('[data-pane-id]').first();

		// One tab: no tablist, but the address input and "+ New tab" are
		// present in the merged row.
		await expect(pane.getByRole('tablist')).toHaveCount(0);
		await expect(pane.getByRole('button', { name: 'New tab' })).toBeVisible();

		// Add a second tab via the new-tab menu → a route.
		await pane.getByRole('button', { name: 'New tab' }).click();
		await page.locator('.fixed.z-50').getByText('Todos', { exact: true }).click();

		// Tab strip returns.
		await expect(pane.getByRole('tablist')).toHaveCount(1);
		await expect(pane.getByRole('tab')).toHaveCount(2);

		// Closing back down to one tab restores the merged row. The close
		// button is always in the DOM (`tab-strip.tsx` renders it whenever
		// `closable`; only its opacity is hover-gated), so this doesn't need
		// a hover first and isn't conditional on it being found.
		await pane.getByRole('tab').first().getByRole('button', { name: 'Close tab' }).click();
		await expect(pane.getByRole('tablist')).toHaveCount(0);
		await expect(pane.getByRole('button', { name: 'New tab' })).toBeVisible();
	});

	test('6-pane layout has no toolbar overflow', async ({ page }) => {
		const main = await boot(page);
		// Split to the 6-leaf cap (MAX_LEAVES) — 5 splits from the initial 1 pane.
		for (let i = 0; i < 5; i++) {
			await splitRight(page);
		}
		const panes = main.locator('[data-pane-id]');
		await expect(panes).toHaveCount(6);

		const overflowing = await page.evaluate((sel) => {
			const rows = Array.from(document.querySelectorAll(sel)) as HTMLElement[];
			return rows.filter((row) => row.scrollWidth > row.clientWidth + 1).length;
		}, '[data-pane-id] > .flex.shrink-0');
		expect(overflowing).toBe(0);
	});

	test('pane tools reveal on the focused pane and cause no layout shift', async ({ page }) => {
		const main = await boot(page);
		await splitRight(page);
		const panes = main.locator('[data-pane-id]');
		await expect(panes).toHaveCount(2);

		const unfocused = panes.filter({ hasNot: page.locator('[data-focused="true"]') }).first();
		// Actually select by attribute directly — `data-focused` lives on the
		// pane root itself, not a descendant.
		const focusedPane = main.locator('[data-pane-id][data-focused="true"]');
		const idlePane = main.locator('[data-pane-id][data-focused="false"]').first();

		await expect(focusedPane).toHaveCount(1);

		// Focused pane: tools visible without hovering.
		const focusedTools = focusedPane.locator('.pane-tools');
		await expect(focusedTools).toHaveCSS('opacity', '1');

		// Idle, unfocused pane: tools present but invisible at rest.
		const idleTools = idlePane.locator('.pane-tools');
		await expect(idleTools).toHaveCSS('opacity', '0');

		// Measure the row before/after hovering the idle pane into reveal —
		// the reveal must be opacity-only, never a reflow (§4.5, §6A.1).
		const rowBefore = await idlePane.locator('.pane-tools').first().boundingBox();
		const siblingBefore = await idlePane
			.getByRole('button', { name: 'New tab' })
			.boundingBox()
			.catch(() => null);
		await idlePane.hover();
		await expect(idleTools).toHaveCSS('opacity', '1');
		const rowAfter = await idlePane.locator('.pane-tools').first().boundingBox();
		const siblingAfter = await idlePane
			.getByRole('button', { name: 'New tab' })
			.boundingBox()
			.catch(() => null);

		expect(rowBefore?.x).toBeCloseTo(rowAfter?.x ?? -1, 0);
		expect(rowBefore?.y).toBeCloseTo(rowAfter?.y ?? -1, 0);
		if (siblingBefore && siblingAfter) {
			expect(siblingBefore.x).toBeCloseTo(siblingAfter.x, 0);
		}
		void unfocused;
	});

	test('reduced motion: the reveal has no transition', async ({ page }) => {
		await page.emulateMedia({ reducedMotion: 'reduce' });
		const main = await boot(page);
		const tools = main.locator('.pane-tools').first();
		const transition = await tools.evaluate((el) => getComputedStyle(el).transitionDuration);
		// Every listed duration must be 0s under reduced motion.
		for (const d of transition.split(',')) {
			expect(d.trim()).toBe('0s');
		}
	});

	test('pane tools are each ≥24×24 CSS px (WCAG 2.5.8)', async ({ page }) => {
		const main = await boot(page);
		const focusedPane = main.locator('[data-pane-id][data-focused="true"]');
		const tools = focusedPane.locator('.pane-tools button');
		const count = await tools.count();
		expect(count).toBeGreaterThan(0);
		const measurements: string[] = [];
		for (let i = 0; i < count; i++) {
			const box = await tools.nth(i).boundingBox();
			const name = await tools.nth(i).getAttribute('aria-label');
			expect(box).not.toBeNull();
			measurements.push(`${name ?? `#${i}`}: ${box!.width.toFixed(1)}×${box!.height.toFixed(1)}`);
			expect(box!.width).toBeGreaterThanOrEqual(24);
			expect(box!.height).toBeGreaterThanOrEqual(24);
		}
		test.info().annotations.push({
			type: 'pane-tools-target-size',
			description: measurements.join('; '),
		});
	});

	test('an idle pane\'s tool stays hoverable mid-reveal, and its tooltip is reachable (WCAG 1.4.13)', async ({
		page,
	}) => {
		const main = await boot(page);
		await splitRight(page);
		const idlePane = main.locator('[data-pane-id][data-focused="false"]').first();
		await expect(idlePane).toHaveCount(1);

		const idleTools = idlePane.locator('.pane-tools');
		const moreButton = idlePane.getByRole('button', { name: 'More pane actions' });

		// At rest (not focused, not hovered) the idle pane's tools are
		// invisible — this is the "mid-reveal" starting point.
		await expect(idleTools).toHaveCSS('opacity', '0');

		// Hovering the pane (not the button directly) is what triggers the
		// reveal transition; hovering the button itself while it's still
		// opacity:0 proves the element was never removed from the hit-test
		// (a `display`/`visibility` swap instead of `opacity` would make this
		// hover land on whatever is *underneath* it instead).
		await moreButton.hover({ force: true });
		await expect(idleTools).toHaveCSS('opacity', '1');
		// Once revealed, the button is a normal hoverable, visible target —
		// its title-derived tooltip stays reachable because pointer-events
		// were never toggled off during the opacity transition.
		await expect(moreButton).toBeVisible();
		const pe = await moreButton.evaluate((el) => getComputedStyle(el).pointerEvents);
		expect(pe).not.toBe('none');
		const elAtCenter = await moreButton.evaluate((el) => {
			const r = el.getBoundingClientRect();
			const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
			return hit === el || (el.contains(hit) ?? false);
		});
		expect(elAtCenter).toBe(true);
	});

	test('resting control count in a 4-pane layout', async ({ page }) => {
		const main = await boot(page);
		await splitRight(page);
		await splitRight(page);
		await splitRight(page);
		const panes = main.locator('[data-pane-id]');
		await expect(panes).toHaveCount(4);

		// Move the mouse off any pane so nothing is mid-hover-reveal — "resting"
		// means only the one focused pane's tools are visible.
		await page.mouse.move(2, 2);

		// Count every visible, enabled control inside the pane-chrome region
		// (tab strip / merged row + tools) across all four panes — "pane-chrome
		// controls", not the pane content itself.
		const count = await page.evaluate((sel) => {
			const nodes = Array.from(document.querySelectorAll(sel)) as HTMLElement[];
			let n = 0;
			for (const el of nodes) {
				// `checkVisibility({ checkOpacity: true })` walks the ancestor
				// chain — needed here because the reveal sets `opacity: 0` on the
				// `.pane-tools` *wrapper*, not on each button, and a plain
				// `getComputedStyle(button).opacity` reads the button's own
				// (always 1) opacity, not its ancestor's.
				if (!el.checkVisibility?.({ checkOpacity: true, checkVisibilityCSS: true })) continue;
				n++;
			}
			return n;
		}, '[data-pane-id] > .flex.shrink-0 button, [data-pane-id] > .flex.shrink-0 [role="tab"]');

		test.info().annotations.push({
			type: 'resting-control-count',
			description: `${count} controls counted across 4 panes (buttons + tabs in each pane's chrome row; a pane-tools cluster at opacity 0 is excluded). v4 reference: 12.`,
		});
		expect(count).toBeLessThanOrEqual(12);
	});

	test('a11y line: focus and hover contrast ≥ 3:1 (WCAG 1.4.11)', async ({ page }) => {
		const main = await boot(page);
		const focusedPane = main.locator('[data-pane-id][data-focused="true"]');

		// Focused-pane ring vs. its own background — `pane.tsx`'s
		// `--tw-ring-color` (existing, pre-WP-07 focus treatment): a
		// `color-mix(..., 40%, transparent)` token, i.e. semi-transparent by
		// design. Resolve the function the way a browser does (assign it as
		// `color` on a throwaway element, read back computed `rgba(...)`),
		// then alpha-composite it over the pane background before measuring —
		// contrast on a translucent ring is only meaningful against what it
		// actually renders on top of, not its own un-composited channel.
		const [ringColor, paneBg] = await focusedPane.evaluate((el) => {
			const raw = getComputedStyle(el).getPropertyValue('--tw-ring-color').trim();
			const probe = document.createElement('div');
			probe.style.color = raw;
			document.body.appendChild(probe);
			const resolved = getComputedStyle(probe).color;
			probe.remove();
			return [resolved, getComputedStyle(el).backgroundColor];
		});
		const compositedRing = compositeOver(ringColor, paneBg);
		expect(contrastRatio(compositedRing, paneBg)).toBeGreaterThanOrEqual(3);

		// Hovered `⋯` button background vs. its resting (unhovered) background —
		// the new §6A.1 tools cluster. `bg-accent`/`transition-colors` (150ms,
		// icon-button.tsx) means a background read immediately after `.hover()`
		// can land mid-transition — a single `expect.poll` that just waits for
		// "differs from resting" isn't enough, because the *first* differing
		// frame is exactly a mid-transition sample (this is what made the
		// assertion flaky: 1/10 runs caught a frame at alpha 0.297 instead of
		// the settled 0.65, measuring 2.31:1). IconButton's transition also
		// carries `motion-reduce:transition-none` (Tailwind's
		// `prefers-reduced-motion` variant), so emulating reduced motion for
		// this measurement removes the transition entirely — the hover
		// background is the settled value the instant `.hover()` resolves,
		// with no frame to race. Only this measurement runs under reduced
		// motion; it doesn't affect the focus-ring reads below, which don't
		// depend on transitions.
		const moreButton = focusedPane.getByRole('button', { name: 'More pane actions' });
		const restingBg = await moreButton.evaluate((el) => getComputedStyle(el).backgroundColor);
		await page.emulateMedia({ reducedMotion: 'reduce' });
		await moreButton.hover();
		// Belt-and-braces even with the transition removed: poll until two
		// consecutive reads agree (settled) and the value differs from rest,
		// rather than trusting a single post-hover read.
		await expect
			.poll(
				async () => {
					const a = await moreButton.evaluate((el) => getComputedStyle(el).backgroundColor);
					const b = await moreButton.evaluate((el) => getComputedStyle(el).backgroundColor);
					return a === b && a !== restingBg ? a : null;
				},
				{
					message: 'waiting for the hover background to settle',
					timeout: 2000,
				}
			)
			.not.toBeNull();
		const hoverBg = await moreButton.evaluate((el) => getComputedStyle(el).backgroundColor);
		const hoverFg = await moreButton.evaluate((el) => getComputedStyle(el).color);
		// Reset — only the hover-background measurement above needs reduced
		// motion; the focus-ring reads below don't depend on transitions, and
		// leaving reduced motion off matches the default environment.
		await page.emulateMedia({ reducedMotion: 'no-preference' });
		// Composite both resting and hover backgrounds over the pane background
		// before comparing — a `color-mix()` background (like the transparent
		// resting state, and Chrome may serialize the hover mix as `oklab(...)`
		// rather than `rgb()`/`color(srgb ...)` — see `parseRgba` — only reads
		// as a real color once alpha-composited onto what it actually paints
		// over (same reasoning as the focus-ring measurement above).
		const compositedResting = compositeOver(restingBg, paneBg);
		const compositedHover = compositeOver(hoverBg, paneBg);
		const hoverVsRestingContrast = contrastRatio(compositedHover, compositedResting);
		test.info().annotations.push({
			type: 'contrast-measurements',
			description: `focus ring ${ringColor} composited over pane bg ${paneBg} → ${compositedRing}, contrast ${contrastRatio(compositedRing, paneBg).toFixed(2)}:1; hover bg ${hoverBg} (composited ${compositedHover}) vs resting bg ${restingBg} (composited ${compositedResting}), non-text contrast ${hoverVsRestingContrast.toFixed(2)}:1; hover fg ${hoverFg} vs hover bg ${contrastRatio(hoverFg, compositedHover).toFixed(2)}:1.`,
		});
		// The hover state must differ from rest (a real non-text indicator, not
		// just a11y-inert) with at least the 3:1 WCAG 1.4.11 non-text minimum,
		// and the icon itself must clear text contrast against the hover bg.
		expect(hoverBg).not.toBe(restingBg);
		expect(hoverVsRestingContrast).toBeGreaterThanOrEqual(3);
		expect(contrastRatio(hoverFg, compositedHover)).toBeGreaterThanOrEqual(3);

		// Keyboard focus-visible ring on the pane-tools buttons themselves
		// (distinct from the pane-level focus ring measured above). Required
		// fix from the R2 verdict: IconButton's shared `focus-visible:ring-ring`
		// (→ `--primary`) measured 2.74:1 for these two buttons against the
		// pane background — below 3:1. `pane-toolbar.tsx` now overrides the
		// ring *color* to the same `color-mix(in_srgb, var(--fg) 65%,
		// transparent)` swatch already proven ≥3:1 for hover, via
		// `PANE_TOOLS_FOCUS`. Move focus onto the button with a real `Tab`
		// keypress (not `.focus()`, which Chromium doesn't reliably treat as
		// a keyboard interaction for `:focus-visible`) so the ring actually
		// paints.
		const refreshButton = focusedPane.getByRole('button', { name: 'Refresh pane' });
		await refreshButton.focus();
		await page.keyboard.press('Tab');
		await expect(moreButton).toBeFocused();
		expect(await moreButton.evaluate((el) => el.matches(':focus-visible'))).toBe(true);

		const [focusRingColor, focusRingPaneBg] = await moreButton.evaluate((el) => {
			const raw = getComputedStyle(el).getPropertyValue('--tw-ring-color').trim();
			const probe = document.createElement('div');
			probe.style.color = raw;
			document.body.appendChild(probe);
			const resolved = getComputedStyle(probe).color;
			probe.remove();
			// Composite over the *pane's* background (what the ring actually
			// paints on top of), not the button's own (transparent at rest).
			const paneEl = el.closest('[data-pane-id]') as HTMLElement;
			return [resolved, getComputedStyle(paneEl).backgroundColor];
		});
		const compositedFocusRing = compositeOver(focusRingColor, focusRingPaneBg);
		const focusRingContrast = contrastRatio(compositedFocusRing, focusRingPaneBg);

		// Active/pressed state: neither IconButton nor pane-toolbar.tsx defines
		// an `active:` pseudo-class background for these two buttons (only
		// `hover:` and `focus-visible:`), so a mouse-down doesn't paint a third
		// background — pressing while hovered keeps the already-measured hover
		// background, and pressing via keyboard (Enter/Space while
		// focus-visible) keeps the focus ring just measured. There is no
		// separate "active" visual to measure.
		const hasActiveClass = await moreButton.evaluate((el) =>
			Array.from(el.classList).some((c) => c.startsWith('active:'))
		);

		test.info().annotations.push({
			type: 'contrast-measurements',
			description: `pane-tools focus-visible ring ${focusRingColor} composited over pane bg ${focusRingPaneBg} → ${compositedFocusRing}, contrast ${focusRingContrast.toFixed(2)}:1. Active/pressed state: no active: class present on the button (${hasActiveClass}) — pressed visuals fall back to whichever of hover/focus-visible is already active, both already ≥3:1.`,
		});
		expect(focusRingContrast).toBeGreaterThanOrEqual(3);
		expect(hasActiveClass).toBe(false);
	});
});

/** Converts CSS Color 4 `oklab(L a b [/ alpha])` to 0–255 sRGB, gamma-encoded
 *  (the standard OKLab → linear-sRGB matrices, then the sRGB transfer
 *  function). Chrome serializes a `color-mix(in srgb, ...)` custom property
 *  back out as `oklab(...)` via `getComputedStyle` in some builds — it does
 *  NOT always downconvert to `rgb()`/`color(srgb ...)` the way a `color`
 *  (text-color) property does, which is what made the ring measurement's
 *  probe-element trick insufficient for a `background-color` read here. */
function oklabToSrgb255(L: number, a: number, b: number, alpha: number): [number, number, number, number] {
	const l_ = L + 0.3963377774 * a + 0.2158037573 * b;
	const m_ = L - 0.1055613458 * a - 0.0638541728 * b;
	const s_ = L - 0.0894841775 * a - 1.2914855480 * b;
	const l = l_ ** 3;
	const m = m_ ** 3;
	const s = s_ ** 3;
	const rl = 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s;
	const gl = -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s;
	const bl = -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s;
	const gamma = (c: number) => {
		const clamped = Math.min(1, Math.max(0, c));
		return clamped <= 0.0031308 ? 12.92 * clamped : 1.055 * clamped ** (1 / 2.4) - 0.055;
	};
	return [gamma(rl) * 255, gamma(gl) * 255, gamma(bl) * 255, alpha];
}

/** Parses `rgb()`/`rgba()` (0–255 channels), the `color(srgb r g b / a)`
 *  function (0–1 channels — what Chrome resolves a `color-mix()` custom
 *  property to via `getComputedStyle` for a text `color`), and `oklab(...)`
 *  (what it resolves to for some `background-color` `color-mix()`s) into
 *  0–255 `[r, g, b, a(0-1)]`. */
function parseRgba(c: string): [number, number, number, number] {
	const trimmed = c.trim();
	const m = trimmed.match(/-?[\d.]+/g) ?? ['0', '0', '0', '1'];
	if (trimmed.startsWith('oklab(')) {
		const [L, a, b, alpha = '1'] = m;
		return oklabToSrgb255(Number(L), Number(a), Number(b), Number(alpha));
	}
	const [r, g, b, a = '1'] = m;
	if (trimmed.startsWith('color(')) {
		return [Number(r) * 255, Number(g) * 255, Number(b) * 255, Number(a)];
	}
	return [Number(r), Number(g), Number(b), Number(a)];
}

/** Alpha-composites a possibly-translucent foreground color over an opaque
 *  background, returning an opaque `rgb(...)` string — what the pixel
 *  actually renders as. */
function compositeOver(fg: string, bg: string): string {
	const [fr, fg_, fb, fa] = parseRgba(fg);
	const [br, bgc, bb] = parseRgba(bg);
	const mix = (f: number, b: number) => Math.round(f * fa + b * (1 - fa));
	return `rgb(${mix(fr, br)}, ${mix(fg_, bgc)}, ${mix(fb, bb)})`;
}

/** Relative-luminance contrast ratio between two opaque CSS colors (rgb/rgba
 *  strings as `getComputedStyle` returns them, or from `compositeOver`). */
function contrastRatio(a: string, b: string): number {
	const lum = (c: string) => {
		const [r, g, bl] = parseRgba(c).slice(0, 3).map((v) => v / 255);
		const lin = (v: number) => (v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4);
		return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(bl);
	};
	const la = lum(a) + 0.05;
	const lb = lum(b) + 0.05;
	return la > lb ? la / lb : lb / la;
}
