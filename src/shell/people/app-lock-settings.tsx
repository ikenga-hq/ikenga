// App lock block — D-05 `profile` (`designs/people.html?state=profile`), WP-72.
//
// Lock when idle (switch + minutes), Unlock with (OS biometric | PIN), the
// PIN itself, and Lock now. Every change goes through Rust
// (`commands/app_lock.rs`), which re-validates and broadcasts
// `app-lock://changed`, so other windows follow.
//
// The design's rows are kept. One row is added: setting the PIN. The design
// assumes an OS unlock is already there. This build has no OS unlock yet (see
// the Rust header), so a PIN is needed before anything can lock.

import { useEffect, useId, useState } from 'react';

import { Button } from '@/components/ui/button';
import { Switch } from '@/components/ui/switch';
import { cn } from '@/components/ui/utils';
import {
	type AppLockMethod,
	type AppLockStatus,
	appLockClearSecret,
	appLockConfigure,
	appLockLock,
	appLockSetSecret,
} from '@/lib/tauri-cmd';

import {
	DEFAULT_IDLE_MINUTES,
	MAX_IDLE_MINUTES,
	MIN_IDLE_MINUTES,
	parseIdleMinutes,
	secretProblem,
	unlockMethodOptions,
} from './app-lock-model';
import { startAppLockSync, useAppLockStore } from './app-lock-store';
import { Kv, PeopleBlock, PeopleRow } from './frame';

function errorText(err: unknown): string {
	return err instanceof Error ? err.message : String(err);
}

export function AppLockBlock() {
	useEffect(() => startAppLockSync(), []);
	const status = useAppLockStore((s) => s.status);

	return (
		<PeopleBlock title="App lock">
			{status ? (
				<AppLockRows status={status} />
			) : (
				<p className="py-3 text-[var(--text-caption,12px)] leading-relaxed text-[var(--fg-muted)]">
					App lock guards the desktop app. It isn't available here: this is either a browser
					session on the daemon or a build without the lock commands.
				</p>
			)}
		</PeopleBlock>
	);
}

function AppLockRows({ status }: { status: AppLockStatus }) {
	const setStatus = useAppLockStore((s) => s.setStatus);
	const [error, setError] = useState<string | null>(null);
	const [minutesDraft, setMinutesDraft] = useState(String(status.idleMinutes));
	const minutesId = useId();

	// Follow changes from another window, or from Rust's clamping.
	useEffect(() => setMinutesDraft(String(status.idleMinutes)), [status.idleMinutes]);

	const configure = async (next: Partial<{ idleEnabled: boolean; idleMinutes: number; method: AppLockMethod }>) => {
		setError(null);
		try {
			setStatus(
				await appLockConfigure({
					idleEnabled: next.idleEnabled ?? status.idleEnabled,
					idleMinutes: next.idleMinutes ?? status.idleMinutes,
					method: next.method ?? status.method,
				})
			);
		} catch (err) {
			setError(errorText(err));
		}
	};

	const commitMinutes = () => {
		const parsed = parseIdleMinutes(minutesDraft);
		if (parsed === null) {
			setError(`Idle minutes must be a whole number from ${MIN_IDLE_MINUTES} to ${MAX_IDLE_MINUTES}.`);
			setMinutesDraft(String(status.idleMinutes));
			return;
		}
		if (parsed !== status.idleMinutes) void configure({ idleMinutes: parsed });
	};

	const lockNow = async () => {
		setError(null);
		try {
			setStatus(await appLockLock());
		} catch (err) {
			setError(errorText(err));
		}
	};

	const methods = unlockMethodOptions(status.biometric);
	const noSecret = !status.secretSet;

	return (
		<>
			<PeopleRow
				label="Lock when idle"
				sub="Locks the window. Sessions and runs keep going underneath."
			>
				<span className="inline-flex items-center gap-2">
					<Switch
						checked={status.idleEnabled}
						disabled={noSecret}
						onCheckedChange={(checked) => void configure({ idleEnabled: checked })}
						aria-label="Lock when idle"
						title={noSecret ? 'Set a PIN first' : undefined}
					/>
					<Kv className="min-w-[2ch]">{status.idleEnabled ? 'on' : 'off'}</Kv>
				</span>
				<span
					className={cn(
						'inline-flex h-7 items-center gap-1 rounded-[var(--radius-sm)] border border-[var(--border)] bg-[var(--bg-sunken)] px-2',
						!status.idleEnabled && 'opacity-50'
					)}
				>
					<input
						id={minutesId}
						type="text"
						inputMode="numeric"
						value={minutesDraft}
						disabled={!status.idleEnabled}
						onChange={(e) => setMinutesDraft(e.target.value)}
						onBlur={commitMinutes}
						onKeyDown={(e) => {
							if (e.key === 'Enter') {
								e.preventDefault();
								commitMinutes();
							}
						}}
						aria-label="Idle minutes before locking"
						placeholder={String(DEFAULT_IDLE_MINUTES)}
						className="w-10 bg-transparent font-mono text-[var(--text-caption,12px)] text-[var(--fg)] outline-none"
					/>
					<span className="font-mono text-[10px] text-[var(--fg-muted)]">min</span>
				</span>
				{noSecret && <Kv>Set a PIN below first.</Kv>}
			</PeopleRow>

			<PeopleRow label="Unlock with">
				<span
					role="radiogroup"
					aria-label="Unlock method"
					className="inline-flex overflow-hidden rounded-[var(--radius-sm)] border border-[var(--border)]"
				>
					{methods.map((m) => {
						const on = status.method === m.id;
						return (
							<button
								key={m.id}
								type="button"
								role="radio"
								aria-checked={on}
								disabled={m.disabled}
								title={m.why}
								onClick={() => !on && void configure({ method: m.id })}
								className={cn(
									'h-[26px] border-r border-[var(--border)] px-3 text-[var(--text-micro)] outline-none last:border-r-0 focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-[var(--primary)]',
									'disabled:cursor-not-allowed disabled:opacity-45',
									on
										? 'bg-[var(--primary-soft)] text-[var(--fg)]'
										: 'text-[var(--fg-muted)] hover:bg-[var(--bg-raised)] hover:text-[var(--fg)]'
								)}
							>
								{m.label}
							</button>
						);
					})}
				</span>
				{methods.some((m) => m.disabled && m.why) && (
					<Kv>{methods.find((m) => m.disabled && m.why)?.why}</Kv>
				)}
			</PeopleRow>

			<SecretRow status={status} onStatus={setStatus} />

			<PeopleRow label="Lock now">
				<Button
					type="button"
					variant="outline"
					size="sm"
					disabled={noSecret}
					onClick={() => void lockNow()}
					title={noSecret ? 'Set a PIN first — otherwise nothing could unlock it' : undefined}
				>
					Lock now
				</Button>
				<Kv>Every window locks. Rust holds the lock, so reloading doesn't clear it.</Kv>
			</PeopleRow>

			{error && (
				<div role="alert" className="border-t border-[var(--border-soft)] py-2 text-[var(--text-micro)] text-[var(--danger)]">
					{error}
				</div>
			)}
		</>
	);
}

type SecretMode = 'idle' | 'set' | 'change' | 'remove';

function SecretRow({
	status,
	onStatus,
}: {
	status: AppLockStatus;
	onStatus: (s: AppLockStatus) => void;
}) {
	const [mode, setMode] = useState<SecretMode>('idle');
	const [current, setCurrent] = useState('');
	const [next, setNext] = useState('');
	const [confirm, setConfirm] = useState('');
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState<string | null>(null);

	const reset = () => {
		setMode('idle');
		setCurrent('');
		setNext('');
		setConfirm('');
		setError(null);
	};

	const run = async (work: () => Promise<AppLockStatus>) => {
		setBusy(true);
		setError(null);
		try {
			onStatus(await work());
			reset();
		} catch (err) {
			setError(errorText(err));
		} finally {
			setBusy(false);
		}
	};

	const submitNew = () => {
		const problem = secretProblem(next);
		if (problem) return setError(problem);
		if (next !== confirm) return setError("The two entries don't match.");
		if (mode === 'change' && !current) return setError('Enter the current PIN.');
		void run(() => appLockSetSecret(next, mode === 'change' ? current : null));
	};

	const submitRemove = () => {
		if (!current) return setError('Enter the current PIN.');
		void run(() => appLockClearSecret(current));
	};

	const field = (
		value: string,
		set: (v: string) => void,
		label: string,
		autoComplete: string,
		autoFocus?: boolean
	) => (
		<input
			type="password"
			value={value}
			autoComplete={autoComplete}
			// biome-ignore lint/a11y/noAutofocus: the field the user just asked to open
			autoFocus={autoFocus}
			onChange={(e) => {
				set(e.target.value);
				setError(null);
			}}
			onKeyDown={(e) => {
				if (e.key === 'Enter') {
					e.preventDefault();
					if (mode === 'remove') submitRemove();
					else submitNew();
				} else if (e.key === 'Escape') {
					e.preventDefault();
					reset();
				}
			}}
			placeholder={label}
			aria-label={label}
			className="h-7 w-[160px] rounded-[var(--radius-sm)] border border-[var(--border)] bg-[var(--bg-sunken)] px-2 font-mono text-[var(--text-caption,12px)] text-[var(--fg)] outline-none focus:border-[var(--primary)]"
		/>
	);

	return (
		<PeopleRow
			label="PIN or passphrase"
			sub="What unlocks the lock screen. At least 4 characters. Stored as an argon2id hash on this device only."
			top={mode !== 'idle'}
		>
			{mode === 'idle' && (
				<>
					<Kv>{status.secretSet ? 'set' : 'not set'}</Kv>
					{status.secretSet ? (
						<>
							<Button type="button" variant="outline" size="sm" onClick={() => setMode('change')}>
								Change…
							</Button>
							<Button
								type="button"
								variant="ghost"
								size="sm"
								onClick={() => setMode('remove')}
								className="text-[var(--danger)]"
							>
								Remove…
							</Button>
						</>
					) : (
						<Button type="button" size="sm" onClick={() => setMode('set')}>
							Set a PIN…
						</Button>
					)}
				</>
			)}

			{(mode === 'set' || mode === 'change') && (
				<span className="flex flex-wrap items-center gap-2">
					{mode === 'change' && field(current, setCurrent, 'Current PIN', 'current-password', true)}
					{field(next, setNext, 'New PIN', 'new-password', mode === 'set')}
					{field(confirm, setConfirm, 'Repeat it', 'new-password')}
					<Button type="button" size="sm" disabled={busy} onClick={submitNew}>
						{busy ? 'Saving…' : 'Save'}
					</Button>
					<Button type="button" variant="ghost" size="sm" disabled={busy} onClick={reset}>
						Cancel
					</Button>
				</span>
			)}

			{mode === 'remove' && (
				<span className="flex flex-wrap items-center gap-2">
					{field(current, setCurrent, 'Current PIN', 'current-password', true)}
					<Button
						type="button"
						variant="destructive"
						size="sm"
						disabled={busy}
						onClick={submitRemove}
					>
						{busy ? 'Removing…' : 'Remove PIN'}
					</Button>
					<Button type="button" variant="ghost" size="sm" disabled={busy} onClick={reset}>
						Cancel
					</Button>
					<Kv>Removing it also turns idle lock off.</Kv>
				</span>
			)}

			{error && (
				<span role="alert" className="basis-full text-[var(--text-micro)] text-[var(--danger)]">
					{error}
				</span>
			)}
		</PeopleRow>
	);
}
