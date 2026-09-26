// D-06 Editor tab (WP-58): name, icon, description, the six-way run-type
// segment, placements, the key's `when`, the key recorder with its DEC-59
// inline conflict card, and scope — plus the live "it writes" preview and
// Save / Test run / Cancel. Binds to the frozen G-ACTIONS-API
// (`src/lib/actions/store.ts` header, Round 42) and G-ACTIONS
// (`plans/shell-ux-rearchitecture/drafts/actions-schema.md`, FROZEN Round 39).
//
// Built-ins and package actions refuse editing here (§9.1) — hide or rebind
// instead, from the Actions / Keys tabs. Deep links (`?action=<id>`, from the
// Actions tab's Edit / Test run and the Keys tab's Rebind) are read with
// `useSearch({ strict: false })` per the wave 12e mount contract, since
// `routeTree.gen.ts` is not regenerated until WP-63.

import { useEffect, useMemo, useState } from 'react';
import { useSearch } from '@tanstack/react-router';
import { ChevronLeft, FileCode, Lock, ShieldAlert, Terminal, X } from 'lucide-react';
import { EmptyState } from '@/components/states';
import {
	ActionsFileNotWritableError,
	ActionsValidationError,
	addKeybinding,
	bindingsFor,
	LowerScopeOverrideError,
	rebindKey,
	saveUserAction,
	type ActionsScope,
	unbindKey,
} from '@/lib/actions/store';
import { actionsTrustStatus, GATED_RUN_KINDS, type ActionTrust } from '@/lib/actions/client';
import { gatherRunVariables, runAction, type RunOutcome } from '@/lib/actions/runner';
import { iykePath } from '@/lib/actions/runner/iyke';
import { formatKeyLabel, isMacPlatform } from '@/lib/keymap/platform';
import { tryParseWhen } from '@/lib/keymap/when';
import { NgwaTrustSheet } from '@/shell/ngwa/ngwa-trust-sheet';
import { ActionIcon } from '../shared/action-icon';
import { actionsPathLabel } from '../header';
import type { ActionsSurfaceProps } from '../types';
import { ACTION_ICON_CHOICES } from './icons';
import {
	buildUserAction,
	derivedKeyWhen,
	effectiveKeyWhen,
	emptyForm,
	findEditorConflict,
	formFromAction,
	missingRunField,
	planKeybindingWrite,
	PLACEMENT_IDS,
	runRequiresField,
	slug,
	suggestedRestriction,
	type EditorFormState,
} from './form-model';
import { KeyFields } from './key-fields';
import { PlacementsFields } from './placements-fields';
import { PreviewPane } from './preview-pane';
import { RunFields } from './run-fields';
import './editor.css';

function keybindingsPathLabel(scope: 'personal' | 'project', projectRoot: string | null): string {
	return actionsPathLabel(scope, projectRoot).replace(/actions\.json$/, 'keybindings.json');
}

/** A Test run of `iyke` `POST` would mutate through the bridge (§8.1: the
 *  six variables go straight into the request body) — Round 42's decision
 *  keeps every Test run non-mutating, so this previews the method, route
 *  and body locally instead of ever calling the runner. `GET` reads, so it
 *  may run for real; `shell` previews inside the runner itself (already
 *  `testRun`-aware); `chi` / `skill` send and show the run id; `open` /
 *  `view` / `workflow` just run. */
interface IykeTestPreview {
	status: 'iyke-preview';
	method: 'POST';
	route: string;
	path: string | null;
	body: Record<string, string>;
}

type EditorTestOutcome = RunOutcome | IykeTestPreview;

function errorMessage(err: unknown): string {
	if (
		err instanceof ActionsFileNotWritableError ||
		err instanceof ActionsValidationError ||
		err instanceof LowerScopeOverrideError ||
		err instanceof Error
	) {
		return err.message;
	}
	return String(err);
}

export function EditorSurface({ scope, model, onNavigate }: ActionsSurfaceProps) {
	const search = useSearch({ strict: false }) as { action?: string };
	const actionId = search.action ?? null;
	const targetAction = actionId ? model.actionById.get(actionId) : undefined;
	const notFound = Boolean(actionId) && !targetAction;
	const refuseEdit = targetAction ? !targetAction.editable : false;

	const [form, setForm] = useState<EditorFormState>(emptyForm);
	// D-06's own Personal / Project segment (item 1 of the Round-42 review):
	// the editor's write target, independent of the header's Personal /
	// Project switch (`scope`, still read once below as the fallback for a
	// brand-new action). Read and written everywhere a write happens — the
	// save, the key writes, `PreviewPane`, the file-path labels, the trust
	// effect — never the header's `scope` directly.
	const [writeScope, setWriteScope] = useState<ActionsScope>(scope);
	const [saveError, setSaveError] = useState<string | null>(null);
	const [savedAt, setSavedAt] = useState<number | null>(null);
	const [saving, setSaving] = useState(false);
	const [unbindPending, setUnbindPending] = useState(false);
	const [unbindError, setUnbindError] = useState<string | null>(null);
	const [testing, setTesting] = useState(false);
	const [testResult, setTestResult] = useState<EditorTestOutcome | null>(null);
	const [trustEntry, setTrustEntry] = useState<ActionTrust | null>(null);
	const [trustSheetOpen, setTrustSheetOpen] = useState(false);

	// Loads (or resets) the form when the target action changes — keyed on
	// its id (a stable primitive), never on the `EffectiveAction` object
	// itself, which is a new reference on every re-merge and would otherwise
	// wipe in-progress edits every time the store republishes. `writeScope`'s
	// initial value is the loaded action's own `source` when it has one
	// (personal/project — moving scopes is a deliberate re-save, never
	// silently re-derived from the header once loaded); otherwise the
	// header's current `scope`, read once here on purpose — flipping the
	// header switch later must not blow away an in-progress choice.
	useEffect(() => {
		if (refuseEdit) return;
		if (targetAction) {
			const entry = bindingsFor(targetAction.id)[0] ?? null;
			setForm(formFromAction(targetAction, entry));
			setWriteScope(
				targetAction.source === 'personal' || targetAction.source === 'project' ? targetAction.source : scope
			);
		} else if (!actionId) {
			setForm(emptyForm());
			setWriteScope(scope);
		}
		setSaveError(null);
		setSavedAt(null);
		setTestResult(null);
		// eslint-disable-next-line react-hooks/exhaustive-deps
	}, [targetAction?.id, actionId, refuseEdit]);

	const platform = isMacPlatform() ? 'mac' : 'other';

	const testId = form.id || slug(form.name || 'untitled');

	// The current on-disk entry for this action's own command, any scope —
	// the reference `rebindKey` / `unbindKey` edit in place or override
	// (store.ts: "otherwise one negative rule ... and one positive ... are
	// appended"), always freshly derived from `model` so it reflects our own
	// prior writes once they land.
	const referenceKeyEntry = useMemo(
		() => (form.id ? (bindingsFor(form.id)[0] ?? null) : null),
		// eslint-disable-next-line react-hooks/exhaustive-deps
		[model.keymap.entries, form.id]
	);

	const conflict = useMemo(
		() => findEditorConflict(model.keymap.entries, testId, form, platform, model.keymap.held),
		[model.keymap.entries, model.keymap.held, testId, form, platform]
	);

	const whenCheck = tryParseWhen(effectiveKeyWhen(form));

	// Gated on `!form.idTouched`, not `!targetAction`: once a save lands,
	// `handleSave` sets `idTouched` (below) but the deep-link `?action=` in
	// the URL doesn't change, so `targetAction` stays `undefined` for a
	// brand-new action even after it exists — a second Save would otherwise
	// see its own freshly-merged id and refuse itself as a duplicate.
	const duplicateOf = !form.idTouched && testId ? (model.actionById.get(testId)?.name ?? null) : null;

	// Debounced: `form.id` tracks the live name slug for an untouched (brand
	// new) action, so typing a name would otherwise fire this network read
	// on every keystroke.
	useEffect(() => {
		let cancelled = false;
		if (writeScope !== 'project' || !model.projectId || !form.id || !GATED_RUN_KINDS.includes(form.runType)) {
			setTrustEntry(null);
			return;
		}
		const timer = setTimeout(() => {
			actionsTrustStatus(model.projectId)
				.then((status) => {
					if (!cancelled) setTrustEntry(status.actions.find((a) => a.id === form.id) ?? null);
				})
				.catch(() => {
					if (!cancelled) setTrustEntry(null);
				});
		}, 400);
		return () => {
			cancelled = true;
			clearTimeout(timer);
		};
	}, [writeScope, model, form.id, form.runType]);

	function update<K extends keyof EditorFormState>(key: K, value: EditorFormState[K]) {
		setForm((f) => ({ ...f, [key]: value }));
		setSavedAt(null);
	}

	function handleNameChange(name: string) {
		setForm((f) => ({ ...f, name, id: f.idTouched ? f.id : slug(name || 'untitled') }));
		setSavedAt(null);
	}

	async function handleSave() {
		setSaveError(null);
		if (!form.name.trim()) {
			setSaveError('Give it a name first.');
			return;
		}
		if (missingRunField(form)) {
			setSaveError(`This run kind needs ${runRequiresField(form.runType)}.`);
			return;
		}
		if (duplicateOf) {
			setSaveError(`"${testId}" is already used by "${duplicateOf}" — change the name.`);
			return;
		}
		if (!whenCheck.ok) {
			setSaveError(`The When field isn't valid: ${whenCheck.error.message}`);
			return;
		}
		const finalForm: EditorFormState = { ...form, id: testId, idTouched: true };
		setSaving(true);
		try {
			await saveUserAction(writeScope, buildUserAction(finalForm, writeScope));
			// The same plan `PreviewPane` renders (`planKeybindingWrite`) decides
			// which store call happens — the two can never diverge on what's
			// about to land in `keybindings.json`.
			const plan = planKeybindingWrite(finalForm, referenceKeyEntry, writeScope);
			if (plan.action === 'add') {
				await addKeybinding(writeScope, plan.rules[0]);
			} else if (plan.action === 'rebind' && referenceKeyEntry) {
				// Always pass an explicit `when` (never `rule.when`, which is
				// `undefined` for the "always" default) — `rebindKey` falls back to
				// the OLD entry's `when` on `undefined`, which would silently keep
				// a stale restriction instead of clearing it back to "always".
				await rebindKey(writeScope, referenceKeyEntry, finalForm.key, { when: effectiveKeyWhen(finalForm) });
			} else if (plan.action === 'unbind' && referenceKeyEntry) {
				await unbindKey(writeScope, referenceKeyEntry);
			}
			// Merge, don't replace: whatever was typed during the two awaits
			// above stays — only the id/idTouched this save settled are pinned.
			setForm((f) => ({ ...f, id: finalForm.id, idTouched: true }));
			setSavedAt(Date.now());
		} catch (err) {
			setSaveError(errorMessage(err));
		} finally {
			setSaving(false);
		}
	}

	function handleCancel() {
		onNavigate('actions', targetAction ? { action: targetAction.id } : undefined);
	}

	// Round 42's Test-run rule (also in the footer's Test run button title):
	// `shell` only ever previews (the runner is already `testRun`-aware);
	// `iyke` `POST` previews here instead, since sending it would mutate;
	// `iyke` `GET`, `chi`, `skill`, `open` and `workflow` run for real —
	// testing an agent action means sending it, and nothing else here
	// mutates through the bridge or touches `actions.json` / `keybindings.json`.
	async function handleTestRun() {
		setTestResult(null);
		setTesting(true);
		try {
			const run = buildRunSafe();
			if (run.kind === 'iyke' && (run.method ?? 'POST') === 'POST') {
				const body = await gatherRunVariables(run);
				setTestResult({ status: 'iyke-preview', method: 'POST', route: run.route, path: iykePath(run.route), body });
				return;
			}
			const outcome = await runAction(
				{ id: testId, name: form.name || testId, run, scope: writeScope },
				{ testRun: true, projectId: writeScope === 'project' ? model.projectId : null }
			);
			setTestResult(outcome);
		} finally {
			setTesting(false);
		}
	}

	function buildRunSafe() {
		// `runAction` reads `action.run` verbatim — build it fresh from the
		// live form so Test run always reflects exactly what's on screen,
		// saved or not (§8.2: Test run never writes).
		return buildUserAction(form, writeScope).run;
	}

	async function handleUnbindOther() {
		if (!conflict) return;
		setUnbindError(null);
		setUnbindPending(true);
		try {
			await unbindKey(writeScope, conflict.other);
		} catch (err) {
			setUnbindError(errorMessage(err));
		} finally {
			setUnbindPending(false);
		}
	}

	function handleRestrict() {
		update('whenTouched', true);
		update('whenValue', suggestedRestriction(form));
	}

	if (refuseEdit && targetAction) {
		return (
			<div className="edrefuse" data-state="editor">
				<EmptyState
					icon={Lock}
					heading={`"${targetAction.name}" is a built-in`}
					body="Built-in and package actions can't be edited here — hide them from a menu, or rebind their key, from the Actions and Keys tabs."
					action={{ label: 'Rebind its key…', onClick: () => onNavigate('keys', { action: targetAction.id }) }}
				/>
			</div>
		);
	}

	if (notFound) {
		return (
			<div className="edrefuse" data-state="editor">
				<EmptyState
					icon={FileCode}
					heading="That action no longer exists"
					body={`No action with id "${actionId}" is in force. It may have been deleted or reset.`}
					action={{ label: 'New action', onClick: () => onNavigate('editor') }}
				/>
			</div>
		);
	}

	const actionsPath = actionsPathLabel(writeScope, model.projectRoot);
	const keybindingsPath = keybindingsPathLabel(writeScope, model.projectRoot);
	const placementCount = PLACEMENT_IDS.filter((id) => form.placements[id]).length + form.extraPlacements.length;
	// D-06 item 1: moving an existing action to another scope is a save at
	// the new scope, not a move — the old file keeps its copy until it's
	// deleted there.
	const movingFrom =
		targetAction &&
		(targetAction.source === 'personal' || targetAction.source === 'project') &&
		targetAction.source !== writeScope
			? targetAction.source
			: null;

	return (
		<div className="edroot" data-state="editor">
			<div className="edwrap">
				<div className="edform">
					<div className="edcols">
					<div className="edcolA">
						<div className="subhead first">Identity</div>
						<div className="q">
							<label className="lab" htmlFor="edName">
								Name
							</label>
							<span className="hint">What it says in a menu and in the palette.</span>
							<div className="field">
								<input id="edName" type="text" value={form.name} onChange={(e) => handleNameChange(e.target.value)} />
							</div>
							<span className="hint" style={{ marginTop: 4 }}>
								id <span className="mono">{testId}</span>
							</span>
						</div>
						<div className="q">
							<span className="lab">Icon</span>
							<div className="iconpick" role="radiogroup" aria-label="Icon">
								{ACTION_ICON_CHOICES.map((name) => (
									<button
										key={name}
										type="button"
										role="radio"
										aria-checked={form.icon === name}
										className={form.icon === name ? 'on' : ''}
										aria-label={name}
										onClick={() => update('icon', name)}
									>
										<ActionIcon icon={name} className="h-4 w-4" />
									</button>
								))}
							</div>
							<div className="field" style={{ marginTop: 'var(--space-2)', maxWidth: 220 }}>
								<input
									type="text"
									aria-label="Custom Lucide icon name"
									placeholder="or type a Lucide name…"
									value={form.icon}
									onChange={(e) => update('icon', e.target.value)}
								/>
							</div>
						</div>
						<div className="q">
							<label className="lab" htmlFor="edDesc">
								Description
							</label>
							<div className="field area">
								<textarea
									id="edDesc"
									rows={2}
									value={form.description}
									onChange={(e) => update('description', e.target.value)}
								/>
							</div>
						</div>

						<div className="subhead">Run</div>
						<RunFields
							runType={form.runType}
							onChangeRunType={(v) => update('runType', v)}
							chiTarget={form.chiTarget}
							onChangeChiTarget={(v) => update('chiTarget', v)}
							chiEngineId={form.chiEngineId}
							onChangeChiEngineId={(v) => update('chiEngineId', v)}
							chiPrompt={form.chiPrompt}
							onChangeChiPrompt={(v) => update('chiPrompt', v)}
							shellCommand={form.shellCommand}
							onChangeShellCommand={(v) => update('shellCommand', v)}
							shellCwd={form.shellCwd}
							onChangeShellCwd={(v) => update('shellCwd', v)}
							shellConfirm={form.shellConfirm}
							onChangeShellConfirm={(v) => update('shellConfirm', v)}
							iykeRoute={form.iykeRoute}
							onChangeIykeRoute={(v) => update('iykeRoute', v)}
							iykeMethod={form.iykeMethod}
							onChangeIykeMethod={(v) => update('iykeMethod', v)}
							skillName={form.skillName}
							onChangeSkillName={(v) => update('skillName', v)}
							workflowName={form.workflowName}
							onChangeWorkflowName={(v) => update('workflowName', v)}
							openUrl={form.openUrl}
							onChangeOpenUrl={(v) => update('openUrl', v)}
						/>
						{writeScope === 'project' && GATED_RUN_KINDS.includes(form.runType) && (
							<div className="q">
								<span className="lab">Project trust</span>
								{trustEntry ? (
									<span className={`trustbadge ts-${trustEntry.state}`}>
										{trustEntry.state === 'trusted' && 'Trusted — this exact run is pinned'}
										{trustEntry.state === 'changed' && 'Changed since it was trusted — will re-ask'}
										{(trustEntry.state === 'untrusted' || trustEntry.state === 'absent') && 'Not trusted — will refuse to run'}
										{trustEntry.state === 'not-gated' && 'Not gated'}
									</span>
								) : (
									<span className="trustbadge ts-untrusted">
										Not trusted yet{!form.id ? ' — save it first' : ''}
									</span>
								)}
								{trustEntry?.state !== 'trusted' && form.id && (
									<button type="button" className="btn" style={{ marginTop: 'var(--space-2)' }} onClick={() => setTrustSheetOpen(true)}>
										<ShieldAlert className="h-3 w-3" /> Trust this project…
									</button>
								)}
							</div>
						)}
					</div>

					<div className="edcolB">
						<div className="subhead first">Placements</div>
						<PlacementsFields
							placements={form.placements}
							onToggle={(id, checked) => update('placements', { ...form.placements, [id]: checked })}
							filesGlob={form.filesGlob}
							onChangeFilesGlob={(v) => update('filesGlob', v)}
							sectionId={form.sectionId}
							onChangeSectionId={(v) => update('sectionId', v)}
							nativeTop={form.nativeTop}
							onChangeNativeTop={(v) => update('nativeTop', v)}
						/>

						<div className="subhead">Key</div>
						<KeyFields
							keyCombo={form.key}
							onChangeKey={(combo) => update('key', combo)}
							whenTouched={form.whenTouched}
							whenValue={form.whenValue}
							derivedWhen={derivedKeyWhen(form)}
							onChangeWhen={(v) => setForm((f) => ({ ...f, whenTouched: true, whenValue: v }))}
							whenInvalid={!whenCheck.ok}
							conflict={conflict}
							restrictionSuggestion={suggestedRestriction(form)}
							unbindPending={unbindPending}
							unbindError={unbindError}
							onUnbindOther={() => void handleUnbindOther()}
							onRestrict={handleRestrict}
							onChooseAnother={() => update('key', '')}
						/>

						<div className="subhead">Scope</div>
						<div className="q">
							<div className="seg" role="group" aria-label="Scope">
								<button
									type="button"
									className={writeScope === 'personal' ? 'on' : ''}
									onClick={() => setWriteScope('personal')}
								>
									Personal
								</button>
								<button
									type="button"
									className={writeScope === 'project' ? 'on' : ''}
									disabled={!model.projectRoot}
									onClick={() => setWriteScope('project')}
								>
									Project
								</button>
							</div>
							<span className="hint" style={{ display: 'block', marginTop: 'var(--space-2)' }}>
								Writes to <span className="mono">{actionsPath}</span>
								{writeScope === 'project' ? ' — committed, so your team gets it too.' : ' — this machine, every project.'}
							</span>
							{movingFrom && (
								<span className="hint" style={{ display: 'block', marginTop: 'var(--space-2)' }} role="alert">
									Saving here creates a copy at {writeScope === 'project' ? 'Project' : 'Personal'} scope — the{' '}
									{movingFrom === 'project' ? 'Project' : 'Personal'} copy at{' '}
									<span className="mono">{actionsPathLabel(movingFrom, model.projectRoot)}</span> stays until you
									delete it there.
								</span>
							)}
						</div>
					</div>
				</div>
			</div>

			<div className="edside">
				<PreviewPane
					form={form}
					model={model}
					scope={writeScope}
					referenceKeyEntry={referenceKeyEntry}
					actionsPath={actionsPath}
					keybindingsPath={keybindingsPath}
				/>
			</div>
			</div>

			{testResult && (
				<div className="eddrawer" role="region" aria-label="Test run output">
					<header>
						<Terminal className="h-3 w-3" />
						<span>Test run</span>
						<span className="nm2">{form.name || testId}</span>
						<button type="button" aria-label="Close output" onClick={() => setTestResult(null)}>
							<X className="h-3 w-3" />
						</button>
					</header>
					<pre role={testResult.status === 'failed' || testResult.status === 'refused' ? 'alert' : 'status'}>
						{testResult.status === 'preview' && (
							<>
								Not run — <b>{testResult.command}</b> in <b>{testResult.cwd ?? '~'}</b>
								{testResult.confirm ? ' (would ask to confirm first)' : ''}
								{'\n\n'}note a test run never writes actions.json.
							</>
						)}
						{testResult.status === 'iyke-preview' && (
							<>
								Not sent — <b>{testResult.method}</b> <b>{testResult.path ?? testResult.route}</b>
								{'\n'}body {JSON.stringify(testResult.body)}
								{'\n\n'}note a test run never writes actions.json or keybindings.json — a POST route mutates, so it only
								previews here.
							</>
						)}
						{testResult.status === 'done' && (
							<>
								Ran{testResult.runId ? ` · run ${testResult.runId}` : ''}
								{testResult.exec ? ` · exit ${testResult.exec.exitCode ?? 0}` : ''}
							</>
						)}
						{testResult.status === 'failed' && (
							<>
								<span className="e">Failed</span> — {testResult.message}
							</>
						)}
						{testResult.status === 'refused' && (
							<>
								<span className="e">Refused</span> — {testResult.message}
							</>
						)}
					</pre>
					{testResult.status === 'refused' && testResult.trustSheet && (
						<div className="acts" style={{ padding: '0 var(--space-3) var(--space-2)' }}>
							<button type="button" className="btn" onClick={() => setTrustSheetOpen(true)}>
								Trust this project…
							</button>
						</div>
					)}
				</div>
			)}

			{saveError && (
				<div className="testresult tr-failed" role="alert">
					{saveError}
				</div>
			)}

			<div className="formfoot">
				<button type="button" className="btn primary lg" disabled={saving} onClick={() => void handleSave()}>
					Save action
				</button>
				{savedAt && (
					<span className="hint" style={{ color: 'var(--success)' }}>
						Saved to {actionsPath}
						{form.key ? ` and ${keybindingsPath}` : ''}.
					</span>
				)}
				<button
					type="button"
					className="btn lg"
					disabled={testing}
					title="Test run never writes actions.json or keybindings.json: shell only previews, iyke POST previews here, and iyke GET / chi / skill / open / workflow run for real."
					onClick={() => void handleTestRun()}
				>
					Test run
				</button>
				<button type="button" className="btn lg ghost" onClick={handleCancel}>
					<ChevronLeft className="h-3 w-3" /> Cancel
				</button>
				<span className="hintline">
					Scope <b className="mono">{writeScope === 'project' ? 'Project' : 'Personal'}</b> · {placementCount} placement
					{placementCount === 1 ? '' : 's'} ·{' '}
					{form.key ? `key ${formatKeyLabel(form.key, { mac: platform === 'mac' })}` : 'no key'}
				</span>
			</div>

			{trustSheetOpen && (
				<NgwaTrustSheet
					open={trustSheetOpen}
					onOpenChange={setTrustSheetOpen}
					item={null}
					mode="project-actions"
					projectActions={{ projectId: writeScope === 'project' ? model.projectId : null, actionIds: [testId] }}
					onApproved={() => setTrustSheetOpen(false)}
				/>
			)}
		</div>
	);
}
