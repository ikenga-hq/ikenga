import { useQuery, useQueryClient } from '@tanstack/react-query';
import { createFileRoute } from '@tanstack/react-router';
import { confirm as confirmDialog } from '@/lib/transport/dialog-shim';
import {
	Eye,
	EyeOff,
	FolderKanban,
	KeyRound,
	Layers,
	Lock,
	LockKeyhole,
	LockOpen,
	Package,
	Pencil,
	Plus,
	ShieldAlert,
	Trash2,
} from 'lucide-react';
import {
	type KeyboardEvent,
	useCallback,
	useEffect,
	useMemo,
	useRef,
	useState,
} from 'react';

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
	type VaultScope,
	pkgKernelStatus,
} from '@/lib/tauri-cmd';
import {
	secretsLockStateQueryOptions,
	useDeleteScopedSecret,
	useLockSecrets,
	useSetScopedSecret,
	vaultKeysScopedQueryOptions,
	vaultStatusQueryOptions,
} from '@/lib/queries/secrets';import { useShellStore } from '@/lib/shell/shell-store';
import { useUnlockSheet } from '@/shell/secrets/unlock-sheet';

type TabKind = 'workspace' | 'project' | 'pkg';

/** The only string a "revealed" secret ever shows. The real value is never
 *  fetched into this page — the press-and-hold reveal is local-only. */
const REVEAL_SAMPLE = 'sk-••demo';
const HOLD_DELAY_MS = 350;
const HOLD_REVEAL_MS = 10_000;

function SecretsPage() {
	const status = useQuery(vaultStatusQueryOptions());
	const lock = useQuery({
		...secretsLockStateQueryOptions(),
		refetchInterval: 5_000,
	});
	const vaultAvailable = status.data?.available === true;
	const configured = lock.data?.configured ?? false;
	const locked = lock.data?.locked ?? true;
	const vaultUnlocked = vaultAvailable && configured && !locked;
	const unlockSheet = useUnlockSheet();

	const activeProjectId = useShellStore((s) => s.activeProjectId);
	const projects = useShellStore((s) => s.projects);

	const [tab, setTab] = useState<TabKind>('workspace');
	const [projectId, setProjectId] = useState(activeProjectId);
	const [pkgId, setPkgId] = useState('');
	const pkgsQuery = useQuery({
		queryKey: ['pkg-kernel', 'status', 'for-secrets'],
		queryFn: () => pkgKernelStatus(),
		staleTime: 30_000,
	});
	const pkgs = pkgsQuery.data?.installed ?? [];
	const effectivePkgId = pkgId || pkgs[0]?.id || '';

	const scope: VaultScope = useMemo(() => {
		if (tab === 'workspace') return { kind: 'workspace' };
		if (tab === 'project') return { kind: 'project', id: projectId || activeProjectId || 'default' };
		return { kind: 'pkg', id: effectivePkgId };
	}, [tab, projectId, activeProjectId, effectivePkgId]);

	const canQuery = tab !== 'pkg' || !!effectivePkgId;
	const keysQuery = useQuery({
		...vaultKeysScopedQueryOptions(scope),
		enabled: canQuery && vaultUnlocked,
	});

	const [editKey, setEditKey] = useState<string | null>(null);
	const [addingNew, setAddingNew] = useState(false);

	return (
		<div className="mx-auto w-full max-w-[720px] space-y-5 px-6 py-6">
			<header className="space-y-1">
				<h2
					className="text-2xl font-semibold tracking-tight"
					style={{ fontFamily: 'var(--font-display)' }}
				>
					Vault secrets
				</h2>
				<p className="max-w-prose text-sm leading-relaxed text-muted-foreground">
					Stronghold-encrypted at rest, partitioned by scope. Workspace and active-project secrets
					are dumped into the runtime env-vault file that sidecars read; pkg secrets resolve at
					command-handling time inside the kernel.
				</p>
			</header>

			<VaultLockBanner
				available={vaultAvailable}
				configured={configured}
				unlocked={vaultUnlocked}
				error={status.data?.error ?? null}
			/>

			<ScopeTabList tab={tab} onTabChange={setTab} />

			{tab === 'project' && (
				<div className="flex items-center gap-2 text-xs text-muted-foreground">
					<span>Project:</span>
					<select
						className="rounded-md border border-input bg-background px-2 py-1 text-xs"
						value={projectId || activeProjectId}
						onChange={(e) => setProjectId(e.target.value)}
					>
						{projects
							.filter((p) => !p.archived_at)
							.map((p) => (
								<option key={p.id} value={p.id}>
									{p.display_name} {p.id === activeProjectId ? '(active)' : ''}
								</option>
							))}
					</select>
				</div>
			)}
			{tab === 'pkg' && (
				<div className="flex items-center gap-2 text-xs text-muted-foreground">
					<span>Pkg:</span>
					<select
						className="rounded-md border border-input bg-background px-2 py-1 text-xs"
						value={effectivePkgId}
						onChange={(e) => setPkgId(e.target.value)}
						disabled={pkgs.length === 0}
					>
						{pkgs.length === 0 ? (
							<option value="">(no installed pkgs)</option>
						) : (
							pkgs.map((p) => (
								<option key={p.id} value={p.id}>
									{p.id} {p.project_id ? `· project:${p.project_id}` : '· workspace'}
								</option>
							))
						)}
					</select>
				</div>
			)}

			{!vaultUnlocked && (
				<div className="flex items-center gap-3 rounded-md border border-border bg-card px-4 py-6 text-sm text-muted-foreground">
					<ShieldAlert className="h-4 w-4 shrink-0" />
					<span>
						{!vaultAvailable
							? 'The platform keychain is unavailable, so this vault cannot be probed or written.'
							: configured
								? 'Unlock the vault to list, add or change secrets in any scope.'
								: 'Set a passphrase to start using the vault.'}
					</span>
				</div>
			)}

			{vaultUnlocked && (
				<div className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
					<div className="flex items-center justify-between border-b border-border px-3 py-2">
						<div className="flex items-center gap-2 text-xs">
							<KeyRound className="h-3.5 w-3.5 text-muted-foreground" />
							<span className="font-medium">
								{keysQuery.isLoading ? 'Loading…' : `${keysQuery.data?.length ?? 0} secrets`}
							</span>
						</div>
						<Button
							variant="ghost"
							size="sm"
							className="h-7 px-2 text-[11px]"
							onClick={() => setAddingNew(true)}
							disabled={!canQuery}
						>
							<Plus className="mr-1 h-3 w-3" /> Add secret
						</Button>
					</div>
					{!canQuery && (
						<div className="px-3 py-6 text-center text-xs text-muted-foreground">
							Select a pkg to view its secrets.
						</div>
					)}
					{canQuery && keysQuery.isError && (
						<div className="px-3 py-6 text-center text-xs text-red-700 dark:text-red-400">
							{(keysQuery.error as Error).message}
						</div>
					)}
					{canQuery && keysQuery.data && keysQuery.data.length === 0 && (
						<div className="px-3 py-6 text-center text-xs text-muted-foreground">
							No secrets in this scope yet.
						</div>
					)}
					<ul className="divide-y divide-border">
						{(keysQuery.data ?? []).map((k) => (
							<SecretRow
								key={k}
								scope={scope}
								name={k}
								writable={vaultUnlocked}
								onEdit={() => setEditKey(k)}
							/>
						))}
					</ul>
					<div className="border-t border-border px-4 py-2 text-[11px] italic text-muted-foreground">
						Values are never shown in full here: reveal prints a redacted sample. Used-by and
						last-changed are not tracked yet.
					</div>
				</div>
			)}

			{(addingNew || editKey) && (
				<SecretDialog
					scope={scope}
					editKey={editKey}
					onClose={() => {
						setAddingNew(false);
						setEditKey(null);
					}}
				/>
			)}
		</div>
	);
}

function VaultLockBanner({
	available,
	configured,
	unlocked,
	error,
}: {
	available: boolean;
	configured: boolean;
	unlocked: boolean;
	error: string | null;
}) {
	const unlockSheet = useUnlockSheet();
	const lockMutation = useLockSecrets();
	return (
		<div
			className={cn(
				'flex flex-wrap items-center gap-3 rounded-md border px-3 py-2.5 text-xs',
				!available
					? 'border-red-200 bg-red-50 text-red-900 dark:border-red-900/40 dark:bg-red-950/30 dark:text-red-200'
					: unlocked
						? 'border-emerald-200 bg-emerald-50 text-emerald-900 dark:border-emerald-900/40 dark:bg-emerald-950/30 dark:text-emerald-200'
						: 'border-amber-200 bg-amber-50 text-amber-900 dark:border-amber-900/40 dark:bg-amber-950/30 dark:text-amber-200'
			)}
		>
			{unlocked ? (
				<LockOpen className="h-3.5 w-3.5 shrink-0" />
			) : (
				<ShieldAlert className="h-3.5 w-3.5 shrink-0" />
			)}
			<span className="min-w-0 flex-1">
				{!available
					? `Vault unavailable: ${error ?? 'unknown error'}.`
					: !configured
						? 'No passphrase is set — the vault stores values encrypted only after you set one.'
						: unlocked
							? 'Vault unlocked. It re-locks after 5 minutes idle.'
							: 'Vault locked. Unlock it to read or change secrets.'}
			</span>
			{available && (
				<div className="flex shrink-0 items-center gap-1.5">
					{configured && unlocked && (
						<Button
							variant="outline"
							size="sm"
							className="h-6 gap-1 px-2 text-[11px]"
							disabled={lockMutation.isPending}
							title="Lock the vault now; re-enter the passphrase to read or change secrets"
							onClick={() => lockMutation.mutate(undefined)}
						>
							<Lock className="h-3 w-3" />
							{lockMutation.isPending ? 'Locking…' : 'Lock now'}
						</Button>
					)}
					{configured && !unlocked && (
						<Button
							variant="outline"
							size="sm"
							className="h-6 gap-1 px-2 text-[11px]"
							onClick={() => unlockSheet.open('unlock')}
						>
							<LockOpen className="h-3 w-3" />
							Unlock
						</Button>
					)}
					<Button
						variant="outline"
						size="sm"
						className="h-6 gap-1 px-2 text-[11px]"
						title={available ? undefined : 'The vault is unavailable in this session'}
						onClick={() => unlockSheet.open(configured ? 'unlock' : 'set')}
					>
						<LockKeyhole className="h-3 w-3" />
						{configured ? 'Passphrase…' : 'Set passphrase'}
					</Button>
				</div>
			)}
		</div>
	);
}

const TAB_ITEMS: Array<{ kind: TabKind; label: string; icon: React.ReactNode }> = [
	{ kind: 'workspace', label: 'Workspace', icon: <Layers className="h-3.5 w-3.5" /> },
	{ kind: 'project', label: 'Project', icon: <FolderKanban className="h-3.5 w-3.5" /> },
	{ kind: 'pkg', label: 'Pkg', icon: <Package className="h-3.5 w-3.5" /> },
];

function ScopeTabList({ tab, onTabChange }: { tab: TabKind; onTabChange: (t: TabKind) => void }) {
	const listRef = useRef<HTMLDivElement | null>(null);

	const focusTab = useCallback((kind: TabKind) => {
		requestAnimationFrame(() => {
			listRef.current?.querySelector<HTMLElement>(`[data-tab="${kind}"]`)?.focus();
		});
	}, []);

	const onKeyDown = useCallback(
		(e: KeyboardEvent<HTMLDivElement>) => {
			if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(e.key)) return;
			const pos = TAB_ITEMS.findIndex((i) => i.kind === tab);
			let next = pos;
			if (e.key === 'ArrowRight') next = Math.min(pos + 1, TAB_ITEMS.length - 1);
			else if (e.key === 'ArrowLeft') next = Math.max(pos - 1, 0);
			else if (e.key === 'Home') next = 0;
			else if (e.key === 'End') next = TAB_ITEMS.length - 1;
			const target = TAB_ITEMS[next];
			if (!target || target.kind === tab) {
				e.preventDefault();
				return;
			}
			e.preventDefault();
			onTabChange(target.kind);
			focusTab(target.kind);
		},
		[tab, onTabChange, focusTab]
	);

	return (
		<div
			ref={listRef}
			role="tablist"
			aria-label="Secret scope"
			onKeyDown={onKeyDown}
			className="flex items-center gap-1 rounded-md border border-border bg-card p-1"
		>
			{TAB_ITEMS.map((item) => {
				const active = tab === item.kind;
				return (
					<button
						key={item.kind}
						type="button"
						role="tab"
						aria-selected={active}
						data-tab={item.kind}
						tabIndex={active ? 0 : -1}
						onClick={() => onTabChange(item.kind)}
						className={cn(
							'inline-flex items-center gap-1.5 rounded px-3 py-1 text-xs font-medium transition-colors',
							'outline-none focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary',
							active
								? 'bg-accent text-accent-foreground'
								: 'text-muted-foreground hover:bg-accent/50 hover:text-foreground'
						)}
					>
						{item.icon}
						{item.label}
					</button>
				);
			})}
		</div>
	);
}

function SecretRow({
	scope,
	name,
	writable,
	onEdit,
}: {
	scope: VaultScope;
	name: string;
	writable: boolean;
	onEdit: () => void;
}) {
	const qc = useQueryClient();
	const [revealed, setRevealed] = useState(false);
	const holdTimer = useRef<number | null>(null);
	const hideTimer = useRef<number | null>(null);
	const delMut = useDeleteScopedSecret();

	const stopHold = useCallback(() => {
		if (holdTimer.current !== null) {
			window.clearTimeout(holdTimer.current);
			holdTimer.current = null;
		}
	}, []);

	useEffect(
		() => () => {
			stopHold();
			if (hideTimer.current !== null) window.clearTimeout(hideTimer.current);
		},
		[stopHold]
	);

	function startHold() {
		stopHold();
		holdTimer.current = window.setTimeout(() => {
			setRevealed(true);
			if (hideTimer.current !== null) window.clearTimeout(hideTimer.current);
			hideTimer.current = window.setTimeout(() => setRevealed(false), HOLD_REVEAL_MS);
		}, HOLD_DELAY_MS);
	}

	function hideSoon() {
		stopHold();
		if (hideTimer.current !== null) window.clearTimeout(hideTimer.current);
		hideTimer.current = window.setTimeout(() => setRevealed(false), 2_000);
	}

	return (
		<li className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-3 px-3 py-2 text-xs">
			<div className="min-w-0">
				<div className="truncate font-mono font-medium text-foreground">{name}</div>
				<div className="mt-0.5 flex gap-3 font-mono text-[10px] text-muted-foreground">
					<span>used-by: not tracked yet</span>
					<span>last changed: not tracked yet</span>
				</div>
			</div>
			<div className="flex items-center gap-1">
				<span
					aria-live="polite"
					className="mr-1 truncate font-mono text-[11px] text-muted-foreground"
				>
					{revealed ? REVEAL_SAMPLE : '••••••••'}
				</span>
				<Button
					variant="ghost"
					size="sm"
					className="h-6 px-2 text-[11px]"
					aria-label={`Reveal value for ${name} — press and hold`}
					title="Press and hold to reveal a redacted sample; the value is never shown in full"
					onPointerDown={startHold}
					onPointerUp={stopHold}
					onPointerLeave={stopHold}
					onPointerCancel={stopHold}
					onKeyDown={(e: KeyboardEvent<HTMLButtonElement>) => {
						if (e.key === ' ' || e.key === 'Enter') startHold();
					}}
					onKeyUp={(e: KeyboardEvent<HTMLButtonElement>) => {
						if (e.key === ' ' || e.key === 'Enter') stopHold();
					}}
					onBlur={() => {
						stopHold();
						setRevealed(false);
					}}
				>
					{revealed ? <EyeOff className="h-3 w-3" /> : <Eye className="h-3 w-3" />}
				</Button>
				{writable && (
					<>
						<Button
							variant="ghost"
							size="sm"
							className="h-6 px-2 text-[11px]"
							onClick={onEdit}
							aria-label={`Edit secret ${name}`}
						>
							<Pencil className="h-3 w-3" />
						</Button>
						<Button
							variant="ghost"
							size="sm"
							className="h-6 px-2 text-[11px] text-muted-foreground hover:text-red-700"
							onClick={async () => {
								const ok = await confirmDialog(
									`Delete "${name}" from the ${scope.kind} scope? Anything using it will fail at its next run — the value cannot be recovered.`,
									{ title: 'Delete secret', kind: 'warning' }
								);
								if (!ok) return;
								delMut.mutate(
									{ scope, key: name },
									{ onSuccess: () => qc.invalidateQueries({ queryKey: ['secrets'] }) }
								);
							}}
							disabled={delMut.isPending}
							aria-label={`Delete secret ${name}`}
						>
							<Trash2 className="h-3 w-3" />
						</Button>
					</>
				)}
			</div>
		</li>
	);
}

function SecretDialog({
	scope,
	editKey,
	onClose,
}: {
	scope: VaultScope;
	editKey: string | null;
	onClose: () => void;
}) {
	const [name, setName] = useState(editKey ?? '');
	const [value, setValue] = useState('');
	const setMut = useSetScopedSecret();
	const canSave = name.trim().length > 0 && value.length > 0 && !setMut.isPending;

	return (
		<Dialog open onOpenChange={(o) => !o && onClose()}>
			<DialogContent>
				<DialogHeader>
					<DialogTitle>{editKey ? `Edit secret: ${editKey}` : 'Add secret'}</DialogTitle>
					<DialogDescription>
						Scope: <span className="font-mono">{scopeLabel(scope)}</span>. Values are
						Stronghold-encrypted at rest and never written to a log. For an existing secret the
						field starts empty; type a value to replace it.
					</DialogDescription>
				</DialogHeader>
				<div className="space-y-3">
					<div>
						<label className="text-xs font-medium" htmlFor="secret-key-input">
							Key
						</label>
						<Input
							id="secret-key-input"
							value={name}
							onChange={(e) => setName(e.target.value)}
							placeholder="MY_API_KEY"
							disabled={editKey !== null}
							className="font-mono"
						/>
					</div>
					<div>
						<label className="text-xs font-medium" htmlFor="secret-value-input">
							Value
						</label>
						<Input
							id="secret-value-input"
							value={value}
							onChange={(e) => setValue(e.target.value)}
							type="password"
							placeholder={editKey ? '(unchanged unless replaced)' : ''}
							className="font-mono"
						/>
					</div>
					{setMut.error && (
						<p role="alert" className="text-xs text-red-700">
							{setMut.error.message}
						</p>
					)}
				</div>
				<DialogFooter>
					<Button variant="ghost" onClick={onClose}>
						Cancel
					</Button>
					<Button
						onClick={() =>
							setMut.mutate({ scope, key: name.trim(), value }, { onSuccess: onClose })
						}
						disabled={!canSave}
					>
						Save
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}

function scopeLabel(scope: VaultScope): string {
	if (scope.kind === 'workspace') return 'workspace';
	return `${scope.kind}:${scope.id}`;
}

export const Route = createFileRoute('/settings/secrets')({
	component: SecretsPage,
});
