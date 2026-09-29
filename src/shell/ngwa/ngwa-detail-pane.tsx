// Detail pane for selected Ngwa item (WP-15 / locked D-02).
//
// Tabs:
// - Body: Frontmatter / Markdown / Manifest preview
// - Files: In-place editable file tree
// - Permissions: Declared intent / granted permissions
// - Placement: Multi-engine placement matrix
// - Dependents: Items listing this item in requires[]

import { useCallback, useRef, useState } from 'react';
import {
	Shield,
	FileText,
	Folder,
	ChevronDown,
	Play,
	Pause,
	Download,
	Sparkles,
	MoveRight,
	Copy,
	Trash2,
} from 'lucide-react';
import type { NgwaItem } from '@ikenga/contract';
import { resolveTrustFacet } from '@/lib/ngwa/enrichment';
import type { NgwaAct, NgwaItemActionSet, NgwaScopePick } from '@/lib/ngwa/use-ngwa-actions';
import { NgwaPopMenu } from './ngwa-scope-ops';

export interface NgwaDetailPaneProps {
	item: NgwaItem;
	/** D-02's action row, built by `useNgwaItemActions`. Omitted ⇒ no row. */
	actions?: NgwaItemActionSet;
}

type DetailTab = 'body' | 'files' | 'perms' | 'place' | 'deps';

export function NgwaDetailPane({ item, actions }: NgwaDetailPaneProps) {
	const [activeTab, setActiveTab] = useState<DetailTab>('body');
	const [openFolders, setOpenFolders] = useState<Record<string, boolean>>({});

	const trustFacet = resolveTrustFacet(item.trust);

	function toggleFolder(key: string) {
		setOpenFolders((prev) => ({ ...prev, [key]: !prev[key] }));
	}

	return (
		<aside className="detailcol">
			{/* ── Header ── */}
			<div className="dhead">
				<div className="dtitle">
					<h2>{item.display_name || item.name}</h2>
					{item.version && <span className="v">v{item.version}</span>}
					<span className={`badge t-${trustFacet}`}>
						<Shield className="h-3 w-3" />
						{trustFacet}
					</span>
				</div>

				<div className="dsub">
					<span>
						kind: <b>{item.kind}</b>
					</span>
					<span>
						scope: <b>{item.scope.kind}</b>
					</span>
					<span>
						source: <span className="mono">{item.origin.source}</span>
					</span>
					{item.origin.publisher && (
						<span>
							publisher: <span className="mono">{item.origin.publisher}</span>
						</span>
					)}
				</div>

				{item.description && (
					<p className="note" style={{ marginTop: 'var(--space-2)', marginBottom: 0 }}>
						{item.description}
					</p>
				)}

				{actions && <DetailActions actions={actions} />}
			</div>

			{/* ── Tabs ── */}
			<div className="dtabs" role="tablist">
				<button
					type="button"
					role="tab"
					className={`dtab ${activeTab === 'body' ? 'on' : ''}`}
					aria-selected={activeTab === 'body'}
					onClick={() => setActiveTab('body')}
				>
					Body
				</button>
				<button
					type="button"
					role="tab"
					className={`dtab ${activeTab === 'files' ? 'on' : ''}`}
					aria-selected={activeTab === 'files'}
					onClick={() => setActiveTab('files')}
				>
					Files
				</button>
				<button
					type="button"
					role="tab"
					className={`dtab ${activeTab === 'perms' ? 'on' : ''}`}
					aria-selected={activeTab === 'perms'}
					onClick={() => setActiveTab('perms')}
				>
					Permissions
				</button>
				<button
					type="button"
					role="tab"
					className={`dtab ${activeTab === 'place' ? 'on' : ''}`}
					aria-selected={activeTab === 'place'}
					onClick={() => setActiveTab('place')}
				>
					Placement <span className="n">{item.placements.length}</span>
				</button>
				<button
					type="button"
					role="tab"
					className={`dtab ${activeTab === 'deps' ? 'on' : ''}`}
					aria-selected={activeTab === 'deps'}
					onClick={() => setActiveTab('deps')}
				>
					Dependents <span className="n">{item.required_by.length}</span>
				</button>
			</div>

			{/* ── Body ── */}
			<div className="dbody sc">
				{activeTab === 'body' && (
					<div>
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
							{item.version && (
								<>
									{'\n'}
									<span className="key">version</span>: {item.version}
								</>
							)}
							{'\n'}
							<span className="dim">---</span>
						</div>
						<div className="md">
							<h1>{item.display_name}</h1>
							<p>{item.description ?? 'No description provided.'}</p>
						</div>
					</div>
				)}

				{activeTab === 'files' && (
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
									<span className="f">
										{item.kind === 'skill' ? 'SKILL.md' : 'manifest.json'}
									</span>
									<span className="c">{item.version ?? '—'}</span>
								</div>
								<div className="t d1" style={{ paddingLeft: '20px' }}>
									<FileText className="h-3.5 w-3.5" />
									<span className="f">README.md</span>
								</div>
							</>
						)}
					</div>
				)}

				{activeTab === 'perms' && (
					<div>
						{item.trust.perms ? (
							<>
								<div className="drow">
									<span className="k2">Enforced by</span>
									<span className="val">Pkg Kernel (manifest sandbox)</span>
								</div>
								<div className="subhead">Declared Permissions</div>
								{item.trust.perms.shell_execute.length > 0 && (
									<div className="drow">
										<span className="k2 mono">shell_execute</span>
										<span className="val">{item.trust.perms.shell_execute.join(', ')}</span>
									</div>
								)}
								{item.trust.perms.net.length > 0 && (
									<div className="drow">
										<span className="k2 mono">net</span>
										<span className="val">{item.trust.perms.net.join(', ')}</span>
									</div>
								)}
								{item.trust.perms.fs_write_outside_sandbox.length > 0 && (
									<div className="drow">
										<span className="k2 mono">fs_write</span>
										<span className="val">{item.trust.perms.fs_write_outside_sandbox.join(', ')}</span>
									</div>
								)}
							</>
						) : (
							<div className="drow">
								<span className="k2">Permissions</span>
								<span className="val text-muted-foreground">
									{item.kind === 'skill' || item.kind === 'agent' || item.kind === 'command'
										? 'None — declares intent, never granted sensitive permissions'
										: 'No permissions block on this item'}
								</span>
							</div>
						)}
					</div>
				)}

				{activeTab === 'place' && (
					<div>
						{['claude', 'codex', 'gemini'].map((eng) => {
							const placement = item.placements.find((p) => p.engine === eng);
							const hasPlacement = placement !== undefined;
							return (
								<div key={eng} className="drow">
									<span className="k2">{eng}</span>
									{hasPlacement ? (
										<>
											<span className="val mono text-xs">{placement.path}</span>
											<span className="rt">
												<span className="state s-enabled">placed</span>
											</span>
										</>
									) : (
										<span className="val text-muted-foreground text-xs">
											not placed in this engine
										</span>
									)}
								</div>
							);
						})}
					</div>
				)}

				{activeTab === 'deps' && (
					<div>
						<div className="drow">
							<span className="k2">Required by</span>
							<span className="val">
								{item.required_by.length > 0
									? item.required_by.map((r) => r.name).join(', ')
									: 'None — no package lists this in requires[]'}
							</span>
						</div>
						{item.requires.length > 0 && (
							<div className="drow" style={{ marginTop: 'var(--space-2)' }}>
								<span className="k2">Requires</span>
								<span className="val">
									{item.requires.map((r) => `${r.name} (${r.kind})`).join(', ')}
								</span>
							</div>
						)}
					</div>
				)}
			</div>
		</aside>
	);
}

/** D-02 `.dacts`: Disable/Enable · Move… · Copy to… · Update · Open folder ·
 *  Remove… (danger) · Hand to Chi (primary). A disabled action keeps its
 *  reason as its title. */
function DetailActions({ actions }: { actions: NgwaItemActionSet }) {
	const on = actions.toggle.label === 'Disable';
	return (
		<div className="dacts" data-dacts>
			<ActButton act={actions.toggle} icon={on ? <Pause className="h-3 w-3" /> : <Play className="h-3 w-3" />} />
			<PickButton pick={actions.move} label="Move…" icon={<MoveRight className="h-3 w-3" />} />
			<PickButton pick={actions.copy} label="Copy to…" icon={<Copy className="h-3 w-3" />} />
			<ActButton
				act={{ ...actions.update, label: 'Update' }}
				icon={<Download className="h-3 w-3" />}
				title={actions.update.disabledReason ?? actions.update.label}
			/>
			<ActButton act={actions.openFolder} icon={<Folder className="h-3 w-3" />} />
			<ActButton act={actions.remove} icon={<Trash2 className="h-3 w-3" />} className="chip danger" />
			<ActButton act={actions.handToChi} icon={<Sparkles className="h-3 w-3" />} className="chip on" />
		</div>
	);
}

function ActButton({
	act,
	icon,
	className = 'chip',
	title,
}: {
	act: NgwaAct;
	icon: React.ReactNode;
	className?: string;
	title?: string;
}) {
	return (
		<button
			type="button"
			className={className}
			disabled={act.disabledReason !== undefined}
			title={title ?? act.disabledReason}
			onClick={act.run}
		>
			{icon} {act.label}
		</button>
	);
}

function PickButton({ pick, label, icon }: { pick: NgwaScopePick; label: string; icon: React.ReactNode }) {
	const [open, setOpen] = useState(false);
	const ref = useRef<HTMLButtonElement>(null);
	const close = useCallback(() => setOpen(false), []);
	return (
		<span className="pickwrap">
			<button
				ref={ref}
				type="button"
				className="chip"
				aria-haspopup="menu"
				aria-expanded={open}
				disabled={pick.disabledReason !== undefined}
				title={pick.disabledReason}
				onClick={() => setOpen((o) => !o)}
			>
				{icon} {label}
			</button>
			{open && (
				<NgwaPopMenu
					pop={{ id: pick.title, title: pick.title, items: pick.targets }}
					anchor={ref}
					onClose={close}
					className="storepop"
				/>
			)}
		</span>
	);
}
