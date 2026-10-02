import { type FormEvent, lazy, Suspense, useEffect, useState } from 'react';
import { useReauthStore } from '@/lib/transport/reauth-store';
import { isT1Session } from '@/lib/transport/t1-session';
import { D05_FOCUS } from '@/shell/people/focus';
import { ShareModeBanner } from '@/shell/people/shared-with-you';
import { T1SignInForm } from './t1-sign-in-form';

// WP-76 (G-ACCESS §7.3): a signed-out browser on a T1 server boots straight
// into this overlay, so an invite link (`/remote/invite#t=…`) is answered
// here — the invitee may have no account yet.
const InvitePage = lazy(() =>
	import('@/routes/remote/invite').then((m) => ({ default: m.InvitePage }))
);

/** Whether this tab was opened on an invite link. */
function onInvitePath(): boolean {
	if (typeof window === 'undefined') return false;
	const p = window.location.pathname;
	return p === '/remote/invite' || p.startsWith('/remote/invite/');
}

/**
 * "Pair this device" (G-ACCESS §3.12, WP-74b): beside the T0 token and WP-20's
 * T1 password modes. A browser with no credential at all, or a paired device
 * whose grant died, gets a code field; pairing itself runs at `/remote/pair`
 * (the code travels in the `#c=` fragment, never to the server's logs).
 */
function PairThisDevice() {
	const errorMsg = useReauthStore((s) => s.errorMsg);
	const pairWithCode = useReauthStore((s) => s.pairWithCode);
	const setMode = useReauthStore((s) => s.setMode);
	const [code, setCode] = useState('');
	const host = typeof window !== 'undefined' ? window.location.host : '';
	const submit = (e: FormEvent) => {
		e.preventDefault();
		pairWithCode(code);
	};
	return (
		<div
			data-state="reauth-pair"
			className={`${D05_FOCUS} fixed inset-0 z-50 grid place-items-center bg-[color-mix(in_srgb,var(--bg-base)_78%,transparent)] p-6 backdrop-blur-xs`}
		>
			<form
				onSubmit={submit}
				aria-label="Pair this device"
				className="w-full max-w-[400px] overflow-hidden rounded-xl border border-[var(--border-strong)] bg-[var(--bg-surface)] text-[var(--fg)] shadow-2xl"
			>
				<div className="flex items-center gap-2.5 border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-5 py-4">
					<span className="h-2 w-2 flex-none rounded-full bg-[var(--primary)]" />
					<h2 className="m-0 text-[length:var(--text-h4)] font-semibold">Pair this device</h2>
				</div>
				<div className="flex flex-col gap-3 p-5">
					<p className="m-0 text-[length:var(--text-body-sm)] leading-relaxed text-[var(--fg-muted)]">
						On the computer that runs {host || 'Ikenga'}, open Settings › Devices › Pair a device
						and type the code it shows. The computer confirms before this device gets anything.
					</p>
					<input
						aria-label="Pairing code"
						value={code}
						onChange={(e) => setCode(e.target.value.toUpperCase())}
						placeholder="K7P-42Q"
						autoCapitalize="characters"
						autoComplete="one-time-code"
						spellCheck={false}
						maxLength={9}
						className="rounded-md border border-[var(--border)] bg-[var(--bg-sunken)] px-3 py-2 text-center font-mono text-[18px] tracking-[0.25em] text-[var(--fg)] outline-none focus:border-[var(--primary)] focus:ring-2 focus:ring-[var(--primary-soft)]"
					/>
					<button
						type="submit"
						className="rounded-md bg-[var(--primary)] px-5 py-2 font-semibold text-[length:var(--text-body-sm)] text-[var(--primary-fg)] hover:opacity-90 cursor-pointer"
					>
						Pair with this code
					</button>
					{errorMsg && (
						<div role="alert" className="font-mono text-[12px] text-[var(--danger)]">
							{errorMsg}
						</div>
					)}
					<button
						type="button"
						onClick={() => setMode('auto')}
						className="text-[length:var(--text-micro)] text-[var(--fg-muted)] underline-offset-2 hover:underline cursor-pointer"
					>
						{isT1Session() ? 'Sign in with a password instead' : 'I have a token instead'}
					</button>
				</div>
			</form>
		</div>
	);
}

/** The way into pair mode from the T0 token mode. (Under T1 the restyled
 *  sign-in carries it with equal weight, WP-76.) */
function PairModeLink() {
	const setMode = useReauthStore((s) => s.setMode);
	return (
		<button
			type="button"
			onClick={() => setMode('pair')}
			className="mt-3 text-[length:var(--text-micro)] text-[var(--fg-muted)] underline-offset-2 hover:underline cursor-pointer"
		>
			Pair this device with a code
		</button>
	);
}

/**
 * The session chrome every browser boot root renders: the share-mode strip
 * (G-ACCESS §4.5.2, WP-76) and, when a credential is needed, the
 * re-authentication overlay.
 */
export function ReauthOverlay() {
	return (
		<>
			<ShareModeBanner />
			<ReauthDialog />
		</>
	);
}

function ReauthDialog() {
	const isOpen = useReauthStore((s) => s.isOpen);
	const mode = useReauthStore((s) => s.mode);
	const tokenInput = useReauthStore((s) => s.tokenInput);
	const setTokenInput = useReauthStore((s) => s.setTokenInput);
	const errorMsg = useReauthStore((s) => s.errorMsg);
	const reconnect = useReauthStore((s) => s.reconnect);

	const [timeStr, setTimeStr] = useState<string>('');
	const [loading, setLoading] = useState<boolean>(false);

	// Gated on `isOpen`. Hooks run before the `!isOpen` early return below, so
	// an ungated interval ticks and re-renders this component once a second
	// for the entire lifetime of every window, overlay shown or not.
	useEffect(() => {
		if (!isOpen) return;
		const updateTime = () => {
			const d = new Date();
			setTimeStr(d.toTimeString().split(' ')[0] || '');
		};
		updateTime();
		const interval = setInterval(updateTime, 1000);
		return () => clearInterval(interval);
	}, [isOpen]);

	if (!isOpen) return null;
	if (mode === 'pair') return <PairThisDevice />;
	// T1: there is no token to paste; principals sign in (G-PRINCIPAL §2.4),
	// or accept an invite (§7.3) — D-05 `sign-in`, restyled (WP-76).
	if (isT1Session()) {
		if (onInvitePath()) {
			return (
				<Suspense fallback={null}>
					<InvitePage />
				</Suspense>
			);
		}
		return <T1SignInForm />;
	}

	const handleReconnect = async () => {
		setLoading(true);
		try {
			await reconnect(tokenInput);
		} finally {
			setLoading(false);
		}
	};

	return (
		<div className="fixed inset-0 z-50 grid place-items-center bg-[color-mix(in_srgb,var(--bg-base)_78%,transparent)] p-6 backdrop-blur-xs">
			<div className="w-full max-w-[460px] overflow-hidden rounded-xl border border-[var(--border-strong)] bg-[var(--bg-surface)] text-[var(--fg)] shadow-2xl">
				{/* Top bar */}
				<div className="flex items-center gap-2.5 border-b border-[var(--border-soft)] bg-[var(--danger-soft)] px-5 py-4">
					<span className="h-2 w-2 flex-none rounded-full bg-[var(--danger)]" />
					<h2 className="m-0 text-[length:var(--text-h4)] font-semibold">
						Session needs re-authenticating
					</h2>
					<span className="ml-auto font-mono text-[length:var(--text-micro)] text-[var(--fg-faint)]">
						{timeStr}
					</span>
				</div>

				{/* Body */}
				<div className="p-5">
					<p className="mb-4 text-[length:var(--text-body-sm)] text-[var(--fg-muted)] leading-relaxed">
						The daemon restarted and minted a new token, so this tab's saved one no longer works.
						Your work is untouched — paste the current token to pick it back up.
					</p>

					<div className="flex gap-2">
						<input
							type="password"
							value={tokenInput}
							onChange={(e) => setTokenInput(e.target.value)}
							placeholder="Paste auth token..."
							className="flex-1 rounded-md border border-[var(--border)] bg-[var(--bg-sunken)] px-3 py-2 font-mono text-[length:var(--text-body-sm)] text-[var(--fg)] outline-none focus:border-[var(--primary)] focus:ring-2 focus:ring-[var(--primary-soft)]"
							autoFocus
							spellCheck={false}
							onKeyDown={(e) => {
								if (e.key === 'Enter') {
									e.preventDefault();
									handleReconnect();
								}
							}}
						/>
						<button
							type="button"
							onClick={handleReconnect}
							disabled={loading}
							className="rounded-md bg-[var(--primary)] px-5 py-2 font-semibold text-[length:var(--text-body-sm)] text-[var(--primary-fg)] hover:opacity-90 disabled:opacity-50 cursor-pointer"
						>
							{loading ? 'Connecting...' : 'Reconnect'}
						</button>
					</div>

					{errorMsg && (
						<div className="mt-3 font-mono text-[12px] text-[var(--danger)]">{errorMsg}</div>
					)}

					<PairModeLink />

					<div className="mt-4 border-t border-[var(--border-soft)] pt-4 text-[length:var(--text-micro)] text-[var(--fg-faint)] leading-relaxed">
						<b className="text-[var(--live)] font-semibold">Still running on the host</b> — session
						active. Nothing is lost by reconnecting.
					</div>
				</div>
			</div>
		</div>
	);
}
