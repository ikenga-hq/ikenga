// Host confirm — D-05 `pair-confirm` (`designs/people.html?state=pair-confirm`),
// G-ACCESS §3.6 + the DEC-77 fingerprint row (§3.5, D-8). WP-74b.
//
// Mounted once at the boot root beside `AppLockOverlay` (`boot/primary.tsx`).
// While at least one pairing session this window began is open, it polls
// `access_pair_pending` every 2 s (§3.6: no new event channel); when a request
// reaches `awaiting_host`, the full-window confirm mounts. The pair sheet
// (`devices-pair-sheet.tsx`) reads the same poll for its burned-code state.
//
// Deny burns the session and leaves no device row; Pair device issues the
// credential (§3.8). Both are audited by the daemon (`pair.denied`,
// `pair.allowed`). `full` is never offered here (P-9). Each outcome ends in
// D-05's toast ("Paired <name> · <tier>", "Denied · the code is now dead").
//
// The pair sheet hands its code over to this confirm the moment the request
// reaches `awaiting_host` (it closes without cancelling, D-05 `closeOverlays()
// → pair-confirm`), so no modal dialog sits over it.

import { CheckCircle2, XCircle } from 'lucide-react';
import { type KeyboardEvent, useEffect, useId, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { create } from 'zustand';

import { FloatingToastChip } from '@/components/ui/floating-toast-chip';
import { Segmented } from '@/components/ui/segmented';
import type { Tier } from '@/lib/access/caps.gen';
import { TIER_LABELS } from '@/lib/access/caps.gen';
import {
	accessPairDecide,
	accessPairPending,
	type DeviceView,
	type PairRequest,
	type PairTicket,
	parseAccessError,
} from '@/lib/access/client';

import { DEFAULT_PAIR_TIER, PAIR_TIERS, relativeTime } from './devices-model';
import { D05_DANGER, D05_FOCUS } from './focus';

const POLL_MS = 2000;

interface PairWatchState {
	/** Open tickets this window began, by pairing id → expiry (ms). */
	watching: Record<string, number>;
	/** The latest `access_pair_pending` answer. */
	pending: PairRequest[];
	/** Bumped when a device is paired, so the table refreshes. */
	revision: number;
	lastPaired: DeviceView | null;
	/** The post-decision toast (D-05), or null. */
	notice: { tone: 'ok' | 'denied'; text: string } | null;
	watch: (t: PairTicket) => void;
	unwatch: (pairingId: string) => void;
	setPending: (rows: PairRequest[]) => void;
	paired: (pairingId: string, device: DeviceView | null) => void;
	setNotice: (notice: PairWatchState['notice']) => void;
}

export const usePairWatch = create<PairWatchState>((set) => ({
	watching: {},
	pending: [],
	revision: 0,
	lastPaired: null,
	notice: null,
	setNotice: (notice) => set({ notice }),
	watch: (t) => set((s) => ({ watching: { ...s.watching, [t.pairingId]: t.expiresAt } })),
	unwatch: (id) =>
		set((s) => {
			const { [id]: _gone, ...rest } = s.watching;
			return { watching: rest, pending: s.pending.filter((p) => p.pairingId !== id) };
		}),
	setPending: (rows) => set({ pending: rows }),
	paired: (id, device) =>
		set((s) => {
			const { [id]: _gone, ...rest } = s.watching;
			return {
				watching: rest,
				pending: s.pending.filter((p) => p.pairingId !== id),
				revision: s.revision + 1,
				lastPaired: device,
			};
		}),
}));

/** Poll while anything this window began is still open (§3.6). */
function usePendingPoll() {
	const watching = usePairWatch((s) => s.watching);
	const setPending = usePairWatch((s) => s.setPending);
	const open = Object.values(watching).some((exp) => exp > Date.now());
	useEffect(() => {
		if (!open) return;
		let cancelled = false;
		const tick = async () => {
			try {
				const rows = await accessPairPending();
				if (!cancelled) setPending(rows);
			} catch {
				// A daemon that idled out or restarted: the next tick retries;
				// the sheet shows the code's own expiry meanwhile.
			}
		};
		void tick();
		const t = setInterval(() => void tick(), POLL_MS);
		return () => {
			cancelled = true;
			clearInterval(t);
		};
	}, [open, setPending]);
}

/** Mount once at the root of the primary window. */
export function PairConfirmOverlay() {
	usePendingPoll();
	const pending = usePairWatch((s) => s.pending);
	const watching = usePairWatch((s) => s.watching);
	const notice = usePairWatch((s) => s.notice);
	const setNotice = usePairWatch((s) => s.setNotice);
	const request = pending.find((p) => p.state === 'awaiting_host' && watching[p.pairingId]);
	if (typeof document === 'undefined') return null;
	return (
		<>
			{request &&
				createPortal(<PairConfirm key={request.pairingId} request={request} />, document.body)}
			{notice && (
				<div data-pair-toast={notice.tone}>
					<FloatingToastChip
						variant={notice.tone === 'ok' ? 'notice' : 'info'}
						anchor="viewport-bottom-right"
						icon={notice.tone === 'ok' ? <CheckCircle2 /> : <XCircle />}
						label={notice.text}
						ttlMs={4000}
						onDismiss={() => setNotice(null)}
					/>
				</div>
			)}
		</>
	);
}

export function PairConfirm({
	request,
	now: nowProp,
}: {
	request: PairRequest;
	/** Fixed clock for tests. */
	now?: number;
}) {
	const [tier, setTier] = useState<Exclude<Tier, 'full'>>(DEFAULT_PAIR_TIER);
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const [now, setNow] = useState(() => nowProp ?? Date.now());
	const paired = usePairWatch((s) => s.paired);
	const unwatch = usePairWatch((s) => s.unwatch);
	const setNotice = usePairWatch((s) => s.setNotice);
	const panel = useRef<HTMLDivElement>(null);
	const ids = { code: useId(), words: useId() };

	// Take focus on mount (a security decision; nothing behind it should
	// keep the keyboard). The panel, not a button, so a stray Enter decides
	// nothing. The pair sheet is still closing when this mounts, and while it
	// is open its focus scope pulls focus back into itself; so keep claiming
	// for a few frames until focus holds here (WP-74b review R1).
	useEffect(() => {
		let raf = 0;
		let held = 0;
		const deadline = performance.now() + 1500;
		const claim = () => {
			const el = panel.current;
			if (!el) return;
			if (el.contains(document.activeElement)) held++;
			else {
				held = 0;
				el.focus();
			}
			if (held < 3 && performance.now() < deadline) raf = requestAnimationFrame(claim);
		};
		claim();
		return () => cancelAnimationFrame(raf);
	}, []);

	// Modal: Tab and Shift+Tab cycle inside the confirm, never into the frame.
	const trapTab = (e: KeyboardEvent<HTMLDivElement>) => {
		if (e.key !== 'Tab' || !panel.current) return;
		// Tabbable only: the tier tabs rove, so only the chosen one counts.
		const items = Array.from(
			panel.current.querySelectorAll<HTMLElement>(
				'button:not(:disabled), [href], input, [tabindex]'
			)
		).filter((el) => el.tabIndex >= 0);
		if (items.length === 0) return;
		const first = items[0];
		const last = items[items.length - 1];
		const active = document.activeElement;
		if (e.shiftKey && (active === first || active === panel.current)) {
			e.preventDefault();
			last.focus();
		} else if (!e.shiftKey && active === last) {
			e.preventDefault();
			first.focus();
		}
	};

	useEffect(() => {
		if (nowProp !== undefined) return;
		const t = setInterval(() => setNow(Date.now()), 1000);
		return () => clearInterval(t);
	}, [nowProp]);

	const decide = async (decision: 'allow' | 'deny') => {
		setBusy(true);
		setError(null);
		try {
			const res = await accessPairDecide(
				request.pairingId,
				decision,
				decision === 'allow' ? tier : undefined
			);
			if (decision === 'allow') {
				paired(request.pairingId, res.device ?? null);
				setNotice({
					tone: 'ok',
					text: `Paired ${res.device?.name ?? request.deviceName} · ${TIER_LABELS[tier].label}`,
				});
			} else {
				unwatch(request.pairingId);
				setNotice({ tone: 'denied', text: 'Denied · the code is now dead' });
			}
		} catch (e) {
			const { code, message } = parseAccessError(e);
			setError(
				code === 'expired' || code === 'gone'
					? 'This request is no longer waiting — the code expired or was replaced.'
					: message
			);
			if (code === 'expired' || code === 'gone' || code === 'not_found') unwatch(request.pairingId);
		} finally {
			setBusy(false);
		}
	};

	// D-05 `.drow`: a 28px row on a soft hairline; key `.k2` at micro, value at
	// caption, a `.val.mono` value at micro mono (the code keeps body size).
	const row =
		'grid min-h-[28px] grid-cols-[132px_1fr_auto] items-center gap-2 border-b border-[var(--border-soft)] py-1 text-[length:var(--text-caption,12px)]';
	const k = 'text-[length:var(--text-micro)] text-[var(--fg-muted)]';
	const v = 'min-w-0 text-[var(--fg)]';
	const mono = 'font-mono text-[length:var(--text-micro)]';

	return (
		<div
			data-state="pair-confirm"
			className={`${D05_FOCUS} pointer-events-auto fixed inset-0 z-[60] grid place-items-center bg-[color-mix(in_srgb,var(--bg-base)_82%,transparent)] p-6 backdrop-blur-xs`}
		>
			<div
				ref={panel}
				tabIndex={-1}
				role="dialog"
				aria-modal="true"
				aria-labelledby="pair-confirm-title"
				// The code and the four words are what the person checks: read
				// them out with the dialog (D-8).
				aria-describedby={`${ids.code} ${ids.words}`}
				data-pair-panel
				onKeyDown={trapTab}
				className="w-full max-w-[560px] overflow-hidden rounded-xl border border-[var(--border-strong)] bg-[var(--bg-surface)] text-[var(--fg)] shadow-2xl outline-none"
			>
				<div className="flex items-center gap-2 border-b border-[var(--border-soft)] px-4 py-3">
					<h2
						id="pair-confirm-title"
						className="m-0 text-[length:var(--text-h3)] font-semibold"
						style={{ fontFamily: 'var(--font-display)' }}
					>
						A device wants to pair
					</h2>
					<NewChip />
				</div>
				<div className="px-4 py-3">
					<div className={row}>
						<span className={k}>Device</span>
						<span className={v}>{request.deviceName}</span>
					</div>
					<div className={row}>
						<span className={k}>Address</span>
						<span className={`${v} ${mono}`}>{request.remoteAddr}</span>
					</div>
					<div className={row}>
						<span className={k}>Asked</span>
						<span className={v}>{relativeTime(request.askedAt, now)}</span>
					</div>
					<div className={row} id={ids.code}>
						<span className={k}>Code it typed</span>
						<span className={`${v} font-mono text-[length:var(--text-body)] tracking-[0.14em]`}>
							{request.code}
						</span>
						<span className="font-mono text-[length:var(--text-micro)] text-[var(--fg-muted)]">
							check this matches the phone
						</span>
					</div>
					<div className={row} data-row="fingerprint" id={ids.words}>
						<span className={k}>Words on the phone</span>
						<span className={`${v} ${mono}`}>{request.fingerprint.join(' · ')}</span>
					</div>

					<h3 className="m-0 mt-3 text-[length:var(--text-micro)] font-semibold uppercase tracking-[0.1em] text-[var(--fg-muted)]">
						What it may do
					</h3>
					<div className="mt-2">
						<Segmented
							ariaLabel="What it may do"
							value={tier}
							onValueChange={(id) => setTier(id as Exclude<Tier, 'full'>)}
							items={PAIR_TIERS.map((t) => ({ id: t, label: TIER_LABELS[t].label }))}
						/>
					</div>
					<p className="m-0 mt-2 text-[length:var(--text-caption,12px)] text-[var(--fg-muted)]">
						{TIER_LABELS[tier].long}
					</p>

					<div className="mt-3 rounded-md border border-dashed border-[var(--border)] px-3 py-2 text-[length:var(--text-caption,12px)] leading-relaxed text-[var(--fg-muted)]">
						If you did not just type this code into a phone,{' '}
						<b className="text-[var(--fg)]">deny</b>. A denied request leaves the device with
						nothing and burns the code.
					</div>
					{error && (
						<p role="alert" className="m-0 mt-2 text-[12px] text-[var(--danger)]">
							{error}
						</p>
					)}
				</div>
				<div className="flex items-center justify-end gap-2 border-t border-[var(--border-soft)] px-4 py-3">
					<button
						type="button"
						disabled={busy}
						onClick={() => void decide('deny')}
						className={`${D05_DANGER} h-[var(--btn-h-sm,26px)] rounded-[var(--radius-sm)] border px-3 text-[length:var(--text-micro)] font-medium disabled:opacity-50`}
					>
						Deny
					</button>
					<button
						type="button"
						disabled={busy}
						onClick={() => void decide('allow')}
						className="h-[var(--btn-h-sm,26px)] rounded-[var(--radius-sm)] border border-[var(--primary)] bg-[var(--primary)] px-3 text-[length:var(--text-micro)] font-medium text-[var(--primary-fg)] hover:opacity-90 disabled:opacity-50"
					>
						Pair device
					</button>
				</div>
			</div>
		</div>
	);
}

/** D-05's `newchip` (proposed — not in the shipped shell). */
export function NewChip() {
	return (
		<span
			className="newchip inline-flex h-4 flex-none items-center rounded-full bg-[var(--achievement-soft)] px-1.5 font-mono text-[10px] font-medium uppercase tracking-[0.08em] text-[var(--on-achievement,var(--achievement))]"
			title="Proposed — nothing here exists in the shipped shell"
		>
			new
		</span>
	);
}
