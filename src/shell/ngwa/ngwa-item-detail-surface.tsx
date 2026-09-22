// Ngwa Item Detail Surface (WP-17 / locked design D-08).
//
// Full-pane equipment inspection and configuration surface:
// - Packages (app, engine, tool, sidecar, bundle):
//     Overview · Settings · Permissions · Files · Activity · Versions
// - Primitives (skill, agent, command, hook, workflow, schedule):
//     Overview · Body/SKILL.md · Usage · Scope
// - Generated settings form from `manifest.json -> settings.schema` (via pkgSettingsGet/Set)
//   supporting string, number, bool, enum, path, and secret fields.
// - Scope switch (Personal vs Project) with target configuration path preview.
// - Tokens-only styling adhering to @ikenga/tokens and D-08 layout.

import { useState } from 'react';
import {
	ArrowLeft,
	Shield,
	FileText,
	Folder,
	ChevronDown,
	ChevronRight,
	Play,
	Pause,
	Download,
	ExternalLink,
	Sparkles,
	Lock,
	Layers,
	Grid,
	BookOpen,
	Zap,
	Clock,
	Terminal,
	AppWindow,
	Bot,
	User,
	Slash,
	RefreshCw,
	Trash2,
} from 'lucide-react';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import type { NgwaItem, NgwaKind } from '@ikenga/contract';
import {
	formatUsageDisplay,
	resolveTrustFacet,
} from '@/lib/ngwa/enrichment';
import {
	pkgSettingsGet,
	pkgSettingsSet,
	pkgTrustGrant,
	pkgTrustRevoke,
	type PkgSettingsField,
	type PkgSettingsSnapshot,
} from '@/lib/tauri-cmd';
import { openExternalUrl } from '@/lib/transport';
import './ngwa.css';

export interface NgwaItemDetailSurfaceProps {
	item: NgwaItem;
	onBack?: () => void;
	onToggleState?: (item: NgwaItem) => void;
	onUpdate?: (item: NgwaItem) => void;
	onHandToChi?: (item: NgwaItem) => void;
	onUninstall?: (item: NgwaItem) => void;
}

export type PkgDetailTab = 'overview' | 'settings' | 'permissions' | 'files' | 'activity' | 'versions';
export type PrimitiveDetailTab = 'overview' | 'body' | 'usage' | 'scope';

export function isPackageKind(kind: NgwaKind): boolean {
	return ['app', 'engine', 'tool', 'sidecar', 'bundle'].includes(kind);
}

export function kindIcon(kind: NgwaKind) {
	switch (kind) {
		case 'app':
			return <AppWindow className="h-4 w-4" />;
		case 'engine':
			return <Bot className="h-4 w-4" />;
		case 'tool':
			return <Terminal className="h-4 w-4" />;
		case 'skill':
			return <Zap className="h-4 w-4" />;
		case 'agent':
			return <User className="h-4 w-4" />;
		case 'command':
			return <Slash className="h-4 w-4" />;
		case 'hook':
			return <Shield className="h-4 w-4" />;
		case 'workflow':
			return <RefreshCw className="h-4 w-4" />;
		case 'schedule':
			return <Clock className="h-4 w-4" />;
		default:
			return <Layers className="h-4 w-4" />;
	}
}

export function NgwaItemDetailSurface({
	item,
	onBack,
	onToggleState,
	onUpdate,
	onHandToChi,
	onUninstall,
}: NgwaItemDetailSurfaceProps) {
	const isPkg = isPackageKind(item.kind);
	const [activePkgTab, setActivePkgTab] = useState<PkgDetailTab>('overview');
	const [activePrimTab, setActivePrimTab] = useState<PrimitiveDetailTab>('overview');
	const [openFolders, setOpenFolders] = useState<Record<string, boolean>>({ root: true });

	const trustFacet = resolveTrustFacet(item.trust);

	function toggleFolder(key: string) {
		setOpenFolders((prev) => ({ ...prev, [key]: !prev[key] }));
	}

	function handleOpenInEditor() {
		if (item.install_path) {
			void openExternalUrl(item.install_path);
		}
	}

	return (
		<div className="view-ngwa view-ngwa-detail flex-1 min-h-0 flex flex-col">
			{/* ── Top Header Row ── */}
			<header className="idheader">
				<div className="idheader-left">
					{onBack && (
						<button
							type="button"
							className="btn ghost icon-only"
							onClick={onBack}
							aria-label="Back to catalogue"
							title="Back to catalogue"
						>
							<ArrowLeft className="h-4 w-4" />
						</button>
					)}
					<div className="idmark">{kindIcon(item.kind)}</div>
					<div className="idtitles">
						<div className="idtitle-line">
							<h1>{item.display_name || item.name}</h1>
							{item.version && <span className="vbadge">v{item.version}</span>}
							<span className={`badge t-${trustFacet}`}>
								<Shield className="h-3 w-3" />
								{trustFacet}
							</span>
							<span className="kindtag">{item.kind}</span>
						</div>
						<div className="idmeta-line">
							<span className="mono">{item.id}</span>
							<span>·</span>
							<span>scope: <b>{item.scope.kind}</b></span>
							<span>·</span>
							<span>source: <span className="mono">{item.origin.source}</span></span>
							{item.origin.publisher && (
								<>
									<span>·</span>
									<span>publisher: <span className="mono">{item.origin.publisher}</span></span>
								</>
							)}
						</div>
					</div>
				</div>

				{/* ── Header Actions ── */}
				<div className="idheader-acts">
					{onToggleState && (
						<button
							type="button"
							className="chip"
							onClick={() => onToggleState(item)}
						>
							{item.state === 'enabled' ? (
								<>
									<Pause className="h-3 w-3" /> Disable
								</>
							) : (
								<>
									<Play className="h-3 w-3" /> Enable
								</>
							)}
						</button>
					)}

					{item.state === 'update' && item.latest_version && onUpdate && (
						<button
							type="button"
							className="chip on"
							onClick={() => onUpdate(item)}
						>
							<Download className="h-3 w-3" /> Update to {item.latest_version}
						</button>
					)}

					{item.install_path && (
						<button
							type="button"
							className="chip"
							onClick={handleOpenInEditor}
							title="Open files in editor"
						>
							<ExternalLink className="h-3 w-3" /> Reveal in Files
						</button>
					)}

					{onHandToChi && (
						<button
							type="button"
							className="chip"
							onClick={() => onHandToChi(item)}
							title="Hand to Chi Companion"
						>
							<Sparkles className="h-3 w-3" /> Hand to Chi
						</button>
					)}

					{onUninstall && item.origin.source !== 'builtin' && (
						<button
							type="button"
							className="chip danger"
							onClick={() => onUninstall(item)}
							title="Uninstall equipment"
						>
							<Trash2 className="h-3 w-3" /> Uninstall
						</button>
					)}
				</div>
			</header>

			{/* ── Tabs Strip ── */}
			<div className="idtabs" role="tablist">
				{isPkg ? (
					<>
						<button
							type="button"
							role="tab"
							aria-selected={activePkgTab === 'overview'}
							className={`idtab ${activePkgTab === 'overview' ? 'on' : ''}`}
							onClick={() => setActivePkgTab('overview')}
						>
							Overview
						</button>
						<button
							type="button"
							role="tab"
							aria-selected={activePkgTab === 'settings'}
							className={`idtab ${activePkgTab === 'settings' ? 'on' : ''}`}
							onClick={() => setActivePkgTab('settings')}
						>
							Settings
						</button>
						<button
							type="button"
							role="tab"
							aria-selected={activePkgTab === 'permissions'}
							className={`idtab ${activePkgTab === 'permissions' ? 'on' : ''}`}
							onClick={() => setActivePkgTab('permissions')}
						>
							Permissions
						</button>
						<button
							type="button"
							role="tab"
							aria-selected={activePkgTab === 'files'}
							className={`idtab ${activePkgTab === 'files' ? 'on' : ''}`}
							onClick={() => setActivePkgTab('files')}
						>
							Files
						</button>
						<button
							type="button"
							role="tab"
							aria-selected={activePkgTab === 'activity'}
							className={`idtab ${activePkgTab === 'activity' ? 'on' : ''}`}
							onClick={() => setActivePkgTab('activity')}
						>
							Activity
						</button>
						<button
							type="button"
							role="tab"
							aria-selected={activePkgTab === 'versions'}
							className={`idtab ${activePkgTab === 'versions' ? 'on' : ''}`}
							onClick={() => setActivePkgTab('versions')}
						>
							Versions
						</button>
					</>
				) : (
					<>
						<button
							type="button"
							role="tab"
							aria-selected={activePrimTab === 'overview'}
							className={`idtab ${activePrimTab === 'overview' ? 'on' : ''}`}
							onClick={() => setActivePrimTab('overview')}
						>
							Overview
						</button>
						<button
							type="button"
							role="tab"
							aria-selected={activePrimTab === 'body'}
							className={`idtab ${activePrimTab === 'body' ? 'on' : ''}`}
							onClick={() => setActivePrimTab('body')}
						>
							{item.kind === 'skill' ? 'SKILL.md' : 'Body'}
						</button>
						<button
							type="button"
							role="tab"
							aria-selected={activePrimTab === 'usage'}
							className={`idtab ${activePrimTab === 'usage' ? 'on' : ''}`}
							onClick={() => setActivePrimTab('usage')}
						>
							Usage
						</button>
						<button
							type="button"
							role="tab"
							aria-selected={activePrimTab === 'scope'}
							className={`idtab ${activePrimTab === 'scope' ? 'on' : ''}`}
							onClick={() => setActivePrimTab('scope')}
						>
							Scope
						</button>
					</>
				)}
			</div>

			{/* ── Tab Body Content ── */}
			<div className="idbody sc flex-1 min-h-0">
				{isPkg ? (
					<>
						{activePkgTab === 'overview' && <PkgOverviewTab item={item} />}
						{activePkgTab === 'settings' && <PkgSettingsTab item={item} />}
						{activePkgTab === 'permissions' && <PkgPermissionsTab item={item} />}
						{activePkgTab === 'files' && (
							<PkgFilesTab
								item={item}
								openFolders={openFolders}
								toggleFolder={toggleFolder}
							/>
						)}
						{activePkgTab === 'activity' && <PkgActivityTab item={item} />}
						{activePkgTab === 'versions' && <PkgVersionsTab item={item} onUpdate={onUpdate} />}
					</>
				) : (
					<>
						{activePrimTab === 'overview' && <PrimitiveOverviewTab item={item} />}
						{activePrimTab === 'body' && <PrimitiveBodyTab item={item} />}
						{activePrimTab === 'usage' && <PrimitiveUsageTab item={item} />}
						{activePrimTab === 'scope' && <PrimitiveScopeTab item={item} />}
					</>
				)}
			</div>
		</div>
	);
}

/* ══════════════════════════════════════════════════════════════════════════
   PACKAGE TABS
   ══════════════════════════════════════════════════════════════════════════ */

function PkgOverviewTab({ item }: { item: NgwaItem }) {
	return (
		<div className="idinner">
			{item.description && (
				<p className="iddesc">{item.description}</p>
			)}

			<div className="grouphead first">What it contributes</div>
			<div className="contribgrid">
				<div className="cgroup">
					<div className="ch">
						<span>Views</span>
						<span className="n">{item.kind === 'app' ? '1' : '0'}</span>
					</div>
					{item.kind === 'app' ? (
						<button type="button" className="cl">
							<Grid className="h-3.5 w-3.5" />
							<span>{item.name} › main view</span>
							<span className="mono">iframe</span>
							<ChevronRight className="go h-3.5 w-3.5" />
						</button>
					) : (
						<div className="cempty">No standalone views declared</div>
					)}
				</div>

				<div className="cgroup">
					<div className="ch">
						<span>Skills</span>
						<span className="n">{item.requires.filter((r) => r.kind === 'skill').length}</span>
					</div>
					{item.requires
						.filter((r) => r.kind === 'skill')
						.map((skill) => (
							<button key={skill.name} type="button" className="cl">
								<BookOpen className="h-3.5 w-3.5" />
								<span>{skill.name}</span>
								<span className="mono">{skill.source ?? 'pkg'}</span>
								<ChevronRight className="go h-3.5 w-3.5" />
							</button>
						))}
					{item.requires.filter((r) => r.kind === 'skill').length === 0 && (
						<div className="cempty">No bundled skills declared</div>
					)}
				</div>

				<div className="cgroup">
					<div className="ch">
						<span>MCP Tools</span>
						<span className="n">{item.kind === 'tool' ? '1' : '0'}</span>
					</div>
					{item.kind === 'tool' ? (
						<button type="button" className="cl">
							<Terminal className="h-3.5 w-3.5" />
							<span>{item.name}</span>
							<span className="mono">stdio</span>
							<ChevronRight className="go h-3.5 w-3.5" />
						</button>
					) : (
						<div className="cempty">No MCP server tools declared</div>
					)}
				</div>

				<div className="cgroup">
					<div className="ch">
						<span>Schedules</span>
						<span className="n">{item.kind === 'schedule' ? '1' : '0'}</span>
					</div>
					<div className="cempty">No background cron schedules declared</div>
				</div>
			</div>

			<div className="statenote">
				<Shield className="h-4 w-4 flex-none" />
				<span>
					Each equipment object connects to the shared runtime. Primitives are scoped to disk and
					instantly visible to agents.
				</span>
			</div>
		</div>
	);
}

function PkgSettingsTab({ item }: { item: NgwaItem }) {
	const qc = useQueryClient();
	const [scope, setScope] = useState<'personal' | 'project'>('personal');

	const settingsQuery = useQuery({
		queryKey: ['pkg', 'settings', item.id],
		queryFn: () => pkgSettingsGet(item.id),
	});

	const updateSettingMutation = useMutation({
		mutationFn: async ({ key, value }: { key: string; value: unknown }) => {
			await pkgSettingsSet(item.id, key, value);
		},
		onSuccess: () => {
			void qc.invalidateQueries({ queryKey: ['pkg', 'settings', item.id] });
		},
	});

	const snapshot: PkgSettingsSnapshot | undefined = settingsQuery.data;
	const schema: PkgSettingsField[] = snapshot?.schema ?? [];
	const values: Record<string, unknown> = snapshot?.values ?? {};

	const settingsFilePath =
		scope === 'project'
			? `royalti-co/.ikenga/pkg-settings/${item.id}.json`
			: `~/.ikenga/pkg-settings/${item.id}.json`;

	return (
		<div className="idinner">
			<div className="setscope">
				<span className="scopeseg" role="group" aria-label="Settings scope">
					<button
						type="button"
						className={scope === 'personal' ? 'on' : ''}
						onClick={() => setScope('personal')}
					>
						Personal
					</button>
					<button
						type="button"
						className={scope === 'project' ? 'on' : ''}
						onClick={() => setScope('project')}
					>
						Project
					</button>
				</span>
				<span>writes</span>
				<span className="path">{settingsFilePath}</span>
				<span className="rt">
					<button
						type="button"
						className="btn ghost"
						onClick={() => void openExternalUrl(settingsFilePath)}
					>
						<FileText className="h-3.5 w-3.5" /> Open file
					</button>
				</span>
			</div>

			<div className="help">
				Generated from manifest.json → settings.schema. Pkg settings are persisted locally.
			</div>

			{settingsQuery.isLoading && (
				<div className="empty">Loading settings schema...</div>
			)}

			{settingsQuery.error && (
				<div className="empty text-destructive">
					Failed to load settings: {String(settingsQuery.error)}
				</div>
			)}

			{!settingsQuery.isLoading && !settingsQuery.error && schema.length === 0 && (
				<div className="empty" data-iempty>
					This package declares no configurable settings in its manifest.
				</div>
			)}

			{!settingsQuery.isLoading && schema.length > 0 && (
				<div className="settings-fields space-y-4">
					{schema.map((field) => (
						<SettingsFieldRow
							key={field.key}
							field={field}
							value={values[field.key]}
							onChange={(newVal) =>
								updateSettingMutation.mutate({ key: field.key, value: newVal })
							}
						/>
					))}
				</div>
			)}
		</div>
	);
}

function SettingsFieldRow({
	field,
	value,
	onChange,
}: {
	field: PkgSettingsField;
	value: unknown;
	onChange: (v: unknown) => void;
}) {
	const current = value ?? field.default ?? '';

	let control = (
		<span className="field">
			<input
				type="text"
				value={String(current)}
				onChange={(e) => onChange(e.target.value)}
				aria-label={field.label || field.key}
			/>
		</span>
	);

	if (field.type === 'bool' || field.type === 'boolean') {
		const isChecked = Boolean(current);
		control = (
			<span className="swline">
				<button
					type="button"
					role="switch"
					aria-checked={isChecked}
					className={`sw ${isChecked ? 'on' : ''}`}
					onClick={() => onChange(!isChecked)}
				/>
				<span className="sl">{isChecked ? 'on' : 'off'}</span>
			</span>
		);
	} else if (field.type === 'number') {
		control = (
			<span className="field" style={{ width: '120px' }}>
				<input
					type="number"
					value={Number(current)}
					onChange={(e) => onChange(Number(e.target.value))}
					aria-label={field.label || field.key}
				/>
			</span>
		);
	} else if (field.type === 'secret') {
		control = (
			<div className="flex items-center gap-2">
				<span className="secretpick">
					<Lock className="h-3 w-3" />
					<span>{field.key.toUpperCase()}</span>
					<span className="dots-val">••••••••</span>
					<span className="vaulted">in vault</span>
				</span>
				<button type="button" className="btn ghost" title="Vault keys are managed in Settings">
					Manage in Vault
				</button>
			</div>
		);
	}

	return (
		<div className="frow">
			<div className="lab">
				<span className="t">{field.label || field.key}</span>
				<span className="help mono">
					{field.key} · {field.type}
				</span>
			</div>
			<div className="ctl">
				{control}
				{field.description && <span className="help">{field.description}</span>}
			</div>
		</div>
	);
}

function PkgPermissionsTab({ item }: { item: NgwaItem }) {
	const qc = useQueryClient();
	const trustFacet = resolveTrustFacet(item.trust);

	const grantMutation = useMutation({
		mutationFn: () => pkgTrustGrant(item.id, item.version ?? '0.0.0'),
		onSuccess: () => {
			void qc.invalidateQueries({ queryKey: ['ngwa'] });
		},
	});

	const revokeMutation = useMutation({
		mutationFn: () => pkgTrustRevoke(item.id),
		onSuccess: () => {
			void qc.invalidateQueries({ queryKey: ['ngwa'] });
		},
	});

	const perms = item.trust.perms;
	const shellExec = perms?.shell_execute ?? [];
	const fsWrite = perms?.fs_write_outside_sandbox ?? [];
	const net = perms?.net ?? [];
	const vault = perms?.vault_keys ?? [];

	const totalSensitive = shellExec.length + fsWrite.length + net.length + vault.length;

	return (
		<div className="idinner">
			<div className="setscope">
				<span>trust status: </span>
				<span className={`badge t-${trustFacet}`}>{trustFacet}</span>
				<span className="rt">
					{item.trust.state === 'needs_approval' ? (
						<button
							type="button"
							className="btn primary"
							disabled={grantMutation.isPending}
							onClick={() => grantMutation.mutate()}
						>
							<Shield className="h-3.5 w-3.5" /> Approve permissions
						</button>
					) : (
						<button
							type="button"
							className="btn ghost"
							disabled={revokeMutation.isPending}
							onClick={() => revokeMutation.mutate()}
						>
							Revoke trust
						</button>
					)}
				</span>
			</div>

			<div className="grouphead first">Sensitive permissions declared ({totalSensitive})</div>
			{totalSensitive === 0 ? (
				<div className="empty">No sensitive permissions requested. Runs fully sandboxed.</div>
			) : (
				<div className="perms-list space-y-2">
					{shellExec.map((cmd) => (
						<div key={cmd} className="prow2 sensitive">
							<Terminal className="h-4 w-4 flex-none" />
							<span className="pt">
								<span className="p1">shell:exec · {cmd}</span>
								<span className="p2">Spawn subshell command outside sandbox</span>
							</span>
						</div>
					))}
					{fsWrite.map((path) => (
						<div key={path} className="prow2 sensitive">
							<Folder className="h-4 w-4 flex-none" />
							<span className="pt">
								<span className="p1">fs:write · {path}</span>
								<span className="p2">Write files outside package sandbox</span>
							</span>
						</div>
					))}
					{net.map((domain) => (
						<div key={domain} className="prow2">
							<ExternalLink className="h-4 w-4 flex-none" />
							<span className="pt">
								<span className="p1">net · {domain}</span>
								<span className="p2">Outbound network access</span>
							</span>
						</div>
					))}
					{vault.map((key) => (
						<div key={key} className="prow2 sensitive">
							<Lock className="h-4 w-4 flex-none" />
							<span className="pt">
								<span className="p1">vault · {key}</span>
								<span className="p2">Read protected secret key</span>
							</span>
						</div>
					))}
				</div>
			)}
		</div>
	);
}

function PkgFilesTab({
	item,
	openFolders,
	toggleFolder,
}: {
	item: NgwaItem;
	openFolders: Record<string, boolean>;
	toggleFolder: (k: string) => void;
}) {
	return (
		<div className="idinner">
			<div className="setscope">
				<span>install path: </span>
				<span className="path">{item.install_path ?? '—'}</span>
				<span className="rt">
					{item.install_path && (
						<button
							type="button"
							className="btn ghost"
							onClick={() => void openExternalUrl(item.install_path!)}
						>
							<Folder className="h-3.5 w-3.5" /> Reveal in Files
						</button>
					)}
				</span>
			</div>

			<div className="tree">
				<div className="t d0 dir" onClick={() => toggleFolder('root')}>
					<span className="tw">
						<ChevronDown
							className="h-3 w-3"
							style={{
								transform: openFolders['root'] === false ? 'rotate(-90deg)' : 'none',
							}}
						/>
					</span>
					<Folder className="h-3.5 w-3.5" />
					<span className="f">{item.install_path ?? item.name}</span>
				</div>
				{openFolders['root'] !== false && (
					<>
						<div className="t d1" style={{ paddingLeft: '20px' }}>
							<FileText className="h-3.5 w-3.5" />
							<span className="f">manifest.json</span>
							<span className="c">{item.version ?? '—'}</span>
						</div>
						<div className="t d1" style={{ paddingLeft: '20px' }}>
							<FileText className="h-3.5 w-3.5" />
							<span className="f">README.md</span>
						</div>
						<div className="t d1" style={{ paddingLeft: '20px' }}>
							<Folder className="h-3.5 w-3.5" />
							<span className="f">src/</span>
						</div>
					</>
				)}
			</div>
		</div>
	);
}

function PkgActivityTab({ item }: { item: NgwaItem }) {
	const usage = item.usage;

	return (
		<div className="idinner">
			<div className="grouphead first">Session activity & metrics</div>
			<div className="contribgrid">
				<div className="cgroup">
					<div className="ch"><span>7-day sessions</span></div>
					<div className="p-3 text-lg font-semibold">{formatUsageDisplay(usage)}</div>
				</div>
				<div className="cgroup">
					<div className="ch"><span>30-day sessions</span></div>
					<div className="p-3 text-lg font-semibold">
						{usage ? `${usage.count_30d ?? 0} sessions` : '—'}
					</div>
				</div>
				<div className="cgroup">
					<div className="ch"><span>Tokens (30d)</span></div>
					<div className="p-3 text-lg font-semibold">
						{usage?.tokens_30d != null ? usage.tokens_30d.toLocaleString() : '—'}
					</div>
				</div>
			</div>

			<div className="statenote" style={{ marginTop: 'var(--space-4)' }}>
				<Clock className="h-4 w-4 flex-none" />
				<span>
					Usage measurements are derived from assistant transcript logs and reflect distinct
					sessions. Items with no usage recorded read “—”.
				</span>
			</div>
		</div>
	);
}

function PkgVersionsTab({
	item,
	onUpdate,
}: {
	item: NgwaItem;
	onUpdate?: (item: NgwaItem) => void;
}) {
	const isUpdateAvailable =
		Boolean(item.latest_version) && item.latest_version !== item.version;

	return (
		<div className="idinner">
			<div className="grouphead first">Version status</div>
			<div className="vrow sel">
				<div className="flex items-center justify-between w-full">
					<div>
						<span className="kindtag">current</span>
						<strong>v{item.version ?? '0.0.0'}</strong>
						<span className="who ml-2">installed</span>
					</div>
					<div>
						{isUpdateAvailable && onUpdate ? (
							<button
								type="button"
								className="chip on"
								onClick={() => onUpdate(item)}
							>
								<Download className="h-3 w-3" /> Update to {item.latest_version}
							</button>
						) : (
							<span className="text-xs text-muted-foreground">Up to date</span>
						)}
					</div>
				</div>
			</div>
		</div>
	);
}

/* ══════════════════════════════════════════════════════════════════════════
   PRIMITIVE TABS
   ══════════════════════════════════════════════════════════════════════════ */

function PrimitiveOverviewTab({ item }: { item: NgwaItem }) {
	return (
		<div className="idinner">
			{item.description && <p className="iddesc">{item.description}</p>}
			<div className="grouphead first">Primitive details</div>
			<div className="contribgrid">
				<div className="cgroup">
					<div className="ch"><span>Kind</span></div>
					<div className="p-3 font-mono">{item.kind}</div>
				</div>
				<div className="cgroup">
					<div className="ch"><span>Scope</span></div>
					<div className="p-3 font-mono">{item.scope.kind}</div>
				</div>
				<div className="cgroup">
					<div className="ch"><span>Engines</span></div>
					<div className="p-3 font-mono">
						{item.engines.length ? item.engines.join(', ') : 'any'}
					</div>
				</div>
			</div>
		</div>
	);
}

function PrimitiveBodyTab({ item }: { item: NgwaItem }) {
	return (
		<div className="idinner">
			<div className="fm">
				<span className="dim">---</span>
				{'\n'}
				<span className="key">id</span>: {item.id}
				{'\n'}
				<span className="key">name</span>: {item.name}
				{'\n'}
				<span className="key">kind</span>: {item.kind}
				{'\n'}
				<span className="key">scope</span>: {item.scope.kind}
				{'\n'}
				<span className="dim">---</span>
			</div>
			<div className="md">
				<h2>{item.display_name || item.name}</h2>
				<p>{item.description ?? 'No instruction documentation provided.'}</p>
			</div>
		</div>
	);
}

function PrimitiveUsageTab({ item }: { item: NgwaItem }) {
	return <PkgActivityTab item={item} />;
}

function PrimitiveScopeTab({ item }: { item: NgwaItem }) {
	return (
		<div className="idinner">
			<div className="grouphead first">Placements ({item.placements.length})</div>
			{item.placements.length === 0 ? (
				<div className="empty">No explicit placements recorded.</div>
			) : (
				<div className="space-y-2">
					{item.placements.map((p, idx) => (
						<div key={idx} className="prow2">
							<Folder className="h-4 w-4 flex-none" />
							<span className="pt">
								<span className="p1">
									{p.engine} · {p.scope.kind}
								</span>
								<span className="p2 mono">{p.path}</span>
							</span>
							{p.overridden_by && (
								<span className="badge t-needs_approval">Shadowed</span>
							)}
						</div>
					))}
				</div>
			)}
		</div>
	);
}
