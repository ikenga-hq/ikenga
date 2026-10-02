// "Pair a device" — D-05 `pair` (`designs/people.html?state=pair`), G-ACCESS
// §3.1–§3.3, §3.6 (step 4 copy), §3.7. WP-74b.
//
// A sheet over Devices: (1) remote access and (2) the perimeter, read-only
// (§15 N-6); (3) the code, its QR, the expiry and New code; (4) "Confirm it
// here". The QR encodes `<public base>/remote/pair#c=<code>` and is rendered
// client-side (`qrcode-generator`). With no reachable base — the daemon bound
// to loopback only — the QR is hidden with the D-14 copy and the code still
// works on a device that reaches the computer some other way.
//
// The confirm itself is `devices-pair-confirm.tsx`, full-window, driven by the
// same `access_pair_pending` poll this sheet reads for burned codes.

import { RefreshCw } from 'lucide-react';
import qrcode from 'qrcode-generator';
import { useCallback, useEffect, useMemo, useState } from 'react';

import { Button } from '@/components/ui/button';
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from '@/components/ui/dialog';
import { StatusChip } from '@/components/ui/status-chip';
import {
	accessPairBegin,
	accessPairCancel,
	type PairTicket,
	parseAccessError,
} from '@/lib/access/client';

import { type DevicesView, EXPOSURE_COPY, expiresIn, pairPublicBase } from './devices-model';
import { usePairWatch } from './devices-pair-confirm';

/** What the sheet shows in step 3. */
export type PairSheetPhase =
	| { kind: 'loading' }
	| { kind: 'code'; ticket: PairTicket }
	| { kind: 'burned'; ticket: PairTicket }
	| { kind: 'expired'; ticket: PairTicket }
	| { kind: 'error'; message: string; paused: boolean };

export function pairSheetPhase(
	ticket: PairTicket | null,
	error: { message: string; paused: boolean } | null,
	burnedIds: ReadonlySet<string>,
	now: number
): PairSheetPhase {
	if (error) return { kind: 'error', ...error };
	if (!ticket) return { kind: 'loading' };
	if (burnedIds.has(ticket.pairingId)) return { kind: 'burned', ticket };
	if (now >= ticket.expiresAt) return { kind: 'expired', ticket };
	return { kind: 'code', ticket };
}

/** The QR as an SVG of dark cells (no HTML injection). */
export function QrCode({ payload, size = 132 }: { payload: string; size?: number }) {
	const cells = useMemo(() => {
		const qr = qrcode(0, 'M');
		qr.addData(payload);
		qr.make();
		const n = qr.getModuleCount();
		const dark: [number, number][] = [];
		for (let r = 0; r < n; r++) for (let c = 0; c < n; c++) if (qr.isDark(r, c)) dark.push([r, c]);
		return { n, dark };
	}, [payload]);
	const pad = 2;
	const dim = cells.n + pad * 2;
	return (
		<svg
			role="img"
			aria-label="QR code for the pairing link"
			width={size}
			height={size}
			viewBox={`0 0 ${dim} ${dim}`}
			shapeRendering="crispEdges"
			className="rounded-md"
			style={{ background: '#fff' }}
		>
			{cells.dark.map(([r, c]) => (
				<rect key={`${r}-${c}`} x={c + pad} y={r + pad} width={1} height={1} fill="#000" />
			))}
		</svg>
	);
}

export function PairSheet({
	open,
	onOpenChange,
	view,
}: {
	open: boolean;
	onOpenChange: (open: boolean) => void;
	view: DevicesView;
}) {
	const [ticket, setTicket] = useState<PairTicket | null>(null);
	const [error, setError] = useState<{ message: string; paused: boolean } | null>(null);
	const [now, setNow] = useState(() => Date.now());
	const watch = usePairWatch((s) => s.watch);
	const unwatch = usePairWatch((s) => s.unwatch);
	const pending = usePairWatch((s) => s.pending);
	const revision = usePairWatch((s) => s.revision);
	const lastPaired = usePairWatch((s) => s.lastPaired);
	const [openedAt] = useState(revision);

	const begin = useCallback(async () => {
		setError(null);
		setTicket(null);
		try {
			const t = await accessPairBegin(pairPublicBase(view));
			setTicket(t);
			watch(t);
		} catch (e) {
			const { code, message } = parseAccessError(e);
			setError({
				paused: code === 'throttled',
				message:
					code === 'throttled'
						? 'Too many wrong codes — pairing paused for 15 min.'
						: code === 'store_unavailable'
							? "Pairing needs the background server (ikenga-server) with its data folder. It isn't available right now."
							: message,
			});
		}
	}, [view, watch]);

	// A code per opening; the old one dies when the sheet closes.
	// biome-ignore lint/correctness/useExhaustiveDependencies: once per opening, not per `begin` identity
	useEffect(() => {
		if (!open) return;
		void begin();
	}, [open]);

	useEffect(() => {
		if (!open) return;
		const t = setInterval(() => setNow(Date.now()), 1000);
		return () => clearInterval(t);
	}, [open]);

	// The device was paired from the confirm: close.
	useEffect(() => {
		if (open && revision !== openedAt && lastPaired) onOpenChange(false);
	}, [open, revision, openedAt, lastPaired, onOpenChange]);

	const close = (next: boolean) => {
		if (!next && ticket) {
			unwatch(ticket.pairingId);
			void accessPairCancel(ticket.pairingId).catch(() => {});
			setTicket(null);
		}
		onOpenChange(next);
	};

	const burnedIds = useMemo(
		() => new Set(pending.filter((p) => p.state === 'burned').map((p) => p.pairingId)),
		[pending]
	);
	const phase = pairSheetPhase(ticket, error, burnedIds, now);
	const exposure = EXPOSURE_COPY[view.exposure];
	const reachable = view.run === 'running' && view.address !== null;

	return (
		<Dialog open={open} onOpenChange={close}>
			<DialogContent
				data-state="pair"
				data-pair={phase.kind}
				className="max-h-[calc(100dvh-2rem)] overflow-y-auto border-[var(--border-strong)] bg-[var(--bg-surface)] p-0 text-[var(--fg)] sm:max-w-[640px]"
			>
				<DialogHeader className="border-b border-[var(--border-soft)] px-4 py-3">
					<DialogTitle style={{ fontFamily: 'var(--font-display)' }}>Pair a device</DialogTitle>
					<DialogDescription className="sr-only">
						Show a one-time code to another device, then confirm it here.
					</DialogDescription>
				</DialogHeader>

				<ol className="m-0 flex list-none flex-col gap-4 px-4 py-2">
					<Step n={1} title={reachable ? 'Remote access is on' : 'Remote access is off'}>
						{reachable
							? `The daemon serves this workspace at ${view.address}.`
							: 'Nothing is serving this workspace to other devices.'}
					</Step>
					<Step n={2} title="Perimeter">
						<span className="flex flex-wrap items-center gap-2">
							<StatusChip tone={exposure.tone}>{exposure.label}</StatusChip>
							<span>{exposure.note}</span>
						</span>
						<span className="mt-1 block text-[var(--fg-faint)]">
							Set by how the server was started.
						</span>
					</Step>
					<Step n={3} title="Open the address on the other device and enter this code">
						<span className="block">One device, one use. It expires on its own.</span>
						<PairCode phase={phase} now={now} onNewCode={() => void begin()} />
					</Step>
					<Step n={4} title="Confirm it here">
						This computer asks you to approve the request before the device gets anything. You pick
						what it may do at that moment; the default for a new device is{' '}
						<b className="text-[var(--fg)]">View + dispatch</b>. Check the four words match the
						phone.
					</Step>
				</ol>

				<div className="mx-4 mb-3 rounded-md border border-dashed border-[var(--border)] px-3 py-2 text-[var(--text-caption,12px)] text-[var(--fg-muted)]">
					Nothing is granted by the code alone. A code that expires, or a confirm you decline,
					leaves the device with no access at all.
				</div>

				<DialogFooter className="border-t border-[var(--border-soft)] px-4 py-3">
					<Button type="button" variant="outline" size="sm" onClick={() => close(false)}>
						Cancel
					</Button>
					<Button type="button" size="sm" disabled>
						{phase.kind === 'code' ? 'Waiting for the device…' : 'No code open'}
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}

function Step({ n, title, children }: { n: number; title: string; children: React.ReactNode }) {
	return (
		<li className="grid grid-cols-[24px_1fr] gap-3">
			<span className="grid h-6 w-6 place-items-center rounded-full border border-[var(--border)] font-mono text-[11px] text-[var(--fg-muted)]">
				{n}
			</span>
			<div className="min-w-0">
				<div className="text-[var(--text-body-sm)] font-semibold text-[var(--fg)]">{title}</div>
				<div className="mt-0.5 text-[var(--text-caption,12px)] leading-relaxed text-[var(--fg-muted)]">
					{children}
				</div>
			</div>
		</li>
	);
}

function PairCode({
	phase,
	now,
	onNewCode,
}: {
	phase: PairSheetPhase;
	now: number;
	onNewCode: () => void;
}) {
	if (phase.kind === 'loading') {
		return <span className="mt-2 block font-mono">Minting a code…</span>;
	}
	if (phase.kind === 'error') {
		return (
			<span
				className="mt-2 flex flex-wrap items-center gap-2"
				data-pair-error={phase.paused ? 'paused' : 'error'}
			>
				<span role="alert" className="text-[var(--danger)]">
					{phase.message}
				</span>
				{!phase.paused && <NewCode onClick={onNewCode} />}
			</span>
		);
	}
	const { ticket } = phase;
	const dead = phase.kind !== 'code';
	return (
		<span className="mt-2 flex flex-wrap items-center gap-4">
			<span
				data-pair-code
				className={`rounded-md border border-[var(--border-strong)] bg-[var(--bg-sunken)] px-4 py-2.5 font-mono text-[22px] tracking-[0.2em] text-[var(--fg)] ${dead ? 'line-through opacity-50' : ''}`}
			>
				{ticket.code}
			</span>
			{ticket.qrPayload && !dead ? (
				<QrCode payload={ticket.qrPayload} />
			) : (
				!ticket.qrPayload && (
					<span
						className="max-w-[260px] text-[var(--text-caption,12px)] text-[var(--fg-muted)]"
						data-qr="hidden"
					>
						This computer isn't reachable from other devices yet — see Remote access above. You can
						still type the code on a device that reaches it.
					</span>
				)
			)}
			<span className="flex flex-col items-start gap-2">
				{phase.kind === 'code' && (
					<span className="font-mono text-[var(--text-micro)] text-[var(--fg-muted)]">
						expires in <b className="text-[var(--fg)]">{expiresIn(ticket.expiresAt, now)}</b>
					</span>
				)}
				{phase.kind === 'burned' && (
					<span role="alert" className="text-[var(--danger)]">
						A device entered the wrong code — this code is dead.
					</span>
				)}
				{phase.kind === 'expired' && <span>This code expired.</span>}
				<NewCode onClick={onNewCode} />
			</span>
		</span>
	);
}

function NewCode({ onClick }: { onClick: () => void }) {
	return (
		<Button type="button" variant="outline" size="xs" onClick={onClick}>
			<RefreshCw /> New code
		</Button>
	);
}
