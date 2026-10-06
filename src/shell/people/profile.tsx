// Profile tab — D-05 `profile` (`designs/people.html?state=profile`), WP-72;
// WP-76 adds the T1 Account block (`profile-account`, G-ACCESS §2.2, G-98).
//
// The local profile, which is always there, and the App lock block. On a T1
// server (a signed-in browser) the Account block sits between them. On the
// desktop it is descoped (DEC-75, D-3): a T0 shell has no account to show.
//
// Display name is the existing onboarding name (`useShellStore().userName`,
// persisted as `workspace.userName`). "OS user" is what the shipped shell
// knows about you: the OS username (`os_username`) plus the host and OS that
// `app_lock_status` reports.

import { useEffect, useId, useState } from 'react';
import { useShellStore } from '@/lib/shell/shell-store';
import { osUsername } from '@/lib/tauri-cmd';

import { isT1Session } from '@/lib/transport/t1-session';

import { AccountBlock } from './account';
import { AppLockBlock } from './app-lock-settings';
import { startAppLockSync, useAppLockStore } from './app-lock-store';
import { D05_FOCUS } from './focus';
import { Kv, PeopleBlock, PeopleHeader, PeopleRow } from './frame';

export function ProfileTab() {
	return (
		<div
			data-state={isT1Session() ? 'profile-account' : 'profile'}
			className={`${D05_FOCUS} mx-auto w-full max-w-[960px] space-y-4 px-6 py-6`}
		>
			<PeopleHeader tab="profile" />
			{/* D-05 draws the profile blocks narrower than the tables, under
			    the same header as every other tab (it doesn't move). */}
			<div className="max-w-[720px] space-y-4">
				<LocalProfileBlock />
				{isT1Session() && <AccountBlock />}
				<AppLockBlock />
			</div>
		</div>
	);
}

/** First letter of the name, for the avatar. */
export function avatarInitial(name: string, fallback: string): string {
	const source = name.trim() || fallback.trim();
	const first = Array.from(source)[0];
	return first ? first.toUpperCase() : '?';
}

function LocalProfileBlock() {
	useEffect(() => startAppLockSync(), []);
	const userName = useShellStore((s) => s.userName);
	const setUserName = useShellStore((s) => s.setUserName);
	const status = useAppLockStore((s) => s.status);
	const [draft, setDraft] = useState(userName);
	const [osUser, setOsUser] = useState<string | null>(null);
	const nameId = useId();

	useEffect(() => setDraft(userName), [userName]);
	useEffect(() => {
		let cancelled = false;
		osUsername()
			.then((name) => {
				if (!cancelled) setOsUser(name);
			})
			.catch(() => {
				if (!cancelled) setOsUser(null);
			});
		return () => {
			cancelled = true;
		};
	}, []);

	const commit = () => {
		if (draft.trim() !== userName) setUserName(draft);
	};

	const osLine = [status?.host, status?.os].filter(Boolean).join(' · ');

	return (
		<PeopleBlock title="Local profile" right={<Kv>always present</Kv>}>
			<div className="flex items-start gap-3 py-3">
				<div
					aria-hidden
					className="grid h-10 w-10 flex-none place-items-center rounded-full bg-[var(--primary-soft)] font-semibold text-[var(--fg)]"
					style={{ fontFamily: 'var(--font-display)' }}
				>
					{avatarInitial(draft, osUser ?? '')}
				</div>
				<div className="min-w-0 flex-1">
					<PeopleRow
						label="Display name"
						htmlFor={nameId}
						sub="Shown on your own devices. Nothing leaves this machine: there is no account."
					>
						<input
							id={nameId}
							type="text"
							value={draft}
							onChange={(e) => setDraft(e.target.value)}
							onBlur={commit}
							onKeyDown={(e) => {
								if (e.key === 'Enter') {
									e.preventDefault();
									commit();
								} else if (e.key === 'Escape') {
									e.preventDefault();
									setDraft(userName);
								}
							}}
							placeholder={osUser ?? 'Your name'}
							className="h-7 w-[240px] max-w-full rounded-[var(--radius-sm)] border border-[var(--border)] bg-[var(--bg-sunken)] px-2 text-[length:var(--text-caption,12px)] text-[var(--fg)] outline-none focus:border-[var(--primary)]"
						/>
					</PeopleRow>
					<PeopleRow
						label="OS user"
						sub="Read from the operating system. This is all the shell knows about you."
					>
						<Kv>
							<b className="font-medium text-[var(--fg)]">{osUser ?? '—'}</b>
							{osLine && ` · ${osLine}`}
						</Kv>
					</PeopleRow>
				</div>
			</div>
		</PeopleBlock>
	);
}
