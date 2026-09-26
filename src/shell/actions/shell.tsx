// D-06 route shell (WP-57): the pane-hosted `/settings/actions/$tab` route
// body. Header + tab bar + the active surface + the iyke footer line. Binds
// to the frozen G-ACTIONS-API (`src/lib/actions/store.ts` header, Round 42)
// — `useEffectiveModel()` is the one read every tab shares.
//
// Only the `actions` tab (plus its `empty` variant) is built here; `editor`,
// `menus`, `keys` and `import` are WP-58..61's own directories
// (`src/shell/actions/{editor,menus,keys,import}/`, do-not-touch for this
// WP) — this shell leaves a plain placeholder for them that those WPs
// replace with a real import.

import { useMemo, useState } from 'react';
import { Terminal } from 'lucide-react';
import { ErrorState, LoadingState } from '@/components/states';
import { refreshActionsModel, useActionsStore, useEffectiveModel } from '@/lib/actions/store';
import type { ActionsScope } from '@/lib/actions/types';
import type { ActionsTabId } from './types';
import { ActionsHeader } from './header';
import { ActionsTabs } from './tabs';
import { ActionsListSurface } from './actions-list';
import { EmptyActionsState } from './empty';
import '@/shell/ngwa/ngwa.css';
import './actions.css';

const TAB_LABEL: Record<ActionsTabId, string> = {
	actions: 'Actions',
	editor: 'Editor',
	menus: 'Menus',
	keys: 'Keys',
};

function iykeLineFor(tab: ActionsTabId, scope: ActionsScope): string {
	switch (tab) {
		case 'actions':
			return `iyke actions list --scope ${scope}`;
		case 'editor':
			return `iyke actions set --scope ${scope} <id>`;
		case 'menus':
			return `iyke menus show <menu-id>`;
		case 'keys':
			return `iyke keys list --scope ${scope}`;
		default:
			return `iyke actions list --scope ${scope}`;
	}
}

export interface ActionsShellProps {
	tab: ActionsTabId;
}

export function ActionsShell({ tab }: ActionsShellProps) {
	const [scope, setScope] = useState<ActionsScope>('personal');
	const model = useEffectiveModel();
	const status = useActionsStore((s) => s.status);
	const error = useActionsStore((s) => s.error);

	const hasUserActions = useMemo(
		() => model.actions.some((a) => a.source === 'personal' || a.source === 'project'),
		[model.actions]
	);

	const iykeLine = iykeLineFor(tab, scope);

	async function copyIyke() {
		try {
			await navigator.clipboard.writeText(iykeLine);
		} catch {
			// clipboard access denied — nothing further to do
		}
	}

	return (
		<div className="view-ngwa view-acts flex-1 min-h-0 flex flex-col">
			<ActionsHeader tab={tab} scope={scope} onScopeChange={setScope} model={model} />
			<ActionsTabs activeTab={tab} />

			{status === 'loading' && model.actions.length === 0 ? (
				<LoadingState data-state="loading" fill heading="Reading actions and keybindings…" />
			) : status === 'error' && error ? (
				<ErrorState
					data-state="error"
					fill
					heading="Couldn't read the actions files"
					body={error}
					action={{ label: 'Retry', onClick: () => void refreshActionsModel() }}
				/>
			) : tab === 'actions' ? (
				hasUserActions ? (
					<div className="surface on" data-surface="actions" data-state="actions">
						<ActionsListSurface model={model} />
					</div>
				) : (
					<div className="surface on" data-surface="empty">
						<EmptyActionsState model={model} />
					</div>
				)
			) : (
				<div className="surface on" data-surface={tab}>
					<div className="empty">The {TAB_LABEL[tab]} tab lands in a follow-up work package (WP-58..61).</div>
				</div>
			)}

			<div className="iykeline">
				<Terminal className="h-3 w-3" />
				<b>iyke</b>
				<span>{iykeLine}</span>
				<button type="button" className="cp" onClick={() => void copyIyke()}>
					Copy
				</button>
			</div>
		</div>
	);
}
