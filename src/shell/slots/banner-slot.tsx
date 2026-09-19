// Banner slot (WP-20 skeleton, filled by WP-09) — the ONE banner slot.
//
// Spec §6A.8: one banner at a time, priority violation > update > info
// (v4 P8: a deeper queue collapses into a `+N more` chip that cycles).
//
// Mount order before WP-09 (WP-20's verbatim extraction of workspace.tsx):
//   1. ConnectionBanner  2. UpdaterBanner  3. PkgAutoUpdater
//   4. ConnectorBanner   5. TrustReviewBanner
// (spec §3.14's "connection → trust review → updater → connector" was already
// stale against the code.) Every one of them stacked; all five could show.
//
// Priority order now (tier, then original mount order within a tier):
//   violation — ConnectionBanner (a blocking condition: the host is gone),
//               TrustReviewBanner (capability review — P10 folds trust into
//               violations)
//   update    — UpdaterBanner (app binary), PkgAutoUpdater (pkg progress /
//               failure / success strip)
//   info      — ConnectorBanner
//
// The banner components are untouched — their own eligibility logic, dismiss
// / snooze persistence keys and updater wiring stay exactly as they were. Each
// is mounted in its own wrapper, always (so PkgAutoUpdater's background
// auto-update effect and every banner's polling keep running); a wrapper is
// "eligible" when its component rendered anything, and all but the chosen
// one are `hidden`. Eligibility is read from the DOM (a MutationObserver over
// the slot) because a banner's visibility is decided inside it, from state
// the slot cannot see without duplicating each banner's logic.

import { useCallback, useLayoutEffect, useRef, useState } from 'react';
import type { ComponentType } from 'react';
import { ConnectionBanner } from '@/shell/connection-banner';
import { ConnectorBanner } from '@/shell/connector-banner';
import { PkgAutoUpdater } from '@/shell/pkg-auto-updater';
import { TrustReviewBanner } from '@/shell/trust-review-banner';
import { UpdaterBanner } from '@/shell/updater-banner';

export type BannerTier = 'violation' | 'update' | 'info';

export interface BannerQueueEntry {
	id: string;
	tier: BannerTier;
	Component: ComponentType;
}

/** Priority order — first eligible wins. */
export const BANNER_QUEUE: BannerQueueEntry[] = [
	{ id: 'connection', tier: 'violation', Component: ConnectionBanner },
	{ id: 'trust-review', tier: 'violation', Component: TrustReviewBanner },
	{ id: 'updater', tier: 'update', Component: UpdaterBanner },
	{ id: 'pkg-auto-updater', tier: 'update', Component: PkgAutoUpdater },
	{ id: 'connector', tier: 'info', Component: ConnectorBanner },
];

/** Mount order before WP-09, recorded for the PR (DoD T4). */
export const LEGACY_BANNER_MOUNT_ORDER = [
	'connection',
	'updater',
	'pkg-auto-updater',
	'connector',
	'trust-review',
] as const;

function sameList(a: string[], b: string[]): boolean {
	return a.length === b.length && a.every((v, i) => v === b[i]);
}

export function BannerSlot() {
	const ref = useRef<HTMLDivElement | null>(null);
	const [eligible, setEligible] = useState<string[]>([]);
	const eligibleRef = useRef<string[]>([]);
	// Which eligible banner the `+N more` chip has cycled to (0 = top priority).
	const [offset, setOffset] = useState(0);

	const measure = useCallback(() => {
		const root = ref.current;
		if (!root) return;
		const ids = Array.from(root.querySelectorAll<HTMLElement>('[data-banner]'))
			.filter((el) => el.childNodes.length > 0)
			.map((el) => el.dataset.banner ?? '');
		if (sameList(eligibleRef.current, ids)) return;
		eligibleRef.current = ids;
		setEligible(ids);
		// The queue changed: snap back to the top-priority notice so a newly
		// raised violation is never left behind a cycled-to info banner.
		setOffset(0);
	}, []);

	// Before paint on every render, and whenever a banner re-renders itself.
	useLayoutEffect(() => {
		measure();
	});
	useLayoutEffect(() => {
		const root = ref.current;
		if (!root || typeof MutationObserver === 'undefined') return;
		const obs = new MutationObserver(() => measure());
		obs.observe(root, { childList: true, subtree: true });
		return () => obs.disconnect();
	}, [measure]);

	const shown = eligible.length > 0 ? eligible[offset % eligible.length] : null;
	const more = eligible.length - 1;

	return (
		<div ref={ref} data-testid="banner-slot" className="flex flex-none items-stretch">
			<div data-banner-stack className="min-w-0 flex-1">
				{BANNER_QUEUE.map(({ id, tier, Component }) => (
					<div key={id} data-banner={id} data-banner-tier={tier} hidden={id !== shown}>
						<Component />
					</div>
				))}
			</div>
			{more > 0 && (
				<button
					type="button"
					onClick={() => setOffset((o) => (o + 1) % eligible.length)}
					aria-label={`${more} more notice${more === 1 ? '' : 's'} — show next`}
					className="flex-none border-b border-l border-border bg-[var(--bg-surface)] px-3 text-[length:var(--text-micro)] font-medium text-muted-foreground outline-none transition-colors duration-[var(--motion-fast)] ease-[var(--ease-calm)] motion-reduce:transition-none hover:bg-[var(--bg-raised)] hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
				>
					+{more} more
				</button>
			)}
		</div>
	);
}
