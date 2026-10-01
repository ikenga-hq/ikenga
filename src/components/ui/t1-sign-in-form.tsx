// The T1 sign-in dialog (G-PRINCIPAL §2.4, OD-12): WP-20's minimal
// username/password form. `ReauthOverlay` renders it in place of the token
// field when the server reports tier t1. WP-76 restyles it to the D-05
// `sign-in` state (G-ACCESS §2.2, R-7); keep the logic here and the look
// replaceable.

import { type FormEvent, useState } from 'react';
import { useReauthStore } from '@/lib/transport/reauth-store';
import { currentPrincipal } from '@/lib/transport/t1-session';

export function T1SignInForm() {
	const errorMsg = useReauthStore((s) => s.errorMsg);
	const signIn = useReauthStore((s) => s.signIn);
	// A session that ended mid-use (expired, signed out elsewhere, password
	// changed) still knows who it was: prefill that name.
	const previous = currentPrincipal();
	const [username, setUsername] = useState(previous?.username ?? '');
	const [password, setPassword] = useState('');
	const [loading, setLoading] = useState(false);
	const host = typeof window !== 'undefined' ? window.location.host : '';

	const submit = async (e: FormEvent) => {
		e.preventDefault();
		if (loading) return;
		setLoading(true);
		try {
			const ok = await signIn(username, password);
			if (!ok) setPassword('');
		} finally {
			setLoading(false);
		}
	};

	const field =
		'w-full rounded-md border border-[var(--border)] bg-[var(--bg-sunken)] px-3 py-2 text-[var(--text-body-sm)] text-[var(--fg)] outline-none focus:border-[var(--primary)] focus:ring-2 focus:ring-[var(--primary-soft)]';

	return (
		<div className="fixed inset-0 z-50 grid place-items-center bg-[color-mix(in_srgb,var(--bg-base)_78%,transparent)] p-6 backdrop-blur-xs">
			<form
				onSubmit={submit}
				aria-label="Sign in"
				className="w-full max-w-[400px] overflow-hidden rounded-xl border border-[var(--border-strong)] bg-[var(--bg-surface)] text-[var(--fg)] shadow-2xl"
			>
				<div className="flex items-center gap-2.5 border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-5 py-4">
					<span className="h-2 w-2 flex-none rounded-full bg-[var(--primary)]" />
					<h2 className="m-0 text-[var(--text-h4)] font-semibold">
						{previous ? 'Sign in again' : 'Sign in'}
					</h2>
				</div>
				<div className="flex flex-col gap-3 p-5">
					<p className="m-0 text-[var(--text-body-sm)] text-[var(--fg-muted)] leading-relaxed">
						{previous
							? 'Your session ended. Your work is untouched — sign in to pick it back up.'
							: `Sign in to ${host}. Your files, sessions and secrets stay on this server.`}
					</p>
					<label className="flex flex-col gap-1 text-[var(--text-micro)] text-[var(--fg-muted)]">
						Username
						<input
							name="username"
							autoComplete="username"
							value={username}
							onChange={(e) => setUsername(e.target.value)}
							className={field}
							spellCheck={false}
							autoCapitalize="none"
						/>
					</label>
					<label className="flex flex-col gap-1 text-[var(--text-micro)] text-[var(--fg-muted)]">
						Password
						<input
							name="password"
							type="password"
							autoComplete="current-password"
							value={password}
							onChange={(e) => setPassword(e.target.value)}
							className={field}
						/>
					</label>
					<button
						type="submit"
						disabled={loading}
						className="mt-1 rounded-md bg-[var(--primary)] px-5 py-2 font-semibold text-[var(--text-body-sm)] text-[var(--primary-fg)] hover:opacity-90 disabled:opacity-50 cursor-pointer"
					>
						{loading ? 'Signing in...' : 'Sign in'}
					</button>
					{errorMsg && (
						<div role="alert" className="font-mono text-[12px] text-[var(--danger)]">
							{errorMsg}
						</div>
					)}
				</div>
			</form>
		</div>
	);
}
