// WP-58 — the Editor's Run section: the six-way segment (G-ACTIONS §8.1) and
// its per-kind body. `workflow` stays selectable (so a user sees where it
// would go) but is flagged disabled — the runner returns `no-workflow-runner`
// for it until a workflow runner exists (`01` §Phase 6, out of scope here).

import { useRef } from 'react';
import type { ActionRunKind, ChiTarget } from '@/lib/actions/client';
import { RUN_VARIABLES } from '@/lib/actions/client';
import { RUN_TYPES } from './form-model';

const CHI_TARGETS: readonly { id: ChiTarget; label: string }[] = [
	{ id: 'active', label: 'Active session' },
	{ id: 'new', label: 'New session' },
	{ id: 'engine', label: 'Pick engine' },
];

export interface RunFieldsProps {
	runType: ActionRunKind;
	onChangeRunType: (kind: ActionRunKind) => void;

	chiTarget: ChiTarget;
	onChangeChiTarget: (target: ChiTarget) => void;
	chiEngineId: string;
	onChangeChiEngineId: (value: string) => void;
	chiPrompt: string;
	onChangeChiPrompt: (value: string) => void;

	shellCommand: string;
	onChangeShellCommand: (value: string) => void;
	shellCwd: string;
	onChangeShellCwd: (value: string) => void;
	shellConfirm: boolean;
	onChangeShellConfirm: (value: boolean) => void;

	iykeRoute: string;
	onChangeIykeRoute: (value: string) => void;
	iykeMethod: 'GET' | 'POST';
	onChangeIykeMethod: (value: 'GET' | 'POST') => void;

	skillName: string;
	onChangeSkillName: (value: string) => void;

	workflowName: string;
	onChangeWorkflowName: (value: string) => void;

	openUrl: string;
	onChangeOpenUrl: (value: string) => void;
}

export function RunFields(props: RunFieldsProps) {
	const { runType, onChangeRunType } = props;
	const promptRef = useRef<HTMLTextAreaElement>(null);

	function insertVariable(name: string) {
		const el = promptRef.current;
		const token = `{{${name}}}`;
		if (!el) {
			props.onChangeChiPrompt(`${props.chiPrompt}${token}`);
			return;
		}
		const at = el.selectionStart ?? el.value.length;
		const next = `${el.value.slice(0, at)}${token}${el.value.slice(at)}`;
		props.onChangeChiPrompt(next);
		requestAnimationFrame(() => {
			el.focus();
			el.selectionStart = el.selectionEnd = at + token.length;
		});
	}

	return (
		<>
			<div className="seg" role="group" aria-label="Run type">
				{RUN_TYPES.map((r) => (
					<button
						key={r.id}
						type="button"
						className={runType === r.id ? 'on' : ''}
						onClick={() => onChangeRunType(r.id)}
					>
						{r.label}
					</button>
				))}
			</div>

			<div style={{ marginTop: 'var(--space-3)' }}>
				{runType === 'chi' && (
					<>
						<div className="q">
							<label className="lab" htmlFor="edPrompt">
								Prompt template
							</label>
							<div className="field area">
								<textarea
									id="edPrompt"
									ref={promptRef}
									rows={3}
									value={props.chiPrompt}
									onChange={(e) => props.onChangeChiPrompt(e.target.value)}
								/>
							</div>
							<div className="varchips">
								{RUN_VARIABLES.map((v) => (
									<button key={v} type="button" className="varchip" onClick={() => insertVariable(v)}>
										{'{{'}
										{v}
										{'}}'}
									</button>
								))}
							</div>
						</div>
						<div className="q">
							<span className="lab">Target</span>
							<div className="seg" role="group" aria-label="Chi target">
								{CHI_TARGETS.map((t) => (
									<button
										key={t.id}
										type="button"
										className={props.chiTarget === t.id ? 'on' : ''}
										onClick={() => props.onChangeChiTarget(t.id)}
									>
										{t.label}
									</button>
								))}
							</div>
							{props.chiTarget === 'engine' && (
								<div className="field" style={{ marginTop: 'var(--space-2)' }}>
									<input
										type="text"
										placeholder="engine id, e.g. gemini"
										value={props.chiEngineId}
										onChange={(e) => props.onChangeChiEngineId(e.target.value)}
									/>
								</div>
							)}
						</div>
					</>
				)}

				{runType === 'shell' && (
					<>
						<div className="q">
							<label className="lab" htmlFor="edCmd">
								Command
							</label>
							<div className="field">
								<input
									id="edCmd"
									type="text"
									value={props.shellCommand}
									onChange={(e) => props.onChangeShellCommand(e.target.value)}
								/>
							</div>
						</div>
						<div className="q">
							<label className="lab" htmlFor="edCwd">
								Working directory
							</label>
							<span className="hint">Defaults to {'{{project.root}}'} when left blank.</span>
							<div className="field">
								<input
									id="edCwd"
									type="text"
									placeholder="{{project.root}}"
									value={props.shellCwd}
									onChange={(e) => props.onChangeShellCwd(e.target.value)}
								/>
							</div>
						</div>
						<label className="ck">
							<input
								type="checkbox"
								checked={props.shellConfirm}
								onChange={(e) => props.onChangeShellConfirm(e.target.checked)}
							/>
							<span>
								<span className="t1">Confirm before it runs</span>
								<span className="t2">
									A shell action that writes should ask first — not a security control (§8.1), just a courtesy prompt.
								</span>
							</span>
						</label>
					</>
				)}

				{runType === 'iyke' && (
					<div className="q">
						<label className="lab" htmlFor="edRoute">
							iyke route
						</label>
						<span className="hint">Any bridge path, e.g. /pane/navigate.</span>
						<div style={{ display: 'flex', gap: 'var(--space-2)', marginTop: 'var(--space-2)' }}>
							<div className="field" style={{ flex: 1 }}>
								<input
									id="edRoute"
									type="text"
									value={props.iykeRoute}
									onChange={(e) => props.onChangeIykeRoute(e.target.value)}
								/>
							</div>
							<div className="seg" role="group" aria-label="HTTP method">
								{(['POST', 'GET'] as const).map((m) => (
									<button
										key={m}
										type="button"
										className={props.iykeMethod === m ? 'on' : ''}
										onClick={() => props.onChangeIykeMethod(m)}
									>
										{m}
									</button>
								))}
							</div>
						</div>
					</div>
				)}

				{runType === 'skill' && (
					<div className="q">
						<label className="lab" htmlFor="edSkill">
							Skill
						</label>
						<span className="hint">An installed skill's name.</span>
						<div className="field">
							<input
								id="edSkill"
								type="text"
								value={props.skillName}
								onChange={(e) => props.onChangeSkillName(e.target.value)}
							/>
						</div>
					</div>
				)}

				{runType === 'workflow' && (
					<div className="q">
						<label className="lab" htmlFor="edWorkflow">
							Workflow
						</label>
						<span className="hint">
							Disabled until a workflow runner exists — saved here, but every run returns "no runner yet."
						</span>
						<div className="field">
							<input
								id="edWorkflow"
								type="text"
								value={props.workflowName}
								onChange={(e) => props.onChangeWorkflowName(e.target.value)}
							/>
						</div>
					</div>
				)}

				{runType === 'open' && (
					<div className="q">
						<label className="lab" htmlFor="edUrl">
							View or URL
						</label>
						<span className="hint">A shell route (/…), a pkg:// pane route, or an http(s)/mailto link.</span>
						<div className="field">
							<input
								id="edUrl"
								type="text"
								value={props.openUrl}
								onChange={(e) => props.onChangeOpenUrl(e.target.value)}
							/>
						</div>
					</div>
				)}
			</div>
		</>
	);
}
