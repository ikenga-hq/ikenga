// D-06 detail pane (WP-57): overview, JSON preview, test-run entry point —
// the right-hand column of the Actions tab (`designs/actions.html`'s
// `#actionDetail`). Same shape as Ngwa's detail pane
// (`shell/ngwa/ngwa-detail-pane.tsx`): a `dhead` (title/badge/meta) over
// `dtabs`/`dbody`.

import { useState } from 'react';
import { Lock, Play, Shield } from 'lucide-react';
import { bindingsFor } from '@/lib/actions/store';
import type { EffectiveAction } from '@/lib/actions/registry';
import { Kbd } from './shared/kbd';
import { JsonPreview } from './shared/json-preview';
import { menuLabel } from './shared/menu-label';

type DetailTab = 'overview' | 'json' | 'test';

export interface ActionDetailProps {
	action: EffectiveAction;
	onTestRun: (actionId: string) => void;
}

function runSummary(action: EffectiveAction): { label: string; rows: Array<[string, string]> } {
	const run = action.run;
	switch (run.kind) {
		case 'builtin':
			return { label: 'Built-in behaviour', rows: [] };
		case 'dispatch':
			return {
				label: 'Dispatch to Chi',
				rows: [
					['prompt', run.prompt],
					...(run.target ? ([['target', run.target]] as Array<[string, string]>) : []),
				],
			};
		case 'view':
			return { label: 'Open view', rows: [['route', run.route]] };
		case 'chi':
			return {
				label: 'Dispatch to Chi',
				rows: [
					['target', run.target],
					...(run.engineId ? ([['engine', run.engineId]] as Array<[string, string]>) : []),
					['prompt', run.prompt],
				],
			};
		case 'shell':
			return {
				label: 'Shell command',
				rows: [
					['command', run.command],
					...(run.cwd ? ([['cwd', run.cwd]] as Array<[string, string]>) : []),
					['confirm before run', run.confirm ? 'yes' : 'no'],
				],
			};
		case 'iyke':
			return { label: 'iyke route', rows: [['route', run.route], ['method', run.method ?? 'GET']] };
		case 'skill':
			return { label: 'Skill', rows: [['skill', run.skill]] };
		case 'workflow':
			return { label: 'Workflow', rows: [['workflow', run.workflow]] };
		case 'open':
			return { label: 'Open URL / view', rows: [['url', run.url]] };
		default:
			return { label: 'Unknown run kind', rows: [] };
	}
}

function jsonShape(action: EffectiveAction): unknown {
	if (action.userAction) return action.userAction;
	return {
		id: action.id,
		name: action.name,
		...(action.icon ? { icon: action.icon } : {}),
		description: action.description,
		run: action.run,
		placements: action.placements,
		...(action.pkgId ? { pkg: action.pkgId } : {}),
	};
}

export function ActionDetail({ action, onTestRun }: ActionDetailProps) {
	const [activeTab, setActiveTab] = useState<DetailTab>('overview');
	const bindings = bindingsFor(action.id);
	const summary = runSummary(action);
	const testable = action.source === 'personal' || action.source === 'project';

	return (
		<aside className="detailcol">
			<div className="dhead">
				<div className="dtitle">
					<h2>{action.name}</h2>
					<span className="v">{action.id}</span>
					<span className={`kind k-${action.source}`}>{action.source}</span>
					{action.locked && (
						<span className="badge" title="Locked: can be reordered/rebound, never edited or hidden">
							<Lock className="h-3 w-3" /> locked
						</span>
					)}
				</div>
				<div className="dsub">
					<span>
						run: <b>{summary.label}</b>
					</span>
					{!action.editable && (
						<span>
							<b>Built-in</b> — hide or rebind it, edit is not possible
						</span>
					)}
					{action.pkgId && (
						<span>
							pkg: <span className="mono">{action.pkgId}</span>
						</span>
					)}
				</div>
				{action.description && (
					<p className="note" style={{ marginTop: 'var(--space-2)', marginBottom: 0 }}>
						{action.description}
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
					aria-selected={activeTab === 'json'}
					className={`dtab ${activeTab === 'json' ? 'on' : ''}`}
					onClick={() => setActiveTab('json')}
				>
					JSON
				</button>
				<button
					type="button"
					role="tab"
					aria-selected={activeTab === 'test'}
					className={`dtab ${activeTab === 'test' ? 'on' : ''}`}
					onClick={() => setActiveTab('test')}
				>
					Test run
				</button>
			</div>

			<div className="dbody sc">
				{activeTab === 'overview' && (
					<div>
						<div className="runbody">
							{summary.rows.map(([k, v]) => (
								<div key={k} className="drow">
									<span className="k2">{k}</span>
									<span className="val mono">{v}</span>
								</div>
							))}
						</div>

						<div className="drow">
							<span className="k2">key</span>
							<span className="val">
								{bindings.length > 0 ? <Kbd combo={bindings[0].key} /> : <Kbd combo={null} />}
							</span>
						</div>

						<div className="subhead first">Placements</div>
						{action.placements.length === 0 ? (
							<div className="cempty">No placements — reachable only by key or `iyke`.</div>
						) : (
							<div className="placechips">
								{action.placements.map((p) => (
									<span key={p.at} className="grpchip" title={p.when ?? undefined}>
										{menuLabel(p.at)}
										{p.when ? ` · ${p.when}` : ''}
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

				{activeTab === 'test' && (
					<div className="runbody">
						{testable ? (
							<>
								<p className="note">Test run never writes and returns a run id for a `chi` action.</p>
								<button type="button" className="chip" onClick={() => onTestRun(action.id)}>
									<Play className="h-3 w-3" /> Test run in the Editor
								</button>
							</>
						) : (
							<p className="note">
								<Shield className="h-3 w-3" style={{ display: 'inline', verticalAlign: '-2px' }} /> Built-in and
								package actions cannot be test-run from here — make a copy of your own to try it.
							</p>
						)}
					</div>
				)}
			</div>
		</aside>
	);
}
