// `/remote/pair` — the device side of pairing (G-ACCESS §3.12, §3.1–§3.8;
// D-05 `pair` seen from the phone). WP-74b.
//
// A code entry (prefilled from the QR link's `#c=` fragment, which never
// reaches the server), a progress state, the 4 fingerprint words with "Check
// these words match the computer", and the outcomes: allowed → `/remote`;
// denied, expired or burned → the matching message.
//
// Public: rendered before any credential exists. `boot/primary.tsx` mounts it
// standalone (no router, no RPC) when a browser opens `/remote/pair`; the
// route file mounts the same component for in-app navigation.

import { type FormEvent, useEffect, useMemo, useRef, useState } from 'react';

import type { Fingerprint } from '@/lib/access/fingerprint';
import { displayPairCode, normalizePairCode } from '@/lib/access/pair-code';
import { D05_FOCUS } from '@/shell/people/focus';
import {
	codeFromHash,
	deviceNameFromUA,
	hostFromHash,
	type PairOutcome,
	runPairing,
} from './pair-flow';

type Phase =
	| { kind: 'entry'; error?: string }
	| { kind: 'working' }
	| { kind: 'words'; words: Fingerprint }
	| { kind: 'done'; outcome: PairOutcome };

/** Copy for every non-allowed outcome (§3.12). */
export function outcomeCopy(o: PairOutcome): { title: string; body: string } {
	switch (o.kind) {
		case 'allowed':
			return { title: 'Paired', body: 'Opening your workspace…' };
		case 'cookie_rejected':
			return {
				title: "This browser didn't keep the pairing",
				body: "The computer allowed this device, but this connection isn't HTTPS, so the browser dropped the device credential. Pair over a Tailscale address, serve it over HTTPS, or start ikenga-server with --insecure-cookie, then remove this device on the computer and pair again.",
			};
		case 'auth_unavailable':
			return {
				title: "Paired, but the computer couldn't confirm it yet",
				body: "The computer allowed this device, but its sign-in check is temporarily unavailable, so it couldn't confirm this browser kept the device credential. Open your workspace to try again; if it keeps failing, restart ikenga-server on the computer.",
			};
		case 'denied':
			return {
				title: 'The computer said no',
				body: 'The request was denied on the computer. This device has no access. Ask for a new code if that was a mistake.',
			};
		case 'expired':
			return {
				title: 'That code expired',
				body: 'Codes last 10 minutes. Ask for a new one on the computer.',
			};
		case 'burned':
			return {
				title: 'That code is dead',
				body: 'A code works for one device, once. Ask for a new one on the computer.',
			};
		case 'cancelled':
			return {
				title: 'Pairing was cancelled',
				body: 'The computer cancelled this code or started a new one. Ask for a new code.',
			};
		case 'throttled':
			return {
				title: 'Too many tries',
				body: `Wait ${Math.ceil(o.retryAfterMs / 1000)} s and try again.`,
			};
		case 'unreachable':
			return {
				title: "Can't reach the computer",
				body: 'Check this device can open the address the computer showed, then try again.',
			};
		default:
			return {
				title: "That code didn't work",
				body: 'Ask for a new one on the computer.',
			};
	}
}

export function RemotePairPage({
	onPaired = () => window.location.assign('/remote'),
}: {
	onPaired?: () => void;
}) {
	const initial = useMemo(
		() => (typeof window === 'undefined' ? null : codeFromHash(window.location.hash)),
		[]
	);
	const pinnedStoreId = useMemo(
		() => (typeof window === 'undefined' ? null : hostFromHash(window.location.hash)),
		[]
	);
	const [code, setCode] = useState(initial ? displayPairCode(initial) : '');
	const [phase, setPhase] = useState<Phase>({ kind: 'entry' });
	const abort = useRef<AbortController | null>(null);
	const device = useMemo(
		() => deviceNameFromUA(typeof navigator === 'undefined' ? '' : navigator.userAgent),
		[]
	);

	useEffect(() => () => abort.current?.abort(), []);

	const start = async (e?: FormEvent) => {
		e?.preventDefault();
		const norm = normalizePairCode(code);
		if (!norm) {
			setPhase({ kind: 'entry', error: 'A code is 6 letters and numbers, like K7P-42Q.' });
			return;
		}
		abort.current?.abort();
		const ctl = new AbortController();
		abort.current = ctl;
		setPhase({ kind: 'working' });
		const outcome = await runPairing(norm, device, {
			signal: ctl.signal,
			onWords: (words) => setPhase({ kind: 'words', words }),
			// The QR's store id pins the host only for the code it came with.
			pinnedStoreId: initial && norm === initial ? pinnedStoreId : null,
		});
		if (ctl.signal.aborted) return;
		setPhase({ kind: 'done', outcome });
		if (outcome.kind === 'allowed') onPaired();
	};

	const card =
		'w-full max-w-[390px] overflow-hidden rounded-xl border border-[var(--border-strong)] bg-[var(--bg-surface)] text-[var(--fg)] shadow-2xl';

	return (
		<div
			data-state="remote-pair"
			data-phase={phase.kind === 'done' ? phase.outcome.kind : phase.kind}
			className={`${D05_FOCUS} grid min-h-dvh place-items-center bg-[var(--bg-base)] p-4`}
		>
			<div className={card}>
				<div className="border-b border-[var(--border-soft)] px-4 py-3">
					<h1
						className="m-0 text-[length:var(--text-h4)] font-semibold"
						style={{ fontFamily: 'var(--font-display)' }}
					>
						Pair this device
					</h1>
					<p className="m-0 mt-1 text-[length:var(--text-micro)] text-[var(--fg-muted)]">
						{device.name} · {typeof window === 'undefined' ? '' : window.location.host}
					</p>
				</div>

				{(phase.kind === 'entry' || phase.kind === 'working') && (
					<form onSubmit={start} className="flex flex-col gap-3 p-4" aria-label="Pairing code">
						<p className="m-0 text-[length:var(--text-body-sm)] leading-relaxed text-[var(--fg-muted)]">
							Enter the code the computer shows under{' '}
							<b className="text-[var(--fg)]">Pair a device</b>. Nothing is granted by the code
							alone — the computer confirms first.
						</p>
						<input
							aria-label="Code"
							value={code}
							onChange={(e) => setCode(e.target.value.toUpperCase())}
							placeholder="K7P-42Q"
							autoCapitalize="characters"
							autoComplete="one-time-code"
							spellCheck={false}
							maxLength={9}
							disabled={phase.kind === 'working'}
							className="rounded-md border border-[var(--border)] bg-[var(--bg-sunken)] px-3 py-2.5 text-center font-mono text-[22px] tracking-[0.3em] text-[var(--fg)] outline-none focus:border-[var(--primary)] focus:ring-2 focus:ring-[var(--primary-soft)]"
						/>
						{phase.kind === 'entry' && phase.error && (
							<div role="alert" className="text-[12px] text-[var(--danger)]">
								{phase.error}
							</div>
						)}
						<button
							type="submit"
							disabled={phase.kind === 'working'}
							className="rounded-md bg-[var(--primary)] px-5 py-2 font-semibold text-[length:var(--text-body-sm)] text-[var(--primary-fg)] hover:opacity-90 disabled:opacity-50"
						>
							{phase.kind === 'working' ? 'Checking the code…' : 'Pair'}
						</button>
					</form>
				)}

				{phase.kind === 'words' && (
					<div className="flex flex-col gap-3 p-4" data-pair="awaiting_host">
						<p className="m-0 text-[length:var(--text-body-sm)] text-[var(--fg-muted)]">
							Check these words match the computer:
						</p>
						<div
							data-fingerprint
							className="rounded-md border border-[var(--border)] bg-[var(--bg-sunken)] px-3 py-3 text-center font-mono text-[18px] text-[var(--fg)]"
						>
							{phase.words.join(' · ')}
						</div>
						<p className="m-0 text-[length:var(--text-micro)] leading-relaxed text-[var(--fg-muted)]">
							Waiting for the computer to approve this device. If the words differ, deny it there.
						</p>
					</div>
				)}

				{phase.kind === 'done' && (
					<div className="flex flex-col gap-3 p-4" role="status">
						<h2 className="m-0 text-[length:var(--text-body)] font-semibold">
							{outcomeCopy(phase.outcome).title}
						</h2>
						<p className="m-0 text-[length:var(--text-body-sm)] leading-relaxed text-[var(--fg-muted)]">
							{outcomeCopy(phase.outcome).body}
						</p>
						{phase.outcome.kind === 'auth_unavailable' && (
							<button
								type="button"
								onClick={() => onPaired()}
								className="rounded-md border border-[var(--border)] px-4 py-2 text-[length:var(--text-body-sm)] hover:bg-[var(--bg-hover,var(--bg-sunken))]"
							>
								Open your workspace
							</button>
						)}
						{phase.outcome.kind !== 'allowed' && phase.outcome.kind !== 'auth_unavailable' && (
							<button
								type="button"
								onClick={() => {
									setCode('');
									setPhase({ kind: 'entry' });
								}}
								className="rounded-md border border-[var(--border)] px-4 py-2 text-[length:var(--text-body-sm)] hover:bg-[var(--bg-hover,var(--bg-sunken))]"
							>
								Enter a new code
							</button>
						)}
					</div>
				)}
			</div>
		</div>
	);
}
