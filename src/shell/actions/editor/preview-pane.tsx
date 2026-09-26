// WP-58 — the Editor's right side (`edside`): the action sitting in the real
// menu it would land in, and "It writes" — the two files it would touch, from
// the real client shapes (never D-06's display-only previews, §11 item 7:
// no `"key"` inside the action, `command` is the bare id not `action:<id>`,
// `keybindings.json`'s top level is `{ bindings: [...] }` not a bare array).

import type { ActionsScope, EffectiveModel } from '@/lib/actions/store';
import { ActionIcon } from '../shared/action-icon';
import { Kbd } from '../shared/kbd';
import { JsonPreview } from '../shared/json-preview';
import { menuLabel } from '../shared/menu-label';
import {
	buildKeybindingRule,
	buildUserAction,
	menuIdForPlacement,
	PLACEMENT_IDS,
	type EditorFormState,
} from './form-model';

export interface PreviewPaneProps {
	form: EditorFormState;
	model: EffectiveModel;
	scope: ActionsScope;
	actionsPath: string;
	keybindingsPath: string;
}

export function PreviewPane({ form, model, scope, actionsPath, keybindingsPath }: PreviewPaneProps) {
	const draftId = form.id || 'untitled';
	const checkedIds = PLACEMENT_IDS.filter((id) => form.placements[id]);
	const previewMenuId = checkedIds
		.map((id) => menuIdForPlacement(id, form.sectionId, form.nativeTop))
		.find((menuId) => model.menus.get(menuId) !== null);

	const menu = previewMenuId ? model.menus.get(previewMenuId) : null;
	const rule = buildKeybindingRule(form);
	const actionJson = buildUserAction(form, scope);

	return (
		<>
			<div className="subhead first">Preview {previewMenuId ? `· ${menuLabel(previewMenuId)}` : ''}</div>
			<div className="menupreview" style={{ borderTop: 0, borderRadius: 'var(--radius-md)' }}>
				{menu ? (
					<div className="menu inline" role="presentation">
						{menu.items.map((item, i) =>
							item.kind === 'separator' ? (
								// biome-ignore lint/suspicious/noArrayIndexKey: a static default-order list never reorders
								<div key={i} className="msep" />
							) : (
								<div key={item.id} className="mitem">
									<ActionIcon icon={item.action.icon} className="h-3.5 w-3.5" />
									<span>{item.action.name}</span>
								</div>
							)
						)}
						<div className="mitem isnew">
							<ActionIcon icon={form.icon} className="h-3.5 w-3.5" />
							<span>{form.name || '(untitled)'}</span>
							{form.key && <Kbd combo={form.key} className="k" />}
						</div>
					</div>
				) : (
					<div className="note">Pick a placement to see where it lands.</div>
				)}
			</div>
			{checkedIds.length > 1 && (
				<div className="note" style={{ marginTop: 'var(--space-2)' }}>
					Also in{' '}
					{checkedIds
						.map((id) => menuIdForPlacement(id, form.sectionId, form.nativeTop))
						.filter((id) => id !== previewMenuId)
						.map((id) => (
							<span key={id} className="pchip">
								{menuLabel(id)}
							</span>
						))}
				</div>
			)}

			<div className="subhead">It writes</div>
			<div className="jsonbox">
				<h4>
					{actionsPath}
					<span className="rt">live</span>
				</h4>
				<JsonPreview value={actionJson} />
			</div>
			{rule && (
				<div className="jsonbox" style={{ marginTop: 'var(--space-3)' }}>
					<h4>{keybindingsPath}</h4>
					<JsonPreview value={{ bindings: [rule] }} caption={`the rule for "${draftId}" — the rest of the file is untouched`} />
				</div>
			)}
		</>
	);
}
