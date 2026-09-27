import { useQuery } from '@tanstack/react-query';
import { Lock, LockKeyhole, Unlock } from 'lucide-react';
import { createContext, useContext, useMemo, useState, type ReactNode } from 'react';

import { Button } from '@/components/ui/button';
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { StatusChip } from '@/components/ui/status-chip';
import type { SecretsLockState } from '@/lib/tauri-cmd';
import { vaultStatusQueryOptions } from '@/lib/queries/secrets';
import {
	secretsLockStateQueryOptions,
	useLockSecrets,
	useSetSecretsPassphrase,
	useUnlockSecrets,
} from '@/lib/queries/secrets';

type UnlockSheetMode = 'auto' | 'unlock' | 'set';

interface UnlockSheetContextValue {
	open: (mode?: UnlockSheetMode) => void;
}

const UnlockSheetContext = createContext<UnlockSheetContextValue | null>(null);

export function SecretsUnlockSheetProvider({ children }: { children: ReactNode }) {
	const [open, setOpen] = useState(false);
	const [mode, setMode] = useState<UnlockSheetMode>('auto');

	const value = useMemo<UnlockSheetContextValue>(
		() => ({
			open: (nextMode: UnlockSheetMode = 'auto') => {
				setMode(nextMode);
				setOpen(true);
			},
		}),
		[]
	);

	return (
		<UnlockSheetContext.Provider value={value}>
			{children}
			<SecretsUnlockSheet
				open={open}
				mode={mode}
				onClose={() => setOpen(false)}
			/>
		</UnlockSheetContext.Provider>
	);
}

export function useUnlockSheet(): UnlockSheetContextValue {
	const value = useContext(UnlockSheetContext);
	if (!value) {
		throw new Error('useUnlockSheet must be used inside SecretsUnlockSheetProvider');
	}
	return value;
}

interface SecretsUnlockSheetProps {
	open: boolean;
	mode: UnlockSheetMode;
	onClose: () => void;
}

function SecretsUnlockSheet({ open, mode, onClose }: SecretsUnlockSheetProps) {
	const lockState = useQuery({
		...secretsLockStateQueryOptions(),
		enabled: open,
		refetchInterval: 5_000,
	});
	const status = useQuery({
		...vaultStatusQueryOptions(),
		enabled: open,
	});

	const state: SecretsLockState | undefined = lockState.data;
	const configured = state?.configured ?? false;
	const locked = state?.locked ?? true;
	const available = status.data?.available === true;

	const effectiveMode: 'unlock' | 'set' | 'unlocked' =
		mode === 'set' || (!configured && mode === 'auto')
			? 'set'
			: configured && !locked
				? 'unlocked'
				: 'unlock';

	return (
		<Dialog open={open} onOpenChange={(next) => !next && onClose()}>
			<DialogContent className="sm:max-w-md">
				<DialogHeader>
					<DialogTitle>
						{effectiveMode === 'set'
							? 'Set a vault passphrase'
							: effectiveMode === 'unlock'
								? 'Unlock the vault'
								: 'Vault unlocked'}
					</DialogTitle>
					<DialogDescription>
						Secrets are encrypted with AES-256-GCM under a passphrase-derived key. The key lives
						in memory only while the vault is unlocked.
					</DialogDescription>
				</DialogHeader>

				{!available && status.data && (
					<div className="rounded-md border border-red-200 bg-red-50 px-3 py-2 text-xs text-red-900 dark:border-red-900/40 dark:bg-red-950/30 dark:text-red-200">
						Vault unavailable: {status.data.error ?? 'unknown error'}
					</div>
				)}

				{effectiveMode === 'set' && <SetPassphraseBody onDone={onClose} />}
				{effectiveMode === 'unlock' && <UnlockBody onDone={onClose} />}
				{effectiveMode === 'unlocked' && <UnlockedBody onDone={onClose} />}

				<DialogFooter className="flex-row items-center sm:justify-between">
					<StatusChip tone={available ? 'live' : 'danger'} dot>
						{available ? (locked ? 'Locked' : 'Unlocked') : 'Unavailable'}
					</StatusChip>
					<Button variant="ghost" size="sm" onClick={onClose}>
						Close
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}

function SetPassphraseBody({ onDone }: { onDone: () => void }) {
	const [passphrase, setPassphrase] = useState('');
	const [confirmPassphrase, setConfirmPassphrase] = useState('');
	const mutation = useSetSecretsPassphrase();
	const mismatch = confirmPassphrase.length > 0 && passphrase !== confirmPassphrase;

	return (
		<div className="space-y-3">
			<p className="text-xs text-muted-foreground">
				The passphrase unlocks every secret in the vault. There is no recovery: if it is lost, the
				encrypted values are gone.
			</p>
			<div className="space-y-1">
				<label className="text-xs font-medium" htmlFor="unlock-sheet-passphrase">
					Passphrase
				</label>
				<Input
					id="unlock-sheet-passphrase"
					type="password"
					value={passphrase}
					onChange={(e) => setPassphrase(e.target.value)}
					autoFocus
					autoComplete="new-password"
				/>
			</div>
			<div className="space-y-1">
				<label className="text-xs font-medium" htmlFor="unlock-sheet-confirm">
					Confirm passphrase
				</label>
				<Input
					id="unlock-sheet-confirm"
					type="password"
					value={confirmPassphrase}
					onChange={(e) => setConfirmPassphrase(e.target.value)}
					autoComplete="new-password"
				/>
				{mismatch && <p className="text-xs text-red-700">Passphrases don&apos;t match.</p>}
			</div>
			{mutation.isError && (
				<p role="alert" className="text-xs text-red-700">
					{mutation.error.message}
				</p>
			)}
			<Button
				className="w-full"
				disabled={
					passphrase.length === 0 || mismatch || mutation.isPending
				}
				onClick={() =>
					mutation.mutate(
						{ passphrase, currentPassphrase: null },
						{ onSuccess: onDone }
					)
				}
			>
				<LockKeyhole className="mr-1.5 h-3.5 w-3.5" />
				{mutation.isPending ? 'Setting…' : 'Set passphrase and unlock'}
			</Button>
		</div>
	);
}

function UnlockBody({ onDone }: { onDone: () => void }) {
	const [passphrase, setPassphrase] = useState('');
	const mutation = useUnlockSecrets();

	return (
		<div className="space-y-3">
			<div className="space-y-1">
				<label className="text-xs font-medium" htmlFor="unlock-sheet-passphrase">
					Passphrase
				</label>
				<Input
					id="unlock-sheet-passphrase"
					type="password"
					value={passphrase}
					onChange={(e) => setPassphrase(e.target.value)}
					autoFocus
					autoComplete="current-password"
				/>
			</div>
			{mutation.isError && (
				<p role="alert" className="text-xs text-red-700">
					{mutation.error.message}
				</p>
			)}
			<Button
				className="w-full"
				disabled={passphrase.length === 0 || mutation.isPending}
				onClick={() => mutation.mutate(passphrase, { onSuccess: onDone })}
			>
				<Unlock className="mr-1.5 h-3.5 w-3.5" />
				{mutation.isPending ? 'Unlocking…' : 'Unlock'}
			</Button>
		</div>
	);
}

function UnlockedBody({ onDone }: { onDone: () => void }) {
	const mutation = useLockSecrets();

	return (
		<div className="space-y-3">
			<p className="text-xs text-muted-foreground">
				The vault unlocks on passphrase entry and re-locks after 5 minutes idle.
			</p>
			<Button
				variant="outline"
				className="w-full"
				disabled={mutation.isPending}
				onClick={() => mutation.mutate(undefined, { onSuccess: onDone })}
			>
				<Lock className="mr-1.5 h-3.5 w-3.5" />
				{mutation.isPending ? 'Locking…' : 'Lock now'}
			</Button>
		</div>
	);
}
