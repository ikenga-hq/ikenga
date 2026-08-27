// Phase 7 — Settings → Secrets.
//
// Scoped vault management. Tabs: Workspace / Project / Pkg. Project + Pkg
// tabs grow a picker dropdown. The list shows bare key names (the scope
// prefix is stripped by the Rust bridge); values are masked with a
// reveal toggle.
//
// Pkg-scoped values aren't dumped into the runtime env-vault file —
// they're consumed by pkg capability resolvers at command-handling time
// via read_secret_scoped. Workspace + active-project secrets are what
// sidecars + per-call MCPs see.

import { useQuery, useQueryClient } from '@tanstack/react-query';
import { createFileRoute } from '@tanstack/react-router';
import {
	Eye,
	EyeOff,
	FolderKanban,
	KeyRound,
	Layers,
	Package,
	Pencil,
	Plus,
	Trash2,
} from 'lucide-react';
import { useCallback, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from 'react';

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
import { cn } from '@/components/ui/utils';
import {
	useDeleteScopedSecret,
	useSetScopedSecret,
	vaultKeysScopedQueryOptions,
	vaultStatusQueryOptions,
} from '@/lib/queries/secrets';
import { useShellStore } from '@/lib/shell/shell-store';
import {
	pkgKernelStatus,
	secretsGetScoped,
	type PkgInstalledSummary,
	type VaultScope,
} from '@/lib/tauri-cmd';

type TabKind = 'workspace' | 'project' | 'pkg';

function SecretsPage() {
	const status = useQuery(vaultStatusQueryOptions());
	const vaultAvailable = status.data?.available ?? false;
	// The headless daemon's store is flat, env-backed and read-only (see
	// src-tauri/src/secrets_env.rs). Reads work; add/edit/delete are hidden
	// rather than offered and then refused by the RPC.
	const vaultWritable = status.data?.writable ?? true;
	const activeProjectId = useShellStore((s) => s.activeProjectId);
	const projects = useShellStore((s) => s.projects);

	const [tab, setTab] = useState<TabKind>('workspace');
	const [projectId, setProjectId] = useState<string>(activeProjectId);
	const [pkgId, setPkgId] = useState<string>('');

	const pkgsQuery = useQuery({
		queryKey: ['pkg-kernel', 'status', 'for-secrets'],
		queryFn: () => pkgKernelStatus(),
		staleTime: 30_000,
	});
	const pkgs: PkgInstalledSummary[] = pkgsQuery.data?.installed ?? [];

	// Pick a default pkg the first time the Pkg tab opens.
	const effectivePkgId = pkgId || pkgs[0]?.id || '';

	const scope: VaultScope =
		tab === 'workspace'
			? { kind: 'workspace' }
			: tab === 'project'
				? { kind: 'project', id: projectId || activeProjectId }
				: { kind: 'pkg', id: effectivePkgId };

	const canQuery = tab !== 'pkg' || !!effectivePkgId;

	const keysQuery = useQuery({
		...vaultKeysScopedQueryOptions(scope),
		enabled: canQuery && vaultAvailable,
	});

	const [editKey, setEditKey] = useState<string | null>(null);
	const [addingNew, setAddingNew] = useState(false);

	return (
		<div className="flex h-full flex-col">
			<div className="flex h-10 shrink-0 items-center gap-3 border-b border-border-soft px-6 text-xs text-muted-foreground">
				<span>
					Settings · <span className="font-semibold text-foreground">Secrets</span>
				</span>
				<StatusChip tone={vaultAvailable ? 'live' : 'danger'} dot className="ml-auto">
					Vault {vaultAvailable ? 'available' : 'unavailable'}
				</StatusChip>
			</div>

			<div className="flex-1 overflow-y-auto px-6 py-6">
				<div className="mx-auto max-w-3xl space-y-4">
					<header className="space-y-1">
						<h2 className="text-base font-semibold">Vault secrets</h2>
						<p className="text-xs text-muted-foreground">
							Stronghold-encrypted, partitioned by scope. Workspace + active-project secrets are
							dumped into the runtime env-vault file that sidecars read via dotenv. Pkg secrets
							resolve at command-handling time inside the kernel.
						</p>
					</header>

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

					<div className="rounded-md border border-border bg-card">
						<div className="flex items-center justify-between border-b border-border px-3 py-2">
							<div className="flex items-center gap-2 text-xs">
								<KeyRound className="h-3.5 w-3.5 text-muted-foreground" />
								<span className="font-medium">
									{keysQuery.isLoading ? 'Loading…' : `${keysQuery.data?.length ?? 0} secrets`}
								</span>
							</div>
							{vaultWritable ? (
								<Button
									variant="ghost"
									size="sm"
									className="h-7 px-2 text-[11px]"
									onClick={() => setAddingNew(true)}
									disabled={!canQuery || !vaultAvailable}
								>
									<Plus className="mr-1 h-3 w-3" /> Add secret
								</Button>
							) : (
								<span className="text-[11px] text-muted-foreground">Read-only</span>
							)}
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
									writable={vaultWritable}
									onEdit={() => setEditKey(k)}
								/>
							))}
						</ul>
					</div>
				</div>
			</div>

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

const TAB_ITEMS: Array<{ kind: TabKind; label: string; icon: ReactNode }> = [
	{ kind: 'workspace', label: 'Workspace', icon: <Layers className="h-3.5 w-3.5" /> },
	{ kind: 'project', label: 'Project', icon: <FolderKanban className="h-3.5 w-3.5" /> },
	{ kind: 'pkg', label: 'Pkg', icon: <Package className="h-3.5 w-3.5" /> },
];

/** Accessible tab strip for the three secret scopes (role=tablist + roving keyboard). */
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
	const [revealed, setRevealed] = useState<string | null>(null);
	const [busy, setBusy] = useState(false);
	const delMut = useDeleteScopedSecret();

	async function reveal() {
		if (revealed !== null) {
			setRevealed(null);
			return;
		}
		setBusy(true);
		try {
			const v = await secretsGetScoped(scope, name);
			setRevealed(v ?? '');
		} finally {
			setBusy(false);
		}
	}

	return (
		<li className="flex items-center gap-2 px-3 py-2 text-xs">
			<span className="truncate font-mono">{name}</span>
			<span className="ml-2 truncate font-mono text-[11px] text-muted-foreground">
				{revealed === null ? '••••••••' : revealed || '(empty)'}
			</span>
			<div className="ml-auto flex items-center gap-1">
				<Button
					variant="ghost"
					size="sm"
					className="h-6 px-2 text-[11px]"
					onClick={reveal}
					disabled={busy}
					aria-label={revealed === null ? `Reveal value for ${name}` : `Hide value for ${name}`}
				>
					{revealed === null ? <Eye className="h-3 w-3" /> : <EyeOff className="h-3 w-3" />}
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
							onClick={() => {
								if (!window.confirm(`Delete secret "${name}"?`)) return;
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
	const [loaded, setLoaded] = useState(editKey === null);
	const setMut = useSetScopedSecret();

	// Prefill value when editing existing key.
	useMemo(() => {
		if (editKey === null) return;
		void secretsGetScoped(scope, editKey).then((v) => {
			setValue(v ?? '');
			setLoaded(true);
		});
		// eslint-disable-next-line react-hooks/exhaustive-deps
	}, [editKey]);

	const canSave = name.trim().length > 0 && value.length > 0 && !setMut.isPending && loaded;

	function handleSave() {
		setMut.mutate({ scope, key: name.trim(), value }, { onSuccess: onClose });
	}

	return (
		<Dialog open onOpenChange={(o) => !o && onClose()}>
			<DialogContent>
				<DialogHeader>
					<DialogTitle>{editKey ? `Edit secret: ${editKey}` : 'Add secret'}</DialogTitle>
					<DialogDescription>
						Scope: <span className="font-mono">{scopeLabel(scope)}</span>. Values are
						Stronghold-encrypted at rest.
					</DialogDescription>
				</DialogHeader>
				<div className="space-y-3">
					<div>
						<label className="text-xs font-medium">Key</label>
						<Input
							value={name}
							onChange={(e) => setName(e.target.value)}
							placeholder="MY_API_KEY"
							disabled={editKey !== null}
							className="font-mono"
						/>
					</div>
					<div>
						<label className="text-xs font-medium">Value</label>
						<Input
							value={value}
							onChange={(e) => setValue(e.target.value)}
							type="password"
							placeholder={editKey ? '(unchanged unless replaced)' : ''}
							className="font-mono"
						/>
					</div>
					{setMut.error && (
						<p role="alert" className="text-xs text-red-700">
							{(setMut.error as Error).message}
						</p>
					)}
				</div>
				<DialogFooter>
					<Button variant="ghost" onClick={onClose}>
						Cancel
					</Button>
					<Button onClick={handleSave} disabled={!canSave}>
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
