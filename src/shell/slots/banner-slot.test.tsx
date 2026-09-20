// WP-09 T4 — one banner slot: when three banners are eligible only one
// renders (priority violation > update > info), `+N more` cycles the rest, and
// every banner — PkgAutoUpdater included — stays mounted underneath.

import { act, cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const { flags, make } = vi.hoisted(() => {
	const flags = {
		eligible: new Set<string>(),
		mounted: new Set<string>(),
		listeners: new Set<() => void>(),
	};
	/** A fake banner: always mounted (records it), renders only when eligible,
	 *  and re-renders itself when the test flips eligibility — the way the
	 *  real banners flip on their own internal state. */
	function make(React: typeof import('react'), id: string) {
		return function Fake() {
			const [, force] = React.useReducer((n: number) => n + 1, 0);
			React.useEffect(() => {
				flags.mounted.add(id);
				flags.listeners.add(force);
				return () => {
					flags.mounted.delete(id);
					flags.listeners.delete(force);
				};
			}, []);
			return flags.eligible.has(id)
				? React.createElement('div', { role: 'status' }, `banner:${id}`)
				: null;
		};
	}
	return { flags, make };
});

vi.mock('@/shell/connection-banner', async () => ({
	ConnectionBanner: make(await import('react'), 'connection'),
}));
vi.mock('@/shell/trust-review-banner', async () => ({
	TrustReviewBanner: make(await import('react'), 'trust-review'),
}));
vi.mock('@/shell/updater-banner', async () => ({
	UpdaterBanner: make(await import('react'), 'updater'),
}));
vi.mock('@/shell/pkg-auto-updater', async () => ({
	PkgAutoUpdater: make(await import('react'), 'pkg-auto-updater'),
}));
vi.mock('@/shell/connector-banner', async () => ({
	ConnectorBanner: make(await import('react'), 'connector'),
}));

import { BANNER_QUEUE, BannerSlot, LEGACY_BANNER_MOUNT_ORDER } from './banner-slot';

/** Flip eligibility and let the slot's MutationObserver (a microtask) run. */
async function setEligible(ids: string[]) {
	await act(async () => {
		flags.eligible = new Set(ids);
		for (const f of flags.listeners) f();
	});
}

function visibleBanners(): string[] {
	return Array.from(document.querySelectorAll<HTMLElement>('[data-banner]'))
		.filter((el) => !el.hidden && el.childNodes.length > 0)
		.map((el) => el.dataset.banner ?? '');
}

beforeEach(() => {
	flags.eligible = new Set();
});
afterEach(cleanup);

describe('<BannerSlot />', () => {
	it('priority is violation > update > info, mount order kept within a tier', () => {
		expect(BANNER_QUEUE.map((b) => `${b.tier}:${b.id}`)).toEqual([
			'violation:connection',
			'violation:trust-review',
			'update:updater',
			'update:pkg-auto-updater',
			'info:connector',
		]);
		// Every banner that mounted before still mounts — nothing dropped.
		expect([...LEGACY_BANNER_MOUNT_ORDER].sort()).toEqual(BANNER_QUEUE.map((b) => b.id).sort());
	});

	it('T4: renders only one banner when three are eligible — the violation', async () => {
		render(<BannerSlot />);
		await setEligible(['connector', 'updater', 'trust-review']);

		expect(visibleBanners()).toEqual(['trust-review']);
		expect(screen.getAllByRole('status')).toHaveLength(1);
		expect(screen.getByText('banner:trust-review')).toBeTruthy();
		// Every banner is still mounted underneath (PkgAutoUpdater keeps its
		// background auto-update effect running while rendering nothing).
		expect([...flags.mounted].sort()).toEqual(BANNER_QUEUE.map((b) => b.id).sort());

		// `+N more` cycles the queue, still one at a time, in priority order.
		const user = userEvent.setup();
		const more = screen.getByRole('button', { name: /2 more notices/ });
		expect(more.textContent).toBe('+2 more');
		await user.click(more);
		expect(visibleBanners()).toEqual(['updater']);
		await user.click(more);
		expect(visibleBanners()).toEqual(['connector']);
		await user.click(more);
		expect(visibleBanners()).toEqual(['trust-review']);
	});

	it('update outranks info, and a lone banner shows no `+N more`', async () => {
		render(<BannerSlot />);
		await setEligible(['connector', 'pkg-auto-updater']);
		expect(visibleBanners()).toEqual(['pkg-auto-updater']);
		await setEligible(['connector']);
		expect(visibleBanners()).toEqual(['connector']);
		expect(screen.queryByRole('button', { name: /more notice/ })).toBeNull();
		await setEligible([]);
		expect(visibleBanners()).toEqual([]);
	});

	it('snaps back to the top notice when a higher-priority banner appears', async () => {
		render(<BannerSlot />);
		await setEligible(['updater', 'connector']);
		const user = userEvent.setup();
		await user.click(screen.getByRole('button', { name: /1 more notice/ }));
		expect(visibleBanners()).toEqual(['connector']);
		await setEligible(['connection', 'updater', 'connector']);
		expect(visibleBanners()).toEqual(['connection']);
	});
});
