import { createFileRoute } from '@tanstack/react-router';
import { type FormEvent, useEffect, useId, useState } from 'react';

import { type ShareSelection, setShareMode } from '@/lib/transport';
import { type AuthMe, fetchAuthMe, signInWithPassword } from '@/lib/transport/t1-session';

// `/remote/invite#t=<token>` — accepting a "Share kola" invite (G-ACCESS §7.3,
// WP-76; T1 only). The token rides the fragment, so it never reaches a server
// log. The page inspects the invite (`POST /access/invite/inspect`), then
// accepts it as the signed-in account (form 1) or — only when the invite
// allows it (N-11 / DEC-85) — with a new username and password (form 2),
// which the server provisions in one transaction with the membership. On
// success the tab opens the shared project in share mode (§4.5.2).
//
// A signed-out browser never reaches the router on a T1 server: the boot
// path renders `ReauthOverlay`, which shows this page for `/remote/invite`.
export const Route = createFileRoute('/remote/invite')({
	component: () => <InvitePage />,
});

/** `POST /access/invite/inspect`'s body (§7.3). */
export interface InviteInfo {
	project_name: string;
	owner_username: string | null;
	role: 'operator' | 'reviewer' | 'guest';
	scope: 'project' | 'artifact';
	artifact_path?: string;
	expires_at: number;
	member_expires_at?: number;
	allow_new_account: boolean;
}

export interface AcceptedShare {
	projectKey: string;
	ownerPrincipalId: string;
	projectId: string;
	projectName: string;
	ownerUsername?: string | null;
	role: 'operator' | 'reviewer' | 'guest';
	/** Fixed at issue (§7.2); absent from an older server → project. */
	scope?: 'project' | 'artifact';
	artifactPath?: string | null;
}

const ROLE_LINE: Record<InviteInfo['role'], string> = {
	operator: 'Operator — can dispatch to Chi and answer its asks',
	reviewer: 'Reviewer — reads files and transcripts, comments, cannot dispatch',
	guest: 'Guest — one artifact, for a while',
};

/** The token in `#t=…`, or null. */
export function tokenFromHash(hash: string): string | null {
	const m = /(?:^#|&)t=([^&]+)/.exec(hash);
	if (!m?.[1]) return null;
	const t = decodeURIComponent(m[1]);
	return t.startsWith('iki1.') ? t : null;
}

/** An `{ok:false, error, message}` body → the line the page shows. */
export function inviteErrorCopy(
	status: number,
	body: { error?: string; message?: string } | null
): string {
	if (status === 410)
		return 'This invite link is not valid: it was used, revoked or has expired. Ask for a new one.';
	if (status === 429) return 'Too many tries from this address. Wait a few minutes.';
	if (status === 409)
		return body?.message ?? 'This account already has access, or the name is taken.';
	if (status === 401) return 'Sign in to accept this invite.';
	return body?.message ?? `Something went wrong (HTTP ${status}).`;
}

async function post(
	path: string,
	body: unknown
): Promise<{ status: number; json: Record<string, unknown> | null }> {
	const res = await fetch(path, {
		method: 'POST',
		credentials: 'same-origin',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify(body),
	});
	let json: Record<string, unknown> | null = null;
	try {
		json = (await res.json()) as Record<string, unknown>;
	} catch {
		json = null;
	}
	return { status: res.status, json };
}

/** The share selection an accepted invite opens (§4.5.2): scope and
 *  artifact as fixed at issue, so a Guest lands artifact-scoped (WP76-R7). */
export function selectionFromAccept(share: AcceptedShare): ShareSelection {
	const artifact = share.scope === 'artifact' || !!share.artifactPath;
	return {
		projectKey: share.projectKey,
		projectId: share.projectId,
		projectName: share.projectName,
		ownerUsername: share.ownerUsername ?? null,
		role: share.role,
		scope: artifact ? 'artifact' : 'project',
		...(artifact && share.artifactPath ? { artifactPath: share.artifactPath } : {}),
	};
}

function enter(share: AcceptedShare | undefined) {
	if (share) setShareMode(selectionFromAccept(share));
	window.location.assign('/');
}

type Phase =
	| { kind: 'loading' }
	| { kind: 'error'; message: string }
	| { kind: 'ready'; info: InviteInfo; me: AuthMe | null }
	| { kind: 'accepted'; projectName: string };

export function InvitePage() {
	const [token] = useState(() =>
		typeof window === 'undefined' ? null : tokenFromHash(window.location.hash)
	);
	const [phase, setPhase] = useState<Phase>({ kind: 'loading' });
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const [form, setForm] = useState<'signin' | 'create'>('signin');
	const [username, setUsername] = useState('');
	const [password, setPassword] = useState('');
	const ids = { user: useId(), pass: useId() };

	useEffect(() => {
		if (!token) {
			setPhase({
				kind: 'error',
				message: 'This invite link is incomplete. Open the whole link you were sent.',
			});
			return;
		}
		// The token stays in memory only; drop it from the address bar.
		try {
			window.history.replaceState(null, '', window.location.pathname);
		} catch {
			// Leave the URL as it is.
		}
		let cancelled = false;
		void (async () => {
			const [inspected, me] = await Promise.all([
				post('/access/invite/inspect', { token }),
				fetchAuthMe(),
			]);
			if (cancelled) return;
			if (inspected.status !== 200 || !inspected.json) {
				setPhase({ kind: 'error', message: inviteErrorCopy(inspected.status, inspected.json) });
				return;
			}
			const info = inspected.json as unknown as InviteInfo;
			setForm(info.allow_new_account ? 'create' : 'signin');
			setPhase({ kind: 'ready', info, me });
		})();
		return () => {
			cancelled = true;
		};
	}, [token]);

	const accept = async (body: Record<string, unknown>) => {
		const r = await post('/access/invite/accept', { token, ...body });
		if (r.status === 200 && r.json?.ok) {
			const share = r.json.share as AcceptedShare | undefined;
			setPhase({ kind: 'accepted', projectName: share?.projectName ?? 'the project' });
			enter(share);
			return true;
		}
		setError(inviteErrorCopy(r.status, r.json));
		return false;
	};

	const submit = async (e: FormEvent) => {
		e.preventDefault();
		if (busy) return;
		setBusy(true);
		setError(null);
		try {
			if (phase.kind === 'ready' && phase.me) {
				await accept({ existing: true });
			} else if (form === 'signin') {
				const signed = await signInWithPassword(username, password);
				if (!signed.ok) {
					setError(signed.message);
					return;
				}
				await accept({ existing: true });
			} else {
				if (password.length < 12) {
					setError('Use at least 12 characters.');
					return;
				}
				await accept({ username, password });
			}
		} finally {
			setBusy(false);
		}
	};

	const field =
		'w-full rounded-md border border-[var(--border)] bg-[var(--bg-sunken)] px-3 py-2 text-[length:var(--text-body-sm)] text-[var(--fg)] outline-none focus:border-[var(--primary)] focus:ring-2 focus:ring-[var(--primary-soft)]';
	const primary =
		'w-full rounded-md bg-[var(--primary)] px-5 py-2 font-semibold text-[length:var(--text-body-sm)] text-[var(--primary-fg)] hover:opacity-90 disabled:opacity-50 cursor-pointer';

	return (
		<div
			data-state={phase.kind === 'accepted' ? 'invite-accepted' : 'invite'}
			className="fixed inset-0 z-50 grid place-items-center overflow-y-auto bg-[var(--bg-base)] p-6 text-[var(--fg)]"
		>
			<div className="w-full max-w-[420px] space-y-4 text-center">
				<h1
					className="m-0 text-[34px] font-semibold tracking-tight"
					style={{ fontFamily: 'var(--font-display)' }}
				>
					Ikenga
				</h1>
				{phase.kind === 'loading' && (
					<p className="m-0 text-[var(--fg-muted)]">Opening the invite…</p>
				)}
				{phase.kind === 'error' && (
					<p role="alert" className="m-0 text-[length:var(--text-body-sm)] text-[var(--danger)]">
						{phase.message}
					</p>
				)}
				{phase.kind === 'accepted' && (
					<p className="m-0 text-[var(--fg-muted)]">You're in. Opening {phase.projectName}…</p>
				)}
				{phase.kind === 'ready' && (
					<form onSubmit={submit} aria-label="Accept invite" className="space-y-3 text-left">
						<p className="m-0 text-center text-[length:var(--text-body-sm)] leading-relaxed text-[var(--fg-muted)]">
							<b className="text-[var(--fg)]">{phase.info.owner_username ?? 'Someone'}</b> shared{' '}
							<b className="text-[var(--fg)]">{phase.info.project_name}</b> with you on{' '}
							{window.location.host}.
						</p>
						<ul className="m-0 list-none space-y-1 rounded-md border border-[var(--border)] bg-[var(--bg-surface)] p-3 text-[length:var(--text-caption,12px)]">
							<li>{ROLE_LINE[phase.info.role]}</li>
							<li className="text-[var(--fg-muted)]">
								{phase.info.scope === 'artifact'
									? `Scope: ${phase.info.artifact_path ?? 'one artifact'}`
									: 'Scope: the whole project'}
							</li>
							{phase.info.member_expires_at && (
								<li className="text-[var(--fg-muted)]">
									Access ends {new Date(phase.info.member_expires_at).toLocaleString()}
								</li>
							)}
							<li className="text-[var(--fg-muted)]">Secrets are never shared.</li>
						</ul>
						{phase.me ? (
							<button type="submit" disabled={busy} className={primary}>
								{busy ? 'Accepting…' : `Accept as ${phase.me.username}`}
							</button>
						) : (
							<>
								{phase.info.allow_new_account && (
									<div className="flex justify-center gap-1 text-[length:var(--text-micro)]">
										{(['create', 'signin'] as const).map((f) => (
											<button
												key={f}
												type="button"
												aria-pressed={form === f}
												onClick={() => setForm(f)}
												className={
													form === f
														? 'rounded px-2 py-1 font-semibold text-[var(--fg)] underline underline-offset-4'
														: 'rounded px-2 py-1 text-[var(--fg-muted)]'
												}
											>
												{f === 'create' ? 'Create an account' : 'I have an account'}
											</button>
										))}
									</div>
								)}
								<label
									htmlFor={ids.user}
									className="block text-[length:var(--text-micro)] text-[var(--fg-muted)]"
								>
									Username
								</label>
								<input
									id={ids.user}
									autoComplete="username"
									value={username}
									onChange={(e) => setUsername(e.target.value)}
									className={field}
									spellCheck={false}
									autoCapitalize="none"
								/>
								<label
									htmlFor={ids.pass}
									className="block text-[length:var(--text-micro)] text-[var(--fg-muted)]"
								>
									Password{form === 'create' ? ' (at least 12 characters)' : ''}
								</label>
								<input
									id={ids.pass}
									type="password"
									autoComplete={form === 'create' ? 'new-password' : 'current-password'}
									value={password}
									onChange={(e) => setPassword(e.target.value)}
									className={field}
								/>
								<button type="submit" disabled={busy || !username || !password} className={primary}>
									{busy
										? 'Working…'
										: form === 'create'
											? 'Create account and join'
											: 'Sign in to accept'}
								</button>
								{!phase.info.allow_new_account && (
									<p className="m-0 text-center text-[length:var(--text-micro)] text-[var(--fg-muted)]">
										This invite is for an existing account on this server. No account? Ask an admin
										to create one.
									</p>
								)}
							</>
						)}
						{error && (
							<p role="alert" className="m-0 font-mono text-[12px] text-[var(--danger)]">
								{error}
							</p>
						)}
					</form>
				)}
			</div>
		</div>
	);
}
