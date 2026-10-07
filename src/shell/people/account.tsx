// Account block — D-05 `profile-account` (`designs/people.html`), WP-76
// (G-ACCESS §2.2, G-98). Built as the **T1** account view; descoped on the
// desktop (T0 has no accounts, DEC-75), where WP-72's Profile tab is final.
//
// "Signed in as <username>", the `principal_id` (mono, copyable), an `admin`
// chip when `is_admin` (no other admin UI — account administration stays
// root CLI, §4.4), Change password (`POST /auth/password`) and Sign out
// (`POST /auth/logout`). D-2: no "Syncs" list and no email identity.

import { Copy, KeyRound, LogOut } from 'lucide-react';
import { useEffect, useId, useState } from 'react';

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
import { type AuthMe, currentPrincipal, fetchAuthMe } from '@/lib/transport/t1-session';

import { D05_DANGER, D05_FOCUS } from './focus';
import { Kv, PeopleBlock, PeopleRow } from './frame';
import { copyText } from '@/lib/clipboard';

/** §2.2 / §3.10 copy: signing out ends one session and bumps no epoch. */
export const SIGN_OUT_COPY =
	'This browser signs out. Your paired devices stay paired — revoke a lost one in Devices.';
/** §2.2 / §3.10 copy: a password change bumps `session_epoch`. */
export const CHANGE_PASSWORD_COPY =
	'Every other browser signs out, and open connections on your paired devices drop and reconnect. Devices stay paired — revoke a lost one in Devices.';

/** P-24 / G-PRINCIPAL §6.2: the server's minimum. */
export const MIN_PASSWORD = 12;

export type PasswordResult =
	| { ok: true }
	| { ok: false; reason: 'wrong' | 'policy' | 'throttled' | 'error'; message: string };

/** `POST /auth/password {current, new}` → 204 (the broker keeps this browser
 *  signed in on a fresh session id). */
export async function changePassword(current: string, next: string): Promise<PasswordResult> {
	let res: Response;
	try {
		res = await fetch('/auth/password', {
			method: 'POST',
			credentials: 'same-origin',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ current, new: next }),
		});
	} catch (e) {
		return { ok: false, reason: 'error', message: `Connection failed: ${String(e)}` };
	}
	if (res.ok) return { ok: true };
	if (res.status === 401) {
		return { ok: false, reason: 'wrong', message: 'The current password is wrong.' };
	}
	if (res.status === 429) {
		return { ok: false, reason: 'throttled', message: 'Too many attempts. Try again later.' };
	}
	if (res.status === 400) {
		return {
			ok: false,
			reason: 'policy',
			message: `Use at least ${MIN_PASSWORD} characters.`,
		};
	}
	return {
		ok: false,
		reason: 'error',
		message: `Changing the password failed (HTTP ${res.status}).`,
	};
}

/** `POST /auth/logout`, then back to the sign-in surface. */
export async function signOut(): Promise<boolean> {
	try {
		const res = await fetch('/auth/logout', { method: 'POST', credentials: 'same-origin' });
		if (!res.ok && res.status !== 401) return false;
	} catch {
		return false;
	}
	try {
		sessionStorage.removeItem('ikenga_share');
	} catch {
		// Nothing to clear.
	}
	if (typeof window !== 'undefined') window.location.assign('/');
	return true;
}

export function AccountBlock() {
	const [me, setMe] = useState<AuthMe | null>(() => currentPrincipal());
	const [copied, setCopied] = useState(false);
	const [signingOut, setSigningOut] = useState(false);
	const [changing, setChanging] = useState(false);
	const [notice, setNotice] = useState<string | null>(null);
	useEffect(() => {
		let cancelled = false;
		void fetchAuthMe().then((m) => {
			if (!cancelled && m) setMe(m);
		});
		return () => {
			cancelled = true;
		};
	}, []);

	const copyId = async () => {
		if (!me) return;
		setCopied(await copyText(me.principal_id));
	};

	return (
		<PeopleBlock title="Account" right={<Kv>signed in</Kv>}>
			<div data-block="account">
				<PeopleRow label="Signed in as">
					<span
						aria-hidden
						className="grid h-6 w-6 place-items-center rounded-[var(--radius-sm)] bg-[var(--primary-soft)] text-[11px] font-semibold"
						style={{ fontFamily: 'var(--font-display)' }}
					>
						{(me?.username ?? '?').slice(0, 1).toUpperCase()}
					</span>
					<b className="font-mono text-[length:var(--text-caption,12px)] font-medium text-[var(--fg)]">
						{me?.username ?? '—'}
					</b>
					{me?.is_admin && <StatusChip tone="accent">admin</StatusChip>}
				</PeopleRow>
				<PeopleRow
					label="Principal"
					sub="Your id on this server. Devices, shares and audit rows key on it."
				>
					<Kv className="break-all">{me?.principal_id ?? '—'}</Kv>
					<Button
						type="button"
						variant="ghost"
						size="xs"
						disabled={!me}
						onClick={() => void copyId()}
						aria-label="Copy principal id"
					>
						<Copy /> {copied ? 'Copied' : 'Copy'}
					</Button>
				</PeopleRow>
				<PeopleRow label="Password" sub="At least 12 characters.">
					<Button type="button" variant="outline" size="xs" onClick={() => setChanging(true)}>
						<KeyRound /> Change password
					</Button>
					{notice && <Kv>{notice}</Kv>}
				</PeopleRow>
				<PeopleRow label="Sign out" sub="Keeps the local profile and everything on the server.">
					<Button
						type="button"
						variant="outline"
						size="xs"
						className={D05_DANGER}
						onClick={() => setSigningOut(true)}
					>
						<LogOut /> Sign out
					</Button>
				</PeopleRow>
			</div>
			<SignOutConfirm open={signingOut} onClose={() => setSigningOut(false)} />
			<ChangePasswordDialog
				open={changing}
				onClose={() => setChanging(false)}
				onDone={() => setNotice('Password changed. Other browsers were signed out.')}
			/>
		</PeopleBlock>
	);
}

function SignOutConfirm({ open, onClose }: { open: boolean; onClose: () => void }) {
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const go = async () => {
		setBusy(true);
		setError(null);
		if (!(await signOut())) {
			setError('Signing out failed. Try again.');
			setBusy(false);
		}
	};
	return (
		<Dialog open={open} onOpenChange={(o) => !o && onClose()}>
			<DialogContent
				data-state="account-sign-out"
				className={`${D05_FOCUS} bg-[var(--bg-surface)] text-[var(--fg)]`}
			>
				<DialogHeader>
					<DialogTitle>Sign out?</DialogTitle>
					<DialogDescription>{SIGN_OUT_COPY}</DialogDescription>
				</DialogHeader>
				{error && (
					<p role="alert" className="m-0 text-[12px] text-[var(--danger)]">
						{error}
					</p>
				)}
				<DialogFooter>
					<Button type="button" variant="outline" size="sm" onClick={onClose}>
						Stay signed in
					</Button>
					<Button
						type="button"
						variant="destructive"
						size="sm"
						disabled={busy}
						onClick={() => void go()}
					>
						Sign out
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}

function ChangePasswordDialog({
	open,
	onClose,
	onDone,
}: {
	open: boolean;
	onClose: () => void;
	onDone: () => void;
}) {
	const [current, setCurrent] = useState('');
	const [next, setNext] = useState('');
	const [again, setAgain] = useState('');
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const ids = { current: useId(), next: useId(), again: useId() };
	useEffect(() => {
		if (open) {
			setCurrent('');
			setNext('');
			setAgain('');
			setError(null);
		}
	}, [open]);
	const blocker = !current
		? 'Enter your current password'
		: next.length < MIN_PASSWORD
			? `Use at least ${MIN_PASSWORD} characters`
			: next !== again
				? 'The new passwords differ'
				: null;
	const go = async () => {
		setBusy(true);
		setError(null);
		const r = await changePassword(current, next);
		setBusy(false);
		if (r.ok) {
			onDone();
			onClose();
		} else {
			setError(r.message);
		}
	};
	const field =
		'h-8 w-full rounded-[var(--radius-sm)] border border-[var(--border)] bg-[var(--bg-sunken)] px-2 text-[length:var(--text-caption,12px)] text-[var(--fg)] outline-none focus:border-[var(--primary)]';
	return (
		<Dialog open={open} onOpenChange={(o) => !o && onClose()}>
			<DialogContent
				data-state="account-password"
				className={`${D05_FOCUS} bg-[var(--bg-surface)] text-[var(--fg)]`}
			>
				<DialogHeader>
					<DialogTitle>Change password</DialogTitle>
					<DialogDescription>{CHANGE_PASSWORD_COPY}</DialogDescription>
				</DialogHeader>
				<form
					className="space-y-2"
					onSubmit={(e) => {
						e.preventDefault();
						if (!blocker) void go();
					}}
				>
					<label
						htmlFor={ids.current}
						className="block text-[length:var(--text-micro)] text-[var(--fg-muted)]"
					>
						Current password
					</label>
					<input
						id={ids.current}
						type="password"
						autoComplete="current-password"
						value={current}
						onChange={(e) => setCurrent(e.target.value)}
						className={field}
					/>
					<label
						htmlFor={ids.next}
						className="block text-[length:var(--text-micro)] text-[var(--fg-muted)]"
					>
						New password
					</label>
					<input
						id={ids.next}
						type="password"
						autoComplete="new-password"
						value={next}
						onChange={(e) => setNext(e.target.value)}
						className={field}
					/>
					<label
						htmlFor={ids.again}
						className="block text-[length:var(--text-micro)] text-[var(--fg-muted)]"
					>
						New password, again
					</label>
					<input
						id={ids.again}
						type="password"
						autoComplete="new-password"
						value={again}
						onChange={(e) => setAgain(e.target.value)}
						className={field}
					/>
					{error && (
						<p role="alert" className="m-0 text-[12px] text-[var(--danger)]">
							{error}
						</p>
					)}
					<DialogFooter>
						<Button type="button" variant="outline" size="sm" onClick={onClose}>
							Cancel
						</Button>
						<span title={blocker ?? undefined}>
							<Button type="submit" size="sm" disabled={busy || blocker !== null}>
								Change password
							</Button>
						</span>
					</DialogFooter>
				</form>
			</DialogContent>
		</Dialog>
	);
}
