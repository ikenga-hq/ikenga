// D-06 detail pane (WP-57): overview, placements, JSON preview, and the
// dacts (detail actions) row — the right-hand column of the Actions tab
// (`designs/actions.html`'s `#actionDetail`). Same shape as Ngwa's detail
// pane (`shell/ngwa/ngwa-detail-pane.tsx`): a `dhead` (title/badge/meta) over
// `dtabs`/`dbody`.

import { useState } from 'react';
import { ExternalLink, Keyboard, Lock, Pencil, Play } from 'lucide-react';
import {
	type ActionsScope,
	type EffectiveAction,
	bindingsFor,
	hideAction,
	LockedActionError,
} from '@/lib/actions/store';
import { openActionsFile } from '@/lib/actions/client';
import { ActionIcon } from './shared/action-icon';
import { Kbd } from './shared/kbd';
import { JsonPreview } from './shared/json-preview';
import { menuLabel } from './shared/menu-label';
import { runSummary } from './shared/run-label';
import { actionsPathLabel } from './header';

type DetailTab = 'overview' | 'placements' | 'json';

export interface ActionDetailProps {
	action: EffectiveAction;
	/** Blocker 3: the menu ids this action is actually visible in, derived by
	 *  the caller from `model.menus` (a built-in's own `placements` is always
	 *  `[]`) — not `action.placements`. */
	placements: readonly string[];
	scope: ActionsScope;
	projectId: string | null;
	projectRoot: string | null;
	onTestRun: (actionId: string) => void;
	onRebind: (actionId: string) => void;
}

function sourceFileLine(action: EffectiveAction, scope: ActionsScope, projectRoot: string | null): string {
	if (action.source === 'builtin') return 'shell built-in';
	if (action.source === 'package') return `${action.pkgId ?? 'package'} · manifest.json`;
	return actionsPathLabel(scope, projectRoot);
}

function jsonShape(action: EffectiveAction): unknown {
	if (action.userAction) return action.userAction;
	return {
		id: action.id,
		name: action.name,
		...(action.icon ? { icon: action.icon } : {}),
		description: action.description,
		run: action.run,
		...(action.pkgId ? { pkg: action.pkgId } : {}),
	};
}

export function ActionDetail({
	action,
	placements,
	scope,
	projectId,
	projectRoot,
	onTestRun,
	onRebind,
}: ActionDetailProps) {
	const [activeTab, setActiveTab] = useState<DetailTab>('overview');
	const [actionError, setActionError] = useState<string | null>(null);
	const bindings = bindingsFor(action.id);
	const summary = runSummary(action);
	const file = sourceFileLine(action, scope, projectRoot);

	async function handleOpenFile() {
		setActionError(null);
		try {
			await openActionsFile('actions', scope, projectId);
		} catch (err) {
			setActionError(err instanceof Error ? err.message : String(err));
		}
	}

	async function handleHide() {
		setActionError(null);
		try {
			await hideAction(scope, action.id);
		} catch (err) {
			if (err instanceof LockedActionError) setActionError(err.message);
			else setActionError(err instanceof Error ? err.message : String(err));
		}
	}

	return (
		<aside className="detailcol">
			<div className="dhead">
				<div className="dtitle">
					<ActionIcon icon={action.icon} className="h-4 w-4" />
					<h2>{action.name}</h2>
					<span className="v">{action.id}</span>
					<span className={`kind k-${action.source}`}>
						{action.source === 'builtin' ? 'Built-in' : action.source === 'package' ? 'Package' : 'Yours'}
					</span>
					{action.locked && (
						<span className="badge" title="Locked: can be reordered/rebound, never edited or hidden">
							<Lock className="h-3 w-3" /> locked
						</span>
					)}
				</div>
				<div className="dsub">
					<span>
						Run: <b>{summary.label}</b>
					</span>
					<span>Key {bindings.length > 0 ? <Kbd combo={bindings[0].key} /> : <Kbd combo={null} />}</span>
					<span className="mono">{file}</span>
				</div>
				{action.description && (
					<p className="note" style={{ marginTop: 'var(--space-2)', marginBottom: 0 }}>
						{action.description}
					</p>
				)}

				<div className="dacts">
					{action.editable ? (
						<>
							<button type="button" className="chip" onClick={() => onRebind(action.id)}>
								<Pencil className="h-3 w-3" /> Edit
							</button>
							<button type="button" className="chip" onClick={() => onRebind(action.id)}>
								<Keyboard className="h-3 w-3" /> Rebind…
							</button>
						</>
					) : (
						<>
							<button type="button" className="chip" onClick={() => onRebind(action.id)}>
								<Keyboard className="h-3 w-3" /> Rebind…
							</button>
							<button type="button" className="chip" onClick={() => void handleHide()}>
								Hide from menus
							</button>
						</>
					)}
					<button
						type="button"
						className="chip"
						title="Pending — routes to the Editor tab until WP-58/WP-53 land"
						onClick={() => onTestRun(action.id)}
					>
						<Play className="h-3 w-3" /> Test run
					</button>
					<button type="button" className="chip" onClick={() => void handleOpenFile()}>
						<ExternalLink className="h-3 w-3" /> Open file
					</button>
				</div>
				{actionError && (
					<p className="cempty" style={{ color: 'var(--danger)', marginTop: 'var(--space-2)' }} role="alert">
						{actionError}
					</p>
				)}
			</div>

			<div className="dtabs" role="tablist">
				<button
					type="button"
					role="tab"
					aria-selected={activeTab === 'overview'}
					className={`dtab ${activeTab === 'overview' ? 'on' : ''}`}
					onClick={() => setActiveTab('overview')}
				>
					Overview
				</button>
				<button
					type="button"
					role="tab"
					aria-selected={activeTab === 'placements'}
					className={`dtab ${activeTab === 'placements' ? 'on' : ''}`}
					onClick={() => setActiveTab('placements')}
				>
					Placements
				</button>
				<button
					type="button"
					role="tab"
					aria-selected={activeTab === 'json'}
					className={`dtab ${activeTab === 'json' ? 'on' : ''}`}
					onClick={() => setActiveTab('json')}
				>
					JSON
				</button>
			</div>

			<div className="dbody sc">
				{activeTab === 'overview' && (
					<div>
						<div className="drow">
							<span className="k2">id</span>
							<span className="val mono">{action.id}</span>
						</div>
						<div className="drow">
							<span className="k2">Run</span>
							<span className="val mono">{summary.label}</span>
						</div>
						<div className="drow">
							<span className="k2">When</span>
							<span className="val mono">{action.placements.find((p) => p.when)?.when ?? 'always'}</span>
						</div>
						<div className="drow">
							<span className="k2">Scope</span>
							<span className="val">
								{action.source === 'builtin'
									? 'Shell (built-in)'
									: action.source === 'package'
										? 'Package'
										: action.source}
							</span>
						</div>
						<div className="drow">
							<span className="k2">Source</span>
							<span className="val">
								{action.source === 'builtin' ? 'Built-in' : action.source === 'package' ? 'Package' : 'Yours'}
								{action.pkgId ? ` · ${action.pkgId}` : ''}
							</span>
						</div>
					</div>
				)}

				{activeTab === 'placements' && (
					<div>
						{placements.length === 0 ? (
							<div className="cempty">No placements — reachable only by its key or from iyke.</div>
						) : (
							<div className="placechips">
								{placements.map((at) => (
									<span key={at} className="grpchip">
										{menuLabel(at)}
									</span>
								))}
							</div>
						)}
					</div>
				)}

				{activeTab === 'json' && (
					<JsonPreview
						value={jsonShape(action)}
						caption={action.userAction ? undefined : 'Not stored in a file — derived from the effective model.'}
					/>
				)}
			</div>
		</aside>
	);
}
