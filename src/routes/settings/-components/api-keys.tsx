// Vault-backed API key manager. Reads/writes through the secrets queries
// (Stronghold-backed in Rust). Imported by Settings → Integrations.

import { useQuery } from '@tanstack/react-query';
import { useEffect, useMemo, useState } from 'react';
import { confirm as confirmDialog } from '@/lib/transport/dialog-shim';
import { CheckCircle2, Eye, EyeOff, KeyRound, Pencil, Plus, Trash2, XCircle } from 'lucide-react';

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
import { cn } from '@/components/ui/utils';
import {
	useDeleteSecret,
	useSetSecret,
	vaultKeysQueryOptions,
	vaultStatusQueryOptions,
} from '@/lib/queries/secrets';
import { secretsGet } from '@/lib/tauri-cmd';

import { REVEAL_TIMEOUT_MS } from './clear-data';

type KeyCategory = {
	label: string;
	keys: Array<{ name: string; hint?: string }>;
};

export const KEY_CATALOG: KeyCategory[] = [
	{
		label: 'LLM',
		keys: [
			{ name: 'ANTHROPIC_API_KEY', hint: 'Claude SDK adapter, agents' },
			{ name: 'OPENAI_API_KEY', hint: 'Image gen, embeddings' },
		],
	},
	{
		label: 'Email',
		keys: [
			{ name: 'RESEND_API_KEY', hint: 'getroyalti.com cold outreach' },
			{ name: 'LISTMONK_API_URL' },
			{ name: 'LISTMONK_USERNAME' },
			{ name: 'LISTMONK_PASSWORD' },
		],
	},
	{
		label: 'CRM + DB',
		keys: [
			{ name: 'TWENTY_API_URL' },
			{ name: 'TWENTY_API_KEY' },
			{ name: 'SUPABASE_SERVICE_ROLE_KEY', hint: 'Privileged DB access' },
		],
	},
	{
		label: 'Pkgs (Supabase capability)',
		keys: [
			{ name: 'VITE_SUPABASE_URL', hint: 'Shared Supabase URL for pkgs' },
			{ name: 'VITE_SUPABASE_ANON_KEY', hint: 'Shared anon key for pkgs' },
		],
	},
	{
		label: 'Payments',
		keys: [{ name: 'STRIPE_SECRET_KEY' }],
	},
];

export function ApiKeysSectionBody() {
	const status = useQuery(vaultStatusQueryOptions());
	const keys = useQuery(vaultKeysQueryOptions());
	const [editKey, setEditKey] = useState<string | null>(null);
	const [revealedKey, setRevealedKey] = useState<{ key: string; value: string } | null>(null);

	useEffect(() => {
		if (!revealedKey) return;
		const t = setTimeout(() => setRevealedKey(null), REVEAL_TIMEOUT_MS);
		return () => clearTimeout(t);
	}, [revealedKey]);

	const known = useMemo(() => new Set(keys.data ?? []), [keys.data]);
	const vaultAvailable = status.data?.available ?? false;

	async function handleReveal(name: string) {
		if (revealedKey?.key === name) {
			setRevealedKey(null);
			return;
		}
		const v = await secretsGet(name);
		if (v != null) setRevealedKey({ key: name, value: v });
	}

	return (
		<div className="space-y-3 px-4 py-3">
			<p className="text-xs text-muted-foreground">
				Stored encrypted in your OS keychain. Sidecars read these instead of <code>.env</code> files
				when the app is running.
			</p>

			{status.data && (
				<div
					className={cn(
						'rounded-md border px-3 py-2 text-xs',
						vaultAvailable
							? 'border-emerald-200 bg-emerald-50 text-emerald-900 dark:border-emerald-900/40 dark:bg-emerald-950/30 dark:text-emerald-200'
							: 'border-red-200 bg-red-50 text-red-900 dark:border-red-900/40 dark:bg-red-950/30 dark:text-red-200'
					)}
				>
					<div className="flex items-start gap-2">
						{vaultAvailable ? (
							<CheckCircle2 className="mt-0.5 h-3.5 w-3.5 shrink-0" />
						) : (
							<XCircle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
						)}
						<div>
							{vaultAvailable ? (
								<span>Vault unlocked via {status.data.keychainBackend}.</span>
							) : (
								<span>
									Vault unavailable: {status.data.error ?? 'unknown error'}. Sidecars will fall back
									to dotenv files.
								</span>
							)}
						</div>
					</div>
				</div>
			)}

			<div className="space-y-4">
				{KEY_CATALOG.map((cat) => (
					<div key={cat.label} className="space-y-1">
						<h3 className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
							{cat.label}
						</h3>
						<div className="overflow-hidden rounded-md border border-border">
							{cat.keys.map((k, i) => {
								const present = known.has(k.name);
								const isRevealed = revealedKey?.key === k.name;
								return (
									<div
										key={k.name}
										className={cn(
											'flex items-center gap-3 px-3 py-2',
											i > 0 && 'border-t border-border'
										)}
									>
										<KeyRound className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
										<div className="min-w-0 flex-1">
											<div className="flex items-center gap-2">
												<code className="text-xs font-medium">{k.name}</code>
												{k.hint && (
													<span className="truncate text-[11px] text-muted-foreground">
														{k.hint}
													</span>
												)}
											</div>
											<div className="mt-0.5 font-mono text-xs text-muted-foreground">
												{!present ? 'Not set' : isRevealed ? revealedKey.value : '••••••••••••'}
											</div>
										</div>
										<div className="flex shrink-0 items-center gap-1">
											{present && (
												<Button
													variant="ghost"
													size="sm"
													onClick={() => handleReveal(k.name)}
													disabled={!vaultAvailable}
													title={isRevealed ? 'Hide' : 'Reveal (auto-hides in 30s)'}
												>
													{isRevealed ? (
														<EyeOff className="h-3.5 w-3.5" />
													) : (
														<Eye className="h-3.5 w-3.5" />
													)}
												</Button>
											)}
											<Button
												variant="ghost"
												size="sm"
												onClick={() => setEditKey(k.name)}
												disabled={!vaultAvailable}
											>
												{present ? (
													<Pencil className="h-3.5 w-3.5" />
												) : (
													<Plus className="h-3.5 w-3.5" />
												)}
											</Button>
										</div>
									</div>
								);
							})}
						</div>
					</div>
				))}
			</div>

			{editKey && (
				<EditKeyDialog
					keyName={editKey}
					present={known.has(editKey)}
					onClose={() => setEditKey(null)}
				/>
			)}
		</div>
	);
}

export function EditKeyDialog({
	keyName,
	present,
	onClose,
}: {
	keyName: string;
	present: boolean;
	onClose: () => void;
}) {
	const [value, setValue] = useState('');
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const setSecret = useSetSecret();
	const deleteSecret = useDeleteSecret();

	async function handleSave() {
		if (!value) return;
		setBusy(true);
		setError(null);
		try {
			await setSecret.mutateAsync({ key: keyName, value });
			onClose();
		} catch (e) {
			setError((e as Error).message);
		} finally {
			setBusy(false);
		}
	}

	async function handleDelete() {
		const ok = await confirmDialog(`Delete ${keyName} from the vault?`, {
			title: 'Delete key',
			kind: 'warning',
		});
		if (!ok) return;
		setBusy(true);
		setError(null);
		try {
			await deleteSecret.mutateAsync(keyName);
			onClose();
		} catch (e) {
			setError((e as Error).message);
		} finally {
			setBusy(false);
		}
	}

	return (
		<Dialog open onOpenChange={(o) => !o && onClose()}>
			<DialogContent className="sm:max-w-md">
				<DialogHeader>
					<DialogTitle>{present ? 'Edit' : 'Add'} key</DialogTitle>
					<DialogDescription>
						<code>{keyName}</code> — stored encrypted in {`OS keychain → Stronghold`}.
					</DialogDescription>
				</DialogHeader>
				<div className="space-y-2">
					<Input
						type="password"
						placeholder={present ? 'Enter new value to replace' : 'Paste value'}
						value={value}
						onChange={(e) => setValue(e.target.value)}
						autoFocus
					/>
					{error && <p className="text-xs text-red-700">{error}</p>}
				</div>
				<DialogFooter className="flex-row sm:justify-between">
					<div>
						{present && (
							<Button
								variant="outline"
								size="sm"
								onClick={handleDelete}
								disabled={busy}
								className="text-red-700"
							>
								<Trash2 className="mr-1 h-3.5 w-3.5" />
								Delete
							</Button>
						)}
					</div>
					<div className="flex gap-2">
						<Button variant="ghost" size="sm" onClick={onClose} disabled={busy}>
							Cancel
						</Button>
						<Button size="sm" onClick={handleSave} disabled={busy || !value}>
							Save
						</Button>
					</div>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}
