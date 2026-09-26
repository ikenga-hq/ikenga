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
import { ChevronLeft, FileCode, Keyboard, Lock, ShieldAlert } from 'lucide-react';
import { EmptyState } from '@/components/states';
import {
	ActionsFileNotWritableError,
	ActionsValidationError,
	addKeybinding,
	bindingsFor,
	LowerScopeOverrideError,
	rebindKey,
	saveUserAction,
	unbindKey,
} from '@/lib/actions/store';
import { actionsTrustStatus, GATED_RUN_KINDS, type ActionTrust } from '@/lib/actions/client';
import { runAction, type RunOutcome } from '@/lib/actions/runner';
import { isMacPlatform } from '@/lib/keymap/platform';
import { tryParseWhen } from '@/lib/keymap/when';
import { NgwaTrustSheet } from '@/shell/ngwa/ngwa-trust-sheet';
import { ActionIcon } from '../shared/action-icon';
import { actionsPathLabel } from '../header';
import type { ActionsSurfaceProps } from '../types';
import { ACTION_ICON_CHOICES } from './icons';
import {
	buildKeybindingRule,
	buildUserAction,
	derivedKeyWhen,
	effectiveKeyWhen,
	emptyForm,
	findEditorConflict,
	formFromAction,
	missingRunField,
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
	const [saveError, setSaveError] = useState<string | null>(null);
	const [savedAt, setSavedAt] = useState<number | null>(null);
	const [saving, setSaving] = useState(false);
	const [unbindPending, setUnbindPending] = useState(false);
	const [unbindError, setUnbindError] = useState<string | null>(null);
	const [testing, setTesting] = useState(false);
	const [testResult, setTestResult] = useState<RunOutcome | null>(null);
	const [trustEntry, setTrustEntry] = useState<ActionTrust | null>(null);
	const [trustSheetOpen, setTrustSheetOpen] = useState(false);

	// Loads (or resets) the form when the target action changes — keyed on
	// its id (a stable primitive), never on the `EffectiveAction` object
	// itself, which is a new reference on every re-merge and would otherwise
	// wipe in-progress edits every time the store republishes.
	useEffect(() => {
		if (refuseEdit) return;
		if (targetAction) {
			const entry = bindingsFor(targetAction.id)[0] ?? null;
			setForm(formFromAction(targetAction, entry));
		} else if (!actionId) {
			setForm(emptyForm());
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
		() => findEditorConflict(model.keymap.entries, testId, form, platform),
		[model.keymap.entries, testId, form, platform]
	);

	const whenCheck = tryParseWhen(effectiveKeyWhen(form));

	const duplicateOf =
		!targetAction && testId ? (model.actionById.get(testId)?.name ?? null) : null;

	useEffect(() => {
		let cancelled = false;
		if (scope !== 'project' || !model.projectId || !form.id || !GATED_RUN_KINDS.includes(form.runType)) {
			setTrustEntry(null);
			return;
		}
		actionsTrustStatus(model.projectId)
			.then((status) => {
				if (!cancelled) setTrustEntry(status.actions.find((a) => a.id === form.id) ?? null);
			})
			.catch(() => {
				if (!cancelled) setTrustEntry(null);
			});
		return () => {
			cancelled = true;
		};
	}, [scope, model, form.id, form.runType]);

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
			await saveUserAction(scope, buildUserAction(finalForm, scope));
			const rule = buildKeybindingRule(finalForm);
			if (rule) {
				// Always pass an explicit `when` (never `rule.when`, which is
				// `undefined` for the "always" default) — `rebindKey` falls back to
				// the OLD entry's `when` on `undefined`, which would silently keep
				// a stale restriction instead of clearing it back to "always".
				if (referenceKeyEntry) {
					await rebindKey(scope, referenceKeyEntry, rule.key, { when: effectiveKeyWhen(finalForm) });
				} else {
					await addKeybinding(scope, rule);
				}
			} else if (referenceKeyEntry) {
				await unbindKey(scope, referenceKeyEntry);
			}
			setForm(finalForm);
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

	async function handleTestRun() {
		setTestResult(null);
		setTesting(true);
		try {
			const outcome = await runAction(
				{ id: testId, name: form.name || testId, run: buildRunSafe(), scope },
				{ testRun: true, projectId: scope === 'project' ? model.projectId : null }
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
		return buildUserAction(form, scope).run;
	}

	async function handleUnbindOther() {
		if (!conflict) return;
		setUnbindError(null);
		setUnbindPending(true);
		try {
			await unbindKey(scope, conflict.other);
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
					data-state="editor"
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
					data-state="editor"
					icon={FileCode}
					heading="That action no longer exists"
					body={`No action with id "${actionId}" is in force. It may have been deleted or reset.`}
					action={{ label: 'New action', onClick: () => onNavigate('editor') }}
				/>
			</div>
		);
	}

	const actionsPath = actionsPathLabel(scope, model.projectRoot);
	const keybindingsPath = keybindingsPathLabel(scope, model.projectRoot);

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
						{scope === 'project' && GATED_RUN_KINDS.includes(form.runType) && (
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
							<span className="hint">
								<Keyboard className="h-3 w-3" style={{ display: 'inline', verticalAlign: '-2px' }} /> Set from the header's
								Personal / Project switch above.
							</span>
							<div className="field" style={{ marginTop: 'var(--space-2)' }}>
								<input type="text" readOnly value={scope === 'project' ? 'Project' : 'Personal'} />
							</div>
							<span className="hint" style={{ display: 'block', marginTop: 'var(--space-2)' }}>
								Writes to <span className="mono">{actionsPath}</span>
								{scope === 'project' ? ' — committed, so your team gets it too.' : ' — this machine, every project.'}
							</span>
						</div>
					</div>
				</div>
			</div>

			<div className="edside">
				<PreviewPane form={form} model={model} scope={scope} actionsPath={actionsPath} keybindingsPath={keybindingsPath} />
			</div>
			</div>

			{testResult && (
				<div className={`testresult tr-${testResult.status}`} role={testResult.status === 'failed' || testResult.status === 'refused' ? 'alert' : 'status'}>
					{testResult.status === 'preview' && (
						<>
							Not run — <span className="mono">{testResult.command}</span> in{' '}
							<span className="mono">{testResult.cwd ?? '~'}</span>
							{testResult.confirm ? ' (would ask to confirm first)' : ''}
						</>
					)}
					{testResult.status === 'done' && (
						<>
							Ran{testResult.runId ? ` · run ${testResult.runId}` : ''}
							{testResult.exec ? ` · exit ${testResult.exec.exitCode ?? 0}` : ''}
						</>
					)}
					{testResult.status === 'failed' && <>Failed — {testResult.message}</>}
					{testResult.status === 'refused' && (
						<>
							Refused — {testResult.message}
							{testResult.trustSheet && (
								<button type="button" className="btn" style={{ marginLeft: 'var(--space-2)' }} onClick={() => setTrustSheetOpen(true)}>
									Trust this project…
								</button>
							)}
						</>
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
				<button type="button" className="btn lg" disabled={testing} onClick={() => void handleTestRun()}>
					Test run
				</button>
				<button type="button" className="btn lg ghost" onClick={handleCancel}>
					<ChevronLeft className="h-3 w-3" /> Cancel
				</button>
				<span className="hintline">
					{savedAt
						? `Saved to ${actionsPath}${form.key ? ` and ${keybindingsPath}` : ''}.`
						: 'Test run never writes actions.json or keybindings.json. It sends chi, runs iyke/skill for real, and only previews shell without running it.'}
				</span>
			</div>

			{trustSheetOpen && (
				<NgwaTrustSheet
					open={trustSheetOpen}
					onOpenChange={setTrustSheetOpen}
					item={null}
					mode="project-actions"
					projectActions={{ projectId: scope === 'project' ? model.projectId : null, actionIds: [testId] }}
					onApproved={() => setTrustSheetOpen(false)}
				/>
			)}
		</div>
	);
}
