// The T1 sign-in surface — D-05 `sign-in` (`designs/people.html`), restyled
// by WP-76 (G-ACCESS §2.2, G-98, P-25) over WP-20's minimal username /
// password form (G-PRINCIPAL §2.4, OD-12). `ReauthOverlay` renders it in place
// of the token field when the server reports tier t1.
//
// Deviation D-1 (forced by DEC-75 / G-98 / P-25): the mockup's "account
// optional" lede, email field, "Continue without an account" and sync fine
// print become: a Username field, "Sign in to <host>. Your files, sessions and
// secrets stay on this server.", and **Pair this device with a code** with
// the same weight as Sign in (→ the "Pair this device" mode, `/remote/pair`).

import { type FormEvent, useId, useState } from 'react';
import { useReauthStore } from '@/lib/transport/reauth-store';
import { currentPrincipal } from '@/lib/transport/t1-session';
import { D05_FOCUS } from '@/shell/people/focus';

export function T1SignInForm() {
	const errorMsg = useReauthStore((s) => s.errorMsg);
	const signIn = useReauthStore((s) => s.signIn);
	const setMode = useReauthStore((s) => s.setMode);
	// A session that ended mid-use (expired, signed out elsewhere, password
	// changed) still knows who it was: prefill that name.
	const previous = currentPrincipal();
	const [username, setUsername] = useState(previous?.username ?? '');
	const [password, setPassword] = useState('');
	const [loading, setLoading] = useState(false);
	const host = typeof window !== 'undefined' ? window.location.host : '';
	const ids = { user: useId(), pass: useId() };

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
		'w-full rounded-md border border-[var(--border)] bg-[var(--bg-sunken)] px-3 py-2 text-[length:var(--text-body-sm)] text-[var(--fg)] outline-none focus:border-[var(--primary)] focus:ring-2 focus:ring-[var(--primary-soft)]';
	const wide =
		'w-full rounded-md px-5 py-2 text-[length:var(--text-body-sm)] cursor-pointer disabled:opacity-50';

	return (
		<div
			data-state="sign-in"
			className={`${D05_FOCUS} fixed inset-0 z-50 grid place-items-center overflow-y-auto bg-[var(--bg-base)] p-6 text-[var(--fg)]`}
		>
			<form onSubmit={submit} aria-label="Sign in" className="w-full max-w-[420px] space-y-3">
				<h1
					className="m-0 text-center text-[34px] font-semibold tracking-tight"
					style={{ fontFamily: 'var(--font-display)' }}
				>
					Ikenga
				</h1>
				<h2 className="sr-only">{previous ? 'Sign in again' : 'Sign in'}</h2>
				<p className="m-0 text-center text-[length:var(--text-body-sm)] leading-relaxed text-[var(--fg-muted)]">
					{previous
						? 'Your session ended. Your work is untouched — sign in to pick it back up.'
						: `Sign in to ${host}. Your files, sessions and secrets stay on this server.`}
				</p>
				<label htmlFor={ids.user} className="sr-only">
					Username
				</label>
				<input
					id={ids.user}
					name="username"
					autoComplete="username"
					placeholder="Username"
					value={username}
					onChange={(e) => setUsername(e.target.value)}
					className={field}
					spellCheck={false}
					autoCapitalize="none"
				/>
				<label htmlFor={ids.pass} className="sr-only">
					Password
				</label>
				<input
					id={ids.pass}
					name="password"
					type="password"
					autoComplete="current-password"
					placeholder="Password"
					value={password}
					onChange={(e) => setPassword(e.target.value)}
					className={field}
				/>
				<button
					type="submit"
					disabled={loading}
					className={`${wide} border border-[var(--primary)] bg-[var(--primary)] font-semibold text-[var(--primary-fg)] hover:opacity-90`}
				>
					{loading ? 'Signing in...' : 'Sign in'}
				</button>
				{errorMsg && (
					<div role="alert" className="text-center font-mono text-[12px] text-[var(--danger)]">
						{errorMsg}
					</div>
				)}
				<div className="flex items-center gap-3 text-[length:var(--text-micro)] text-[var(--fg-faint)]">
					<span className="h-px flex-1 bg-[var(--border-soft)]" />
					or
					<span className="h-px flex-1 bg-[var(--border-soft)]" />
				</div>
				<button
					type="button"
					onClick={() => setMode('pair')}
					className={`${wide} border border-[var(--border)] bg-[var(--bg-surface)] text-[var(--fg)] hover:bg-[var(--bg-sunken)]`}
				>
					Pair this device with a code
				</button>
				<p className="m-0 pt-2 text-center text-[length:var(--text-micro)] leading-relaxed text-[var(--fg-muted)]">
					Accounts on this server are created by its operator, or by an invite someone shared with
					you. Nothing here is synced anywhere else.
				</p>
			</form>
		</div>
	);
}
