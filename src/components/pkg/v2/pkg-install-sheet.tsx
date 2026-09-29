// Install sheet — re-uses the loupe chrome with three tabs (Manifest URL ·
// Local path · Registry). Launched from the toolbar [Install pkg] button or
// from a registry row's [Install] action.
//
// Two paths actually install today:
//   1. Local path — pkgInstallFromPath against a manifest.json directory.
//   2. Registry, pkg-targeted — fetches the per-pkg detail, resolves a
//      dep plan, walks it via pkgInstallFromRegistry. Mirrors the install
//      loop that used to live in the legacy /packages/browse page.
// Manifest URL stays parked; the signed-registry path covers the same need
// with the signature/integrity guarantees baked in.

import { Globe, KeyRound, Lock, Plus, Shield, Terminal, X } from 'lucide-react';
import { useCallback, useRef, useState, type KeyboardEvent } from 'react';
import { Button } from '@/components/ui/button';
import { Sheet, SheetContent } from '@/components/ui/sheet';
import { cn } from '@/components/ui/utils';
import { pkgInstallFromPath } from '@/lib/tauri-cmd';
import { runInstallPlan } from '@/lib/registry/install-plan';
import { useMutation, useQueryClient } from '@tanstack/react-query';
import type { PkgRowV2 } from '@/lib/pkgs/use-derived';
import {
	useInstallPlanResolver,
	useRefreshRegistry,
	useRegistryIndex,
	useRegistryPkgDetail,
	type InstallStep,
	type RegistryEntry,
} from '@/lib/registry/use-registry';
import { PkgScreenshotCarousel } from './pkg-screenshots';

// ── Elevated-capability types (ADR-017 / WP-08) ──────────────────────────────
// These mirror the manifest shapes; extracted inline so we don't depend on the
// full contract package at this import boundary.
interface HttpCapability {
	auth_secret?: string | null;
	auth_header?: string;
}
interface NamedSecret {
	name: string;
	vault_key: string;
	required?: boolean;
	format?: string | null;
}
interface SecretsCapability {
	declarations?: NamedSecret[];
}
interface InvokeCapability {
	commands?: string[];
}
interface ElevatedCaps {
	http?: HttpCapability | null;
	secrets?: SecretsCapability | null;
	invoke?: InvokeCapability | null;
}

/** Read elevated caps from a manifest-shaped record. */
function extractElevatedCaps(
	manifest: Record<string, unknown> | null | undefined
): ElevatedCaps | null {
	if (!manifest) return null;
	const caps = manifest.capabilities as Record<string, unknown> | undefined;
	if (!caps) return null;
	const http = caps.http as HttpCapability | null | undefined;
	const secrets = caps.secrets as SecretsCapability | null | undefined;
	const invoke = caps.invoke as InvokeCapability | null | undefined;
	if (!http && !secrets && !invoke) return null;
	return { http: http ?? null, secrets: secrets ?? null, invoke: invoke ?? null };
}

/** Whether any elevated cap is declared. */
function hasElevatedCaps(caps: ElevatedCaps | null): boolean {
	if (!caps) return false;
	return !!(caps.http || caps.secrets || caps.invoke);
}

/** Read net globs from a manifest permissions block. */
function extractNetHosts(manifest: Record<string, unknown> | null | undefined): string[] {
	if (!manifest) return [];
	const perms = manifest.permissions as Record<string, unknown> | undefined;
	const net = perms?.['net'] ?? perms?.net;
	return Array.isArray(net) ? (net as string[]) : [];
}

/** Elevated-cap panel — shown in the pkg-targeted install sheet before the
 *  install button. Surfaces each declared elevated cap with its scope and a
 *  plain-English description. For non-builtin registry pkgs (pre-WP-06 GA)
 *  shows the interim "trusted capabilities require builtin provenance" note so
 *  the user knows the elevated caps will be inert at this trust level. */
function ElevatedCapsPanel({
	caps,
	netHosts,
	isTrustedSource,
}: {
	caps: ElevatedCaps;
	netHosts: string[];
	/** True when the install source is a builtin or a signed-registry entry.
	 *  False for plain registry installs (unsigned, pre-WP-06). */
	isTrustedSource: boolean;
}) {
	const items: React.ReactNode[] = [];

	if (caps.http) {
		const hosts = netHosts.length ? netHosts : ['any allowed host'];
		const withCred = caps.http.auth_secret
			? ` using your "${caps.http.auth_secret}" credential`
			: '';
		items.push(
			<div key="http" className="flex items-start gap-2.5 py-2">
				<Globe className="mt-0.5 h-3.5 w-3.5 shrink-0 text-muted-foreground" aria-hidden />
				<div className="min-w-0">
					<div className="text-xs font-medium text-foreground">Makes network requests</div>
					<div className="text-[11px] text-muted-foreground">
						To: {hosts.join(', ')}
						{withCred}
					</div>
				</div>
			</div>
		);
	}

	if (caps.secrets && (caps.secrets.declarations?.length ?? 0) > 0) {
		const names = (caps.secrets.declarations ?? []).map((d) => d.name);
		items.push(
			<div key="secrets" className="flex items-start gap-2.5 py-2">
				<KeyRound className="mt-0.5 h-3.5 w-3.5 shrink-0 text-muted-foreground" aria-hidden />
				<div className="min-w-0">
					<div className="text-xs font-medium text-foreground">Reads stored secrets</div>
					<div className="text-[11px] text-muted-foreground">{names.join(', ')}</div>
				</div>
			</div>
		);
	}

	if (caps.invoke && (caps.invoke.commands?.length ?? 0) > 0) {
		const cmds = caps.invoke.commands ?? [];
		items.push(
			<div key="invoke" className="flex items-start gap-2.5 py-2">
				<Terminal className="mt-0.5 h-3.5 w-3.5 shrink-0 text-muted-foreground" aria-hidden />
				<div className="min-w-0">
					<div className="text-xs font-medium text-foreground">Runs host commands</div>
					<div className="text-[11px] font-mono text-muted-foreground">{cmds.join(', ')}</div>
				</div>
			</div>
		);
	}

	return (
		<section className="space-y-2">
			<label className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground/70">
				elevated capabilities
			</label>
			<div className="divide-y divide-border rounded-sm border border-border bg-background">
				{items}
				{/* Trust state banner */}
				{isTrustedSource ? (
					<div className="flex items-center gap-2 px-3 py-2 text-[11px]">
						<Shield className="h-3.5 w-3.5 shrink-0 text-[var(--live)]" aria-hidden />
						<span className="text-muted-foreground">
							Trusted by provenance — elevated capabilities will be active.
						</span>
					</div>
				) : (
					<div className="flex items-center gap-2 px-3 py-2 text-[11px]">
						<Lock className="h-3.5 w-3.5 shrink-0 text-[var(--achievement)]" aria-hidden />
						<span className="text-muted-foreground">
							<span className="font-medium text-foreground">
								Trusted capabilities require builtin provenance.
							</span>{' '}
							Elevated capabilities will be <span className="font-medium">inert</span> until the pkg
							is signature-verified (WP-06 signing pipeline pending).
						</span>
					</div>
				)}
			</div>
		</section>
	);
}

type InstallTab = 'manifest-url' | 'local-path' | 'registry';

// Local horizontal roving-tablist keyboard handler for the generic install tab
// bar. Mirrors the shell <TabStrip> convention (data-tab-index + [role="tab"])
// but stays local to this screen rather than widening that file's exports.
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
		(e: KeyboardEvent<HTMLDivElement>) => {
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

interface InstallProgress {
	done: number;
	total: number;
	current: string;
}

export function PkgInstallSheet({
	open,
	onOpenChange,
	defaultUrl: _defaultUrl,
	defaultTab = 'manifest-url',
	pkg,
}: {
	open: boolean;
	onOpenChange: (open: boolean) => void;
	defaultUrl?: string;
	defaultTab?: InstallTab;
	/**
	 * If set, the sheet opens in "pkg-targeted" mode: hero preview + about
	 * + manifest preview + signed-registry install, instead of the generic
	 * three-tab paste/path/registry form. Comes from clicking [Install] on
	 * a specific registry row.
	 */
	pkg?: PkgRowV2 | null;
}) {
	const qc = useQueryClient();
	const [tab, setTab] = useState<InstallTab>(defaultTab);
	const genericTabs: InstallTab[] = ['manifest-url', 'local-path', 'registry'];
	const tabRoving = useRovingTabs(genericTabs.length, (i) => setTab(genericTabs[i]));
	const [path, setPath] = useState('');
	const [url, setUrl] = useState('');
	const [installError, setInstallError] = useState<string | null>(null);
	const [installProgress, setInstallProgress] = useState<InstallProgress | null>(null);

	const indexQuery = useRegistryIndex();
	const indexUrl = indexQuery.data?.indexUrl;
	// An already-installed pkg with a newer registry version carries the same
	// `registryEntry` (populated in use-derived) — so the in-place update path
	// reuses this exact sheet + signed dep-plan resolution as a fresh install.
	const isUpdate = Boolean(pkg?.installed && pkg?.latest);
	// Per-pkg detail is fetched only when this sheet is targeting a registry
	// pkg (or an installed pkg with an update) — the catalog row's hero
	// metadata is already enough until the user commits.
	const registryEntry: RegistryEntry | undefined = pkg?.registryEntry ?? undefined;
	const detailQuery = useRegistryPkgDetail(indexUrl, registryEntry);

	// ── Elevated capabilities (ADR-017, WP-08) ──────────────────────────────
	// Prefer the fetched detail (has the full manifest shape); fall back to the
	// pkg.manifest preview if detail is still loading.
	const manifestSource: Record<string, unknown> | null =
		(detailQuery.data as Record<string, unknown> | undefined) ?? pkg?.manifest ?? null;
	const elevatedCaps = extractElevatedCaps(manifestSource);
	const netHosts = extractNetHosts(manifestSource);
	// Trusted source = builtin provenance OR signed-registry (publisher_key
	// present in the registry entry, once WP-06 signing pipeline ships).
	// Until WP-06 GA, registry installs always show the "inert" copy.
	const isTrustedSource =
		pkg?.origin === 'builtin' ||
		!!(registryEntry as Record<string, unknown> | undefined)?.publisherKey;
	const planResolver = useInstallPlanResolver(indexUrl);
	const refreshRegistry = useRefreshRegistry();

	const fromPathMut = useMutation({
		mutationFn: () => pkgInstallFromPath(path),
		onSuccess: () => {
			void qc.refetchQueries({ queryKey: ['pkg'] });
			onOpenChange(false);
		},
		onError: (e) => setInstallError((e as Error).message ?? String(e)),
	});

	const fromRegistryMut = useMutation({
		mutationFn: async () => {
			if (!detailQuery.data) throw new Error('Registry detail not loaded yet');
			const plan: InstallStep[] = await planResolver.mutateAsync({
				root: detailQuery.data,
				// On an update, install exactly the version the index advertised
				// (`pkg.latest`) rather than trusting the detail file's array
				// ordering — deterministic and immune to index/detail drift.
				version: isUpdate ? (pkg?.latest ?? undefined) : undefined,
			});
			const done = await runInstallPlan(plan, {
				onProgress: (p) =>
					setInstallProgress({
						done: p.done,
						total: p.total,
						current: p.current || (pkg?.name ?? ''),
					}),
			});
			return done;
		},
		onSuccess: async () => {
			setInstallError(null);
			await qc.invalidateQueries({ queryKey: ['pkg'] });
			refreshRegistry();
			// Brief pause so the user sees the final "N of N" before the sheet closes.
			setTimeout(() => {
				setInstallProgress(null);
				onOpenChange(false);
			}, 800);
		},
		onError: (e) => {
			setInstallError((e as Error).message ?? String(e));
			setInstallProgress(null);
		},
	});

	return (
		<Sheet open={open} onOpenChange={onOpenChange}>
			<SheetContent
				side="right"
				className="flex w-full flex-col gap-0 border-l border-border !bg-background p-0 text-foreground sm:max-w-[560px]"
			>
				<div className="flex items-center gap-3 border-b border-border bg-muted/40 p-4">
					<div className="grid h-9 w-9 place-items-center rounded-sm border border-border bg-background text-primary">
						<Plus className="h-4.5 w-4.5" />
					</div>
					<div>
						<div className="font-display text-lg font-medium tracking-tight">
							{pkg ? `${isUpdate ? 'Update' : 'Install'} ${pkg.name}` : 'Install pkg'}
						</div>
						<div className="text-[11px] text-muted-foreground/70">
							{pkg
								? isUpdate
									? `${pkg.id}  v${pkg.version} → v${pkg.latest}`
									: `${pkg.id}@${pkg.version}`
								: 'paste a manifest URL or pick from registry'}
						</div>
					</div>
					<button
						type="button"
						onClick={() => onOpenChange(false)}
						className="ml-auto rounded p-1.5 text-muted-foreground hover:bg-accent hover:text-foreground"
						aria-label="Close"
					>
						<X className="h-4 w-4" />
					</button>
				</div>

				{pkg ? (
					<>
						<div className="flex-1 space-y-5 overflow-y-auto p-5">
							<PkgScreenshotCarousel row={pkg} variant="install-preview" />
							<section className="space-y-2">
								<label className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground/70">
									about
								</label>
								<p className="text-sm leading-relaxed text-foreground">{pkg.desc || '—'}</p>
							</section>
							{pkg.scopes.length > 0 && (
								<section className="space-y-2">
									<label className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground/70">
										manifest preview · {pkg.id}@{pkg.version}
									</label>
									<dl className="grid grid-cols-[88px_1fr] gap-y-1.5 text-xs">
										<dt className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground/70">
											routes
										</dt>
										<dd className="m-0 flex flex-wrap gap-1 font-mono text-muted-foreground">
											{pkg.routes.length ? (
												pkg.routes.map((r) => (
													<code
														key={r}
														className="rounded-sm border border-border bg-background px-1 py-0.5 text-foreground"
													>
														{r}
													</code>
												))
											) : (
												<span className="text-muted-foreground/70">none</span>
											)}
										</dd>
										<dt className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground/70">
											scopes
										</dt>
										<dd className="m-0 flex flex-wrap gap-1 font-mono text-muted-foreground">
											{pkg.scopes.map((s) => {
												const warn = /write|engine|shell\.execute|net:https/.test(s);
												return (
													<span
														key={s}
														className={cn(
															'rounded-sm border px-1.5 py-0.5 font-mono text-[10px]',
															warn
																? 'border-destructive/40 bg-destructive/10 text-destructive'
																: 'border-primary/30 bg-primary/10 text-primary'
														)}
													>
														{s}
													</span>
												);
											})}
										</dd>
									</dl>
								</section>
							)}
							{/* Elevated-cap disclosure (ADR-017 / WP-08) — shown whenever the
							    pkg's manifest declares capabilities.http / .secrets / .invoke */}
							{hasElevatedCaps(elevatedCaps) && elevatedCaps && (
								<ElevatedCapsPanel
									caps={elevatedCaps}
									netHosts={netHosts}
									isTrustedSource={isTrustedSource}
								/>
							)}
							{pkg.scopes.some((s) => /^fs:write/.test(s)) && (
								<div className="space-y-1 rounded-sm border border-destructive/30 bg-destructive/10 p-3 text-sm text-destructive">
									<div className="font-medium">Review write scopes</div>
									<p className="text-[12.5px] leading-relaxed">
										This pkg requests write access to{' '}
										{pkg.scopes
											.filter((s) => /^fs:write/.test(s))
											.map((s) => s.replace(/^fs:write:/, ''))
											.join(', ')}
										. Only install if you trust the publisher.
									</p>
								</div>
							)}
							{installError && (
								<div className="rounded-sm border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
									Install failed: {installError}
								</div>
							)}
							{installProgress && <InstallProgressBar progress={installProgress} />}
						</div>
						<div className="flex items-center gap-2 border-t border-border bg-muted/30 px-5 py-3">
							<RegistryStatus
								detailLoading={detailQuery.isLoading}
								detailError={detailQuery.error as Error | null}
								indexError={indexQuery.error as Error | null}
							/>
							<span className="flex-1" />
							<Button size="sm" variant="ghost" onClick={() => onOpenChange(false)}>
								Cancel
							</Button>
							<Button
								size="sm"
								disabled={!detailQuery.data || fromRegistryMut.isPending || !!installProgress}
								onClick={() => fromRegistryMut.mutate()}
							>
								<Plus className="mr-1.5 h-3.5 w-3.5" />
								{fromRegistryMut.isPending || installProgress
									? `${isUpdate ? 'Updating' : 'Installing'}… ${installProgress?.done ?? 0}/${installProgress?.total ?? '?'}`
									: `${isUpdate ? 'Update' : 'Install'} ${pkg.name}`}
							</Button>
						</div>
					</>
				) : (
					<>
						<div
							ref={tabRoving.containerRef}
							role="tablist"
							aria-label="Install source"
							onKeyDown={tabRoving.onKeyDown}
							className="flex gap-0 border-b border-border bg-muted/40 px-5"
						>
							{genericTabs.map((id, i) => {
								const selected = tab === id;
								return (
									<button
										key={id}
										type="button"
										role="tab"
										id={`install-tab-${id}`}
										aria-selected={selected}
										aria-controls="install-tab-panel"
										data-tab-index={i}
										tabIndex={selected ? 0 : -1}
										onClick={() => setTab(id)}
										className={cn(
											'border-b-2 px-3 py-2.5 text-sm font-medium transition-colors',
											'outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset',
											selected
												? 'border-primary text-foreground'
												: 'border-transparent text-muted-foreground hover:text-foreground'
										)}
									>
										{id === 'manifest-url'
											? 'Manifest URL'
											: id === 'local-path'
												? 'Local path'
												: 'Registry'}
									</button>
								);
							})}
						</div>

						<div
							role="tabpanel"
							id="install-tab-panel"
							aria-labelledby={`install-tab-${tab}`}
							className="flex-1 space-y-5 overflow-y-auto p-5"
						>
							{tab === 'manifest-url' && (
								<section className="space-y-2">
									<label className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground/70">
										manifest URL
									</label>
									<input
										value={url}
										onChange={(e) => setUrl(e.target.value)}
										placeholder="https://pkgs.ikenga.ai/com.royalti.content/0.3.1/manifest.json"
										className="w-full rounded-sm border border-border bg-background px-2.5 py-1.5 font-mono text-sm outline-none focus:border-primary"
									/>
									<p className="text-[11.5px] leading-relaxed text-muted-foreground">
										Paste a JSON manifest URL. The kernel fetches and validates against the schema
										in{' '}
										<code className="rounded-sm bg-background px-1 text-primary">
											@ikenga/contract
										</code>
										. Manifest preview lands in Phase 4.
									</p>
								</section>
							)}
							{tab === 'local-path' && (
								<section className="space-y-2">
									<label className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground/70">
										absolute path
									</label>
									<input
										value={path}
										onChange={(e) => setPath(e.target.value)}
										placeholder="/Users/you/my-pkg"
										className="w-full rounded-sm border border-border bg-background px-2.5 py-1.5 font-mono text-sm outline-none focus:border-primary"
									/>
									<p className="text-[11.5px] leading-relaxed text-muted-foreground">
										Install from a local directory containing a <code>manifest.json</code>. Useful
										for development; the directory must remain at this path.
									</p>
								</section>
							)}
							{tab === 'registry' && (
								<section className="space-y-2 text-sm text-muted-foreground">
									The registry browser opens on /packages?filter=store — close this sheet and pick a
									pkg row's <strong>Install</strong> button to land back in this sheet with the
									manifest pre-loaded.
								</section>
							)}
						</div>

						<div className="flex justify-end gap-2 border-t border-border bg-muted/30 px-5 py-3">
							<Button size="sm" variant="ghost" onClick={() => onOpenChange(false)}>
								Cancel
							</Button>
							{tab === 'local-path' ? (
								<Button
									size="sm"
									onClick={() => fromPathMut.mutate()}
									disabled={!path || fromPathMut.isPending}
								>
									{fromPathMut.isPending ? 'Installing…' : 'Install'}
								</Button>
							) : tab === 'manifest-url' ? (
								<Button size="sm" disabled title="Use Registry for signed installs">
									Install
								</Button>
							) : null}
						</div>
					</>
				)}
			</SheetContent>
		</Sheet>
	);
}

/* ───── Subcomponents ───── */

function InstallProgressBar({ progress }: { progress: InstallProgress }) {
	const pct = progress.total > 0 ? Math.round((progress.done / progress.total) * 100) : 0;
	return (
		<div className="space-y-1.5">
			<div className="flex items-center justify-between font-mono text-[11px] text-muted-foreground">
				<span>
					Installing <span className="text-foreground">{progress.current || '…'}</span>
				</span>
				<span>
					{progress.done} of {progress.total}
				</span>
			</div>
			<div className="h-1.5 overflow-hidden rounded-full bg-muted">
				<div
					className="h-full bg-primary transition-all duration-300"
					style={{ width: `${pct}%` }}
				/>
			</div>
		</div>
	);
}

function RegistryStatus({
	detailLoading,
	detailError,
	indexError,
}: {
	detailLoading: boolean;
	detailError: Error | null;
	indexError: Error | null;
}) {
	if (indexError) {
		return (
			<span role="alert" aria-live="assertive" className="font-mono text-[10.5px] text-destructive">
				registry unreachable: {indexError.message}
			</span>
		);
	}
	if (detailError) {
		return (
			<span role="alert" aria-live="assertive" className="font-mono text-[10.5px] text-destructive">
				detail failed: {detailError.message}
			</span>
		);
	}
	if (detailLoading) {
		return (
			<span
				role="status"
				aria-live="polite"
				className="font-mono text-[10.5px] text-muted-foreground"
			>
				resolving plan…
			</span>
		);
	}
	return (
		<span
			role="status"
			aria-live="polite"
			className="font-mono text-[10.5px] text-muted-foreground"
		>
			signature verified · ready
		</span>
	);
}
