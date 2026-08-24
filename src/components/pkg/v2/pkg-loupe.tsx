// Pkg loupe — right-sliding sheet with tabbed detail.
// Used in place of TrustReviewDialog + ViolationsReviewDialog from the
// current /packages page, and as the install sheet (re-uses the same
// chrome with a registry-specific body).

import {
	ArrowUp,
	Ban,
	ChevronRight,
	Copy,
	ExternalLink,
	Plug,
	Plus,
	Power,
	Shield,
	X,
} from 'lucide-react';
import { useCallback, useRef, useState } from 'react';
import { Button } from '@/components/ui/button';
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from '@/components/ui/collapsible';
import { Sheet, SheetContent } from '@/components/ui/sheet';
import { cn } from '@/components/ui/utils';
import type { PkgRowV2 } from '@/lib/pkgs/use-derived';
import {
	pkgKernelStatus,
	pkgSetEnabled,
	pkgSettingsGet,
	pkgSettingsSet,
	pkgTrustGrant,
	pkgTrustRevoke,
	type PkgSettingsField,
	type PkgSettingsSnapshot,
} from '@/lib/tauri-cmd';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { Dot, OriginChip, StateChip, TrustChip, UpdateChip, ViolationChip } from './atoms';
import { PkgScreenshotCarousel, PkgScreenshotHero } from './pkg-screenshots';
import { classifyScope, riskColor } from './scope-classifier';

export type LoupeTab = 'overview' | 'permissions' | 'trust' | 'settings' | 'manifest';

export interface PkgLoupeProps {
	row: PkgRowV2 | null;
	tab?: LoupeTab;
	open: boolean;
	onOpenChange: (open: boolean) => void;
	onInstall?: (row: PkgRowV2) => void;
	onUpdate?: (row: PkgRowV2) => void;
	onUninstall?: (row: PkgRowV2) => void;
}

export function PkgLoupe({
	row,
	tab: initialTab = 'overview',
	open,
	onOpenChange,
	onInstall,
	onUpdate,
	onUninstall,
}: PkgLoupeProps) {
	const [tab, setTab] = useState<LoupeTab>(initialTab);
	return (
		<Sheet open={open} onOpenChange={onOpenChange}>
			<SheetContent
				side="right"
				className="flex w-full flex-col gap-0 border-l border-border !bg-background p-0 text-foreground sm:max-w-[560px]"
			>
				{row ? (
					<>
						<PkgScreenshotHero row={row} />
						<LoupeHead row={row} onClose={() => onOpenChange(false)} />
						<LoupeTabs row={row} tab={tab} onTab={setTab} />
						<div
							role="tabpanel"
							id={`loupe-panel-${tab}`}
							aria-labelledby={`loupe-tab-${tab}`}
							className="flex-1 overflow-y-auto p-5"
						>
							{tab === 'overview' && <TabOverview row={row} />}
							{tab === 'permissions' && <TabPermissions row={row} />}
							{tab === 'trust' && <TabTrust row={row} />}
							{tab === 'settings' && <TabSettings row={row} />}
							{tab === 'manifest' && <TabManifest row={row} />}
						</div>
						<LoupeFoot
							row={row}
							onClose={() => onOpenChange(false)}
							onInstall={onInstall}
							onUpdate={onUpdate}
							onUninstall={onUninstall}
						/>
					</>
				) : null}
			</SheetContent>
		</Sheet>
	);
}

/* ───── Head + Tabs + Foot ───── */

function LoupeHead({ row, onClose }: { row: PkgRowV2; onClose: () => void }) {
	return (
		<div className="flex items-center gap-3 border-b border-border bg-muted/40 p-4">
			<div className="min-w-0">
				<div className="flex items-baseline gap-2">
					<span className="font-display text-lg font-medium tracking-tight">{row.name}</span>
					<span className="rounded-sm border border-border bg-background px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground">
						v{row.version}
					</span>
				</div>
				<div className="truncate font-mono text-[11px] text-muted-foreground/70">{row.id}</div>
			</div>
			<button
				type="button"
				onClick={onClose}
				className="ml-auto rounded p-1.5 text-muted-foreground hover:bg-accent hover:text-foreground"
				aria-label="Close"
			>
				<X className="h-4 w-4" />
			</button>
		</div>
	);
}

// Local horizontal roving-tablist keyboard handler for the bespoke tab bars in
// this sheet (loupe detail + generic install). Mirrors the convention used by
// the shell's <TabStrip> (data-tab-index + [role="tab"]) but stays local so we
// don't widen that file's export surface for two screen-specific strips.
// ←/→ switch, Home/End jump; DOM focus moves to the destination tab next frame.
function useRovingTabs(count: number, onSwitch: (idx: number) => void) {
	const containerRef = useRef<HTMLDivElement | null>(null);
	const focusTab = useCallback((i: number) => {
		requestAnimationFrame(() => {
			containerRef.current
				?.querySelector<HTMLElement>(`[role="tab"][data-tab-index="${i}"]`)
				?.focus();
		});
	}, []);
	const onKeyDown = useCallback(
		(e: React.KeyboardEvent<HTMLDivElement>) => {
			const tabEl = (e.target as HTMLElement).closest<HTMLElement>('[role="tab"]');
			if (!tabEl || !containerRef.current?.contains(tabEl)) return;
			const idx = Number(tabEl.dataset.tabIndex);
			if (Number.isNaN(idx)) return;
			if (e.key === 'ArrowLeft' || e.key === 'ArrowRight') {
				const to = idx + (e.key === 'ArrowRight' ? 1 : -1);
				if (to < 0 || to >= count) return;
				e.preventDefault();
				onSwitch(to);
				focusTab(to);
			} else if (e.key === 'Home') {
				e.preventDefault();
				onSwitch(0);
				focusTab(0);
			} else if (e.key === 'End') {
				e.preventDefault();
				onSwitch(count - 1);
				focusTab(count - 1);
			}
		},
		[count, onSwitch, focusTab]
	);
	return { containerRef, onKeyDown };
}

function LoupeTabs({
	row,
	tab,
	onTab,
}: {
	row: PkgRowV2;
	tab: LoupeTab;
	onTab: (t: LoupeTab) => void;
}) {
	const items: Array<{ id: LoupeTab; label: string; count?: number; pending?: boolean }> = [
		{ id: 'overview', label: 'Overview' },
		{ id: 'permissions', label: 'Permissions', count: row.scopes.length },
		{ id: 'trust', label: 'Trust', pending: row.trust?.state === 'needs_approval' },
		{ id: 'settings', label: 'Settings' },
		{ id: 'manifest', label: 'Manifest' },
	];
	const { containerRef, onKeyDown } = useRovingTabs(items.length, (i) => onTab(items[i].id));
	return (
		<div
			ref={containerRef}
			role="tablist"
			aria-label="Package detail"
			onKeyDown={onKeyDown}
			className="flex gap-0 border-b border-border bg-muted/40 px-5"
		>
			{items.map((it, i) => {
				const selected = tab === it.id;
				return (
					<button
						key={it.id}
						type="button"
						role="tab"
						id={`loupe-tab-${it.id}`}
						aria-selected={selected}
						aria-controls={`loupe-panel-${it.id}`}
						data-tab-index={i}
						tabIndex={selected ? 0 : -1}
						onClick={() => onTab(it.id)}
						className={cn(
							'border-b-2 px-3 py-2.5 text-sm font-medium transition-colors',
							'outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset',
							selected
								? 'border-primary text-foreground'
								: 'border-transparent text-muted-foreground hover:text-foreground'
						)}
					>
						{it.label}
						{typeof it.count === 'number' && (
							<span className="ml-1 font-mono text-[10px] text-muted-foreground/70">
								{it.count}
							</span>
						)}
						{it.pending && <span className="ml-1 text-[11px] text-destructive">· pending</span>}
					</button>
				);
			})}
		</div>
	);
}

function LoupeFoot({
	row,
	onClose,
	onInstall,
	onUpdate,
	onUninstall,
}: {
	row: PkgRowV2;
	onClose: () => void;
	onInstall?: (row: PkgRowV2) => void;
	onUpdate?: (row: PkgRowV2) => void;
	onUninstall?: (row: PkgRowV2) => void;
}) {
	const qc = useQueryClient();
	const grantMut = useMutation({
		mutationFn: () => pkgTrustGrant(row.id, row.version),
		onSuccess: () => qc.refetchQueries({ queryKey: ['pkg'] }),
	});
	const enableMut = useMutation({
		mutationFn: () => pkgSetEnabled(row.id, !row.enabled),
		onSuccess: () => qc.refetchQueries({ queryKey: ['pkg'] }),
	});

	return (
		<div className="flex justify-end gap-2 border-t border-border bg-muted/30 px-5 py-3">
			{row.origin === 'registry' ? (
				<>
					<Button size="sm" variant="ghost" onClick={onClose}>
						Close
					</Button>
					<Button size="sm" onClick={() => onInstall?.(row)}>
						<Plus className="mr-1.5 h-3.5 w-3.5" />
						Install
					</Button>
				</>
			) : row.latest && row.latest !== row.version ? (
				<>
					<Button size="sm" variant="ghost" onClick={onClose}>
						Close
					</Button>
					<Button
						size="sm"
						className="bg-[var(--achievement)] text-[var(--achievement-soft)] hover:bg-[var(--achievement)]/90"
						onClick={() => onUpdate?.(row)}
					>
						<ArrowUp className="mr-1.5 h-3.5 w-3.5" />
						Update to v{row.latest}
					</Button>
				</>
			) : row.trust?.state === 'needs_approval' ? (
				<>
					<Button size="sm" variant="ghost" onClick={onClose}>
						Cancel
					</Button>
					<Button
						size="sm"
						variant="outline"
						className="border-destructive/40 text-destructive hover:bg-destructive/10"
						disabled={grantMut.isPending}
						onClick={() => grantMut.mutate()}
					>
						<Shield className="mr-1.5 h-3.5 w-3.5" />
						{grantMut.isPending ? 'Approving…' : `Approve v${row.version}`}
					</Button>
				</>
			) : (
				<>
					{row.origin !== 'builtin' && onUninstall && (
						<Button
							size="sm"
							variant="ghost"
							className="text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
							onClick={() => onUninstall(row)}
						>
							<Ban className="mr-1.5 h-3.5 w-3.5" />
							Uninstall
						</Button>
					)}
					<Button
						size="sm"
						variant="outline"
						disabled={enableMut.isPending}
						onClick={() => enableMut.mutate()}
					>
						<Power className="mr-1.5 h-3.5 w-3.5" />
						{row.enabled ? 'Disable' : 'Enable'}
					</Button>
				</>
			)}
		</div>
	);
}

/* ───── Tab bodies ───── */

function SectionLabel({ children }: { children: React.ReactNode }) {
	return (
		<div className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground/70">
			{children}
		</div>
	);
}

function TabOverview({ row }: { row: PkgRowV2 }) {
	return (
		<div className="space-y-5">
			{row.trust?.state === 'needs_approval' && <TrustCallout row={row} />}
			{row.violations.length > 0 && <ViolationCallout row={row} />}
			<section className="space-y-2">
				<SectionLabel>about</SectionLabel>
				<p className="text-sm leading-relaxed text-foreground">{row.desc || '—'}</p>
				<div className="flex flex-wrap gap-1.5">
					<OriginChip origin={row.origin} />
					<StateChip state={row.state} />
					<UpdateChip row={row} />
					<TrustChip row={row} />
					<ViolationChip row={row} />
				</div>
			</section>
			<PkgScreenshotCarousel row={row} />
			<EngineInstalls row={row} />
			<section className="space-y-2">
				<SectionLabel>manifest summary</SectionLabel>
				<MetaGrid
					rows={[
						['kind', <code key="k">{row.kind}</code>],
						[
							'routes',
							row.routes.length ? (
								<div className="flex flex-wrap gap-1">
									{row.routes.map((r) => (
										<code key={r}>{r}</code>
									))}
								</div>
							) : (
								<span className="text-muted-foreground/70">none</span>
							),
						],
						[
							'sidecars',
							row.sidecars.length ? (
								<div className="flex flex-wrap gap-1">
									{row.sidecars.map((s) => (
										<code key={s}>{s}</code>
									))}
								</div>
							) : (
								<span className="text-muted-foreground/70">none</span>
							),
						],
						[
							'install',
							<code key="i" className="break-all">
								{row.installPath}
							</code>,
						],
						...(row.installedAt
							? ([['added', new Date(row.installedAt * 1000).toISOString().slice(0, 10)]] as Array<
									[string, React.ReactNode]
								>)
							: []),
					]}
				/>
			</section>
		</div>
	);
}

function TabPermissions({ row }: { row: PkgRowV2 }) {
	if (!row.scopes.length) {
		return (
			<p className="text-sm text-muted-foreground">This pkg requests no sensitive permissions.</p>
		);
	}
	const high = row.scopes.filter((s) => classifyScope(s).risk === 'high').length;
	return (
		<div className="space-y-5">
			<section className="space-y-2">
				<SectionLabel>
					declared scopes · {row.scopes.length}
					{high > 0 && ` · ${high} high-risk`}
				</SectionLabel>
				<div className="space-y-1.5">
					{row.scopes.map((s) => {
						const c = classifyScope(s);
						return (
							<div
								key={s}
								className="grid grid-cols-[8px_1fr_auto] items-center gap-3 rounded-sm border border-border bg-background px-3 py-2"
							>
								<span
									className={cn(
										'h-1.5 w-1.5 rounded-full',
										riskColor(c.risk).replace('text-', 'bg-')
									)}
								/>
								<div>
									<div className="font-mono text-xs text-foreground">{s}</div>
									<div className="text-[11px] text-muted-foreground">{c.label}</div>
								</div>
								<span
									className={cn(
										'rounded-sm border px-1.5 py-0.5 font-mono text-[10px] uppercase',
										c.risk === 'high'
											? 'border-destructive/40 bg-destructive/10 text-destructive'
											: c.risk === 'med'
												? 'border-[var(--achievement)]/40 bg-[var(--achievement)]/10 text-[var(--achievement)]'
												: 'border-[var(--live)]/30 bg-[var(--live)]/10 text-[var(--live)]'
									)}
								>
									risk: {c.risk}
								</span>
							</div>
						);
					})}
				</div>
			</section>
			{row.violations.length > 0 && (
				<section className="space-y-2">
					<SectionLabel>recent violations</SectionLabel>
					{row.violations.map((v) => (
						<div
							key={v.id}
							className="space-y-1 rounded-sm border border-destructive/30 bg-destructive/5 px-3 py-2"
						>
							<div className="text-sm font-medium text-destructive">
								Denied:{' '}
								<code className="rounded-sm bg-background px-1 py-0.5 text-foreground">
									{v.scope_kind}
								</code>
							</div>
							<div className="text-[11px] text-muted-foreground">
								attempted{' '}
								<code className="rounded-sm bg-background px-1 text-foreground">{v.attempted}</code>{' '}
								· declared{' '}
								<code className="rounded-sm bg-background px-1 text-foreground">{v.declared}</code>{' '}
								· {new Date(v.occurred_at * 1000).toISOString().slice(0, 19).replace('T', ' ')}
							</div>
						</div>
					))}
				</section>
			)}
		</div>
	);
}

function TabTrust({ row }: { row: PkgRowV2 }) {
	const t = row.trust;
	const qc = useQueryClient();
	const revokeMut = useMutation({
		mutationFn: () => pkgTrustRevoke(row.id),
		onSuccess: () => qc.refetchQueries({ queryKey: ['pkg'] }),
	});

	let label = 'Unknown';
	let sub = 'Not yet installed.';
	let tone: 'live' | 'warn' | 'danger' | 'muted' = 'muted';
	if (t?.state === 'auto_trusted') {
		label = 'Auto-trusted';
		sub = `Built-in pkg in the com.ikenga.* namespace. Cannot be revoked.`;
		tone = 'live';
	} else if (t?.state === 'auto_granted') {
		label = 'Auto-granted';
		sub = `Skill-only pkg with no sensitive perms. Auto-approved on install.`;
		tone = 'live';
	} else if (t?.state === 'granted') {
		label = 'Trusted';
		sub = `You approved this pkg on install. Permissions are honored.`;
		tone = 'live';
	} else if (t?.state === 'needs_approval') {
		label = 'Pending review';
		const change = t.change_reason;
		if (change?.kind === 'permissions_changed') {
			sub = `Bumped from v${change.prior_version}. Added: ${change.added.join(', ') || '(none)'}. Re-approve before enabling.`;
		} else if (change?.kind === 'revoked') {
			sub = `Trust was revoked. Re-approve to grant declared permissions again.`;
		} else {
			sub = `A change to this pkg requires your re-approval.`;
		}
		tone = 'danger';
	}

	return (
		<div className="space-y-5">
			<section className="space-y-2">
				<SectionLabel>current trust state</SectionLabel>
				<div className="space-y-2 rounded-sm border border-border bg-background p-4">
					<div className="flex items-center gap-2 font-display text-lg font-medium">
						<Dot tone={tone} />
						{label}
					</div>
					<p className="text-sm leading-relaxed text-muted-foreground">{sub}</p>
					{t?.state === 'granted' && (
						<Button
							size="sm"
							variant="ghost"
							className="mt-1 text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
							disabled={revokeMut.isPending}
							onClick={() => revokeMut.mutate()}
						>
							<Ban className="mr-1.5 h-3.5 w-3.5" />
							Revoke trust
						</Button>
					)}
				</div>
			</section>
			<section className="space-y-2">
				<SectionLabel>trust log</SectionLabel>
				<div className="space-y-1.5 font-mono text-[11.5px]">
					{row.installedAt && (
						<div className="text-muted-foreground">
							<Dot tone="live" />{' '}
							<span className="ml-2">
								{new Date(row.installedAt * 1000).toISOString().slice(0, 10)} · v{row.version}{' '}
								installed
							</span>
						</div>
					)}
					{t?.last_granted_at_ms && (
						<div className="text-muted-foreground">
							<Dot tone="live" />{' '}
							<span className="ml-2">
								{new Date(t.last_granted_at_ms).toISOString().slice(0, 10)} · trust granted
							</span>
						</div>
					)}
					{row.violations.map((v) => (
						<div key={v.id} className="text-destructive">
							<Dot tone="danger" />{' '}
							<span className="ml-2">
								{new Date(v.occurred_at * 1000).toISOString().slice(0, 10)} · attempted{' '}
								<code>{v.scope_kind}</code> — denied
							</span>
						</div>
					))}
				</div>
			</section>
		</div>
	);
}

function TabSettings({ row }: { row: PkgRowV2 }) {
	const qc = useQueryClient();
	const settings = useQuery({
		enabled: row.origin !== 'registry',
		queryKey: ['pkg', 'settings', row.id],
		queryFn: () => pkgSettingsGet(row.id),
	});

	if (row.origin === 'registry') {
		return <p className="text-sm text-muted-foreground">Install the pkg to configure it.</p>;
	}
	if (settings.isLoading) {
		return <p className="text-sm text-muted-foreground">Loading settings…</p>;
	}
	if (settings.error) {
		return (
			<p className="text-sm text-destructive">
				Failed to load settings: {(settings.error as Error).message}
			</p>
		);
	}
	const snapshot: PkgSettingsSnapshot | undefined = settings.data;
	const schema = (snapshot?.schema as PkgSettingsField[] | undefined) ?? [];
	if (!schema.length) {
		return (
			<p className="text-sm text-muted-foreground">This pkg has no user-configurable settings.</p>
		);
	}
	return (
		<div className="space-y-5">
			{schema.map((field) => (
				<SettingField
					key={field.key}
					field={field}
					value={(snapshot?.values as Record<string, unknown> | undefined)?.[field.key]}
					onChange={async (value) => {
						await pkgSettingsSet(row.id, field.key, value);
						await qc.refetchQueries({ queryKey: ['pkg', 'settings', row.id] });
					}}
				/>
			))}
		</div>
	);
}

function SettingField({
	field,
	value,
	onChange,
}: {
	field: PkgSettingsField;
	value: unknown;
	onChange: (v: unknown) => Promise<void> | void;
}) {
	const current = value ?? field.default ?? '';
	return (
		<section className="space-y-1.5">
			<div className="flex items-baseline justify-between gap-2">
				<div className="text-sm font-medium text-foreground">{field.label ?? field.key}</div>
				<code className="font-mono text-[10px] text-muted-foreground/70">{field.key}</code>
			</div>
			{field.type === 'bool' ? (
				<button
					type="button"
					onClick={() => onChange(!current)}
					className={cn(
						'inline-flex h-7 items-center rounded-sm border px-2 font-mono text-xs',
						current
							? 'border-[var(--live)]/40 bg-[var(--live)]/15 text-[var(--live)]'
							: 'border-border'
					)}
				>
					{current ? '☑ on' : '☐ off'}
				</button>
			) : (
				<input
					defaultValue={String(current)}
					onBlur={(e) => onChange(e.target.value)}
					className="w-full rounded-sm border border-border bg-background px-2.5 py-1.5 font-mono text-sm outline-none focus:border-primary"
				/>
			)}
			{field.description && (
				<p className="text-[11px] text-muted-foreground">{field.description}</p>
			)}
		</section>
	);
}

function TabManifest({ row }: { row: PkgRowV2 }) {
	const json = JSON.stringify(row.manifest ?? { id: row.id, version: row.version }, null, 2);
	return (
		<section className="space-y-2">
			<SectionLabel>
				manifest.json · {row.id}@{row.version}
			</SectionLabel>
			<pre className="overflow-x-auto rounded-sm border border-border bg-background p-3 font-mono text-[11.5px] leading-relaxed text-foreground">
				{json}
			</pre>
			<div className="flex gap-1.5">
				<Button
					size="sm"
					variant="outline"
					className="h-7"
					onClick={() => void navigator.clipboard?.writeText(json)}
				>
					<Copy className="mr-1.5 h-3.5 w-3.5" />
					Copy
				</Button>
				<Button size="sm" variant="ghost" className="h-7 text-muted-foreground">
					<ExternalLink className="mr-1.5 h-3.5 w-3.5" />
					Open in editor
				</Button>
			</div>
		</section>
	);
}

/* ───── Engine installs (ADR-012 §10 phase 5) ───── */

// Local types for the registry snapshots we read out of pkgKernelStatus().
// PkgKernelStatus['registries'] is typed as `Record<string, unknown>` at the
// tauri-cmd boundary; narrow it inline here rather than widening the global
// type. Mirrors the Rust shapes in `pkg/registries/mcp.rs` +
// `pkg/registries/engine_assets.rs` (Track C/D).
interface InstallReportSnap {
	wrote: string[];
	skipped: string[];
	warnings: string[];
}

interface McpRegistrySnap {
	count?: number;
	entries?: Array<{ pkg_id: string; name: string; key: string }>;
	config_path?: string;
	/** Outer key = pkg_id, inner key = engine_id. */
	adapter_reports?: Record<string, Record<string, InstallReportSnap>>;
}

interface EngineAssetEntrySnap {
	pkg_id: string;
	engine_id: string;
	kind: string;
	source: string;
	target: string;
}

interface EngineAssetsRegistrySnap {
	count?: number;
	entries?: EngineAssetEntrySnap[];
	/** ADR-012 Track P: per-pkg, per-engine fan-out reports for asset
	 * folders (skills/commands/agents). Outer key = pkg_id, inner key =
	 * engine_id. Mirrors the same field on `McpRegistrySnap`. */
	adapter_reports?: Record<string, Record<string, InstallReportSnap>>;
}

interface EngineInstallRow {
	engineId: string;
	/** MCP adapter report (Track D). */
	mcpReport: InstallReportSnap | null;
	/** Asset adapter report (Track P). */
	assetReport: InstallReportSnap | null;
	assets: EngineAssetEntrySnap[];
}

const ENGINE_DISPLAY_NAMES: Record<string, string> = {
	'claude-code': 'Claude Code',
	gemini: 'Gemini CLI',
	codex: 'Codex CLI',
	'cursor-agent': 'Cursor Agent',
	opencode: 'OpenCode',
	pi: 'Pi Coding Agent',
};

function engineDisplayName(id: string): string {
	return ENGINE_DISPLAY_NAMES[id] ?? id;
}

function EngineInstalls({ row }: { row: PkgRowV2 }) {
	// Registry rows are pre-install — there's nothing materialized yet.
	const status = useQuery({
		enabled: row.origin !== 'registry',
		queryKey: ['pkg', 'kernel-status'],
		queryFn: pkgKernelStatus,
		refetchOnWindowFocus: false,
	});

	if (row.origin === 'registry') return null;
	if (!status.data) return null;

	const registries = status.data.registries as Record<string, unknown> | undefined;
	const mcp = (registries?.mcp ?? undefined) as McpRegistrySnap | undefined;
	const engineAssets = (registries?.engine_assets ?? undefined) as
		| EngineAssetsRegistrySnap
		| undefined;

	const mcpAdapterReports = mcp?.adapter_reports?.[row.id] ?? {};
	const assetAdapterReports = engineAssets?.adapter_reports?.[row.id] ?? {};
	const assetEntries = (engineAssets?.entries ?? []).filter((e) => e.pkg_id === row.id);

	// Build a stable ordered set of engines that touched this pkg. Merge
	// IDs from both the MCP and asset report buckets, plus any asset
	// entries (defensive — an entry without a report would still mean the
	// engine touched the pkg).
	const engineSet = new Set<string>();
	for (const id of Object.keys(mcpAdapterReports)) engineSet.add(id);
	for (const id of Object.keys(assetAdapterReports)) engineSet.add(id);
	for (const e of assetEntries) engineSet.add(e.engine_id);
	if (engineSet.size === 0) return null;

	// Keep claude-code first if present, then alphabetical.
	const engineIds = Array.from(engineSet).sort((a, b) => {
		if (a === 'claude-code') return -1;
		if (b === 'claude-code') return 1;
		return a.localeCompare(b);
	});

	const rows: EngineInstallRow[] = engineIds.map((engineId) => ({
		engineId,
		mcpReport: mcpAdapterReports[engineId] ?? null,
		assetReport: assetAdapterReports[engineId] ?? null,
		assets: assetEntries.filter((e) => e.engine_id === engineId),
	}));

	return (
		<section className="space-y-2">
			<SectionLabel>
				<span className="inline-flex items-center gap-1.5">
					<Plug className="h-3 w-3" />
					engine installs
				</span>
			</SectionLabel>
			<div className="space-y-1.5">
				{rows.map((r) => (
					<EngineInstallCard key={r.engineId} row={r} />
				))}
			</div>
		</section>
	);
}

function EngineInstallCard({ row }: { row: EngineInstallRow }) {
	const [open, setOpen] = useState(false);
	const mcpWroteCount = row.mcpReport?.wrote.length ?? 0;
	const mcpSkippedCount = row.mcpReport?.skipped.length ?? 0;
	const assetWroteCount = row.assetReport?.wrote.length ?? 0;
	const assetSkippedCount = row.assetReport?.skipped.length ?? 0;
	// Warnings come from both fan-out paths — concat so the badge counts
	// MCP + asset warnings together and the detail group shows both lists.
	const warnings = [...(row.mcpReport?.warnings ?? []), ...(row.assetReport?.warnings ?? [])];
	const assetCount = row.assets.length;
	const hasDetails =
		mcpWroteCount +
			mcpSkippedCount +
			assetWroteCount +
			assetSkippedCount +
			warnings.length +
			assetCount >
		0;

	// Summary line. MCP fan-out + asset fan-out count toward the same
	// "wrote" notion from the user's perspective. Render them as separate
	// phrases so it's clear what each number refers to. The MCP phrase is
	// kept first for backwards compatibility with the existing test that
	// asserts on "Wrote N MCP servers".
	const phrases: string[] = [];
	if (row.mcpReport) {
		phrases.push(`Wrote ${mcpWroteCount} MCP server${mcpWroteCount === 1 ? '' : 's'}`);
		if (mcpSkippedCount > 0) {
			phrases.push(`skipped ${mcpSkippedCount} (idempotent)`);
		}
	}
	if (row.assetReport && assetWroteCount > 0) {
		phrases.push(`Wrote ${assetWroteCount} asset link${assetWroteCount === 1 ? '' : 's'}`);
	} else if (assetCount > 0) {
		// Fallback when entries exist but the report is missing (e.g.
		// boot-replay before adapter_reports re-populates).
		phrases.push(`${assetCount} asset link${assetCount === 1 ? '' : 's'}`);
	}
	const summary = phrases.join(' · ') || 'No changes';

	return (
		<Collapsible open={open} onOpenChange={setOpen}>
			<div className="rounded-sm border border-border bg-background">
				<CollapsibleTrigger
					disabled={!hasDetails}
					className={cn(
						'flex w-full items-center gap-2 px-3 py-2 text-left',
						hasDetails && 'hover:bg-accent/40'
					)}
				>
					<ChevronRight
						className={cn(
							'h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform',
							open && 'rotate-90',
							!hasDetails && 'opacity-30'
						)}
					/>
					<div className="min-w-0 flex-1">
						<div className="flex items-center gap-2">
							<span className="text-sm font-medium text-foreground">
								{engineDisplayName(row.engineId)}
							</span>
							<code className="rounded-sm border border-border bg-muted/40 px-1 py-0.5 font-mono text-[10px] text-muted-foreground">
								{row.engineId}
							</code>
							{warnings.length > 0 && (
								<span className="rounded-sm border border-[var(--achievement)]/40 bg-[var(--achievement)]/10 px-1.5 py-0.5 font-mono text-[10px] uppercase text-[var(--achievement)]">
									{warnings.length} warning{warnings.length === 1 ? '' : 's'}
								</span>
							)}
						</div>
						<div className="text-[11px] text-muted-foreground">{summary}</div>
					</div>
				</CollapsibleTrigger>
				<CollapsibleContent>
					<div className="space-y-3 border-t border-border bg-muted/20 px-3 py-2.5">
						{assetCount > 0 && (
							<EngineDetailGroup label="Assets">
								{row.assets.map((a) => (
									<li
										key={`${a.kind}-${a.target}`}
										className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5 font-mono text-[11px]"
									>
										<span className="rounded-sm border border-border bg-background px-1 py-0.5 text-foreground">
											{a.kind}
										</span>
										<span className="break-all text-muted-foreground">{a.target}</span>
									</li>
								))}
							</EngineDetailGroup>
						)}
						{(mcpWroteCount > 0 || assetWroteCount > 0) && (
							<EngineDetailGroup label="Wrote">
								{row.mcpReport?.wrote.map((w) => (
									<li
										key={`wrote-mcp-${w}`}
										className="break-all font-mono text-[11px] text-foreground"
									>
										{w}
									</li>
								))}
								{row.assetReport?.wrote.map((w) => (
									<li
										key={`wrote-asset-${w}`}
										className="break-all font-mono text-[11px] text-foreground"
									>
										{w}
									</li>
								))}
							</EngineDetailGroup>
						)}
						{(mcpSkippedCount > 0 || assetSkippedCount > 0) && (
							<EngineDetailGroup label="Skipped">
								{row.mcpReport?.skipped.map((s) => (
									<li
										key={`skipped-mcp-${s}`}
										className="break-all font-mono text-[11px] text-muted-foreground"
									>
										{s}
									</li>
								))}
								{row.assetReport?.skipped.map((s) => (
									<li
										key={`skipped-asset-${s}`}
										className="break-all font-mono text-[11px] text-muted-foreground"
									>
										{s}
									</li>
								))}
							</EngineDetailGroup>
						)}
						{warnings.length > 0 && (
							<EngineDetailGroup label="Warnings" tone="warn">
								{warnings.map((w) => (
									<li
										key={`warn-${w}`}
										className="text-[11.5px] leading-relaxed text-[var(--achievement)]"
									>
										{w}
									</li>
								))}
							</EngineDetailGroup>
						)}
					</div>
				</CollapsibleContent>
			</div>
		</Collapsible>
	);
}

function EngineDetailGroup({
	label,
	tone = 'default',
	children,
}: {
	label: string;
	tone?: 'default' | 'warn';
	children: React.ReactNode;
}) {
	return (
		<div className="space-y-1">
			<div
				className={cn(
					'font-mono text-[10px] uppercase tracking-wider',
					tone === 'warn' ? 'text-[var(--achievement)]/80' : 'text-muted-foreground/70'
				)}
			>
				{label}
			</div>
			<ul className="m-0 list-none space-y-0.5 p-0">{children}</ul>
		</div>
	);
}

/* ───── Shared bits ───── */

function MetaGrid({ rows }: { rows: Array<[string, React.ReactNode]> }) {
	return (
		<dl className="grid grid-cols-[88px_1fr] gap-y-1.5 text-xs">
			{rows.flatMap(([k, v], i) => [
				<dt
					key={`${k}-${i}-k`}
					className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground/70"
				>
					{k}
				</dt>,
				<dd
					key={`${k}-${i}-v`}
					className="m-0 flex flex-wrap items-center gap-1 font-mono text-muted-foreground [&_code]:rounded-sm [&_code]:border [&_code]:border-border [&_code]:bg-background [&_code]:px-1 [&_code]:py-0.5 [&_code]:text-foreground"
				>
					{v}
				</dd>,
			])}
		</dl>
	);
}

function TrustCallout({ row }: { row: PkgRowV2 }) {
	const change = row.trust?.change_reason;
	let note = 'A change to this pkg requires your re-approval.';
	if (change?.kind === 'permissions_changed') {
		note = `Bumped from v${change.prior_version}. Added: ${change.added.join(', ') || '(none)'}. Re-approve before enabling.`;
	}
	return (
		<div className="space-y-1 rounded-sm border border-destructive/30 bg-destructive/10 p-3 text-sm text-destructive">
			<div className="font-medium">Trust review needed</div>
			<p className="text-[12.5px] leading-relaxed">{note}</p>
		</div>
	);
}

function ViolationCallout({ row }: { row: PkgRowV2 }) {
	const v = row.violations[0];
	return (
		<div className="space-y-1 rounded-sm border border-destructive/30 bg-destructive/10 p-3 text-sm text-destructive">
			<div className="font-medium">Permission violation</div>
			<p className="text-[12.5px] leading-relaxed">
				Attempted{' '}
				<code className="rounded-sm bg-background px-1 text-foreground">{v.scope_kind}</code> (
				<code className="rounded-sm bg-background px-1 text-foreground">{v.attempted}</code>) —
				denied at {new Date(v.occurred_at * 1000).toISOString().slice(0, 19).replace('T', ' ')}. No
				action required; pkg is sandboxed.
			</p>
		</div>
	);
}
