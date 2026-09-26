// D-06 route shell (WP-57): the pane-hosted `/settings/actions/$tab` route
// body. Header + tab bar + the active surface + the iyke footer line. Binds
// to the frozen G-ACTIONS-API (`src/lib/actions/store.ts` header, Round 42)
// — `useEffectiveModel()` is the one read every tab shares.
//
// The `actions` tab (plus its `empty`/`loading` variants) is built here.
// `editor`, `menus`, `keys` and `import` mount the wave 12e placeholder
// surfaces (item 19's mount contract, `ActionsSurfaceProps` in `./types`) —
// WP-58..61 each replace only their own folder's `index.tsx`.

import { useMemo, useState } from 'react';
import { Terminal } from 'lucide-react';
import { useNavigate } from '@tanstack/react-router';
import { ErrorState, LoadingState } from '@/components/states';
import { refreshActionsModel, useActionsStore, useEffectiveModel } from '@/lib/actions/store';
import type { ActionsScope } from '@/lib/actions/store';
import { useShellStore } from '@/lib/shell/shell-store';
import type { ActionsSurfaceProps, ActionsTabId, VisibleActionsTabId } from './types';
import { isVisibleActionsTab } from './types';
import { ActionsHeader } from './header';
import { ActionsTabs } from './tabs';
import { ActionsListSurface } from './actions-list';
import { EmptyActionsState } from './empty';
import { IssuesBanner } from './shared/issues-banner';
import { EditorSurface } from './editor';
import { MenusSurface } from './menus';
import { KeysSurface } from './keys';
import { ImportSurface } from './import';
import '@/shell/ngwa/ngwa.css';
import './actions.css';

function iykeCommandFor(tab: ActionsTabId, scope: ActionsScope): string {
	switch (tab) {
		case 'actions':
			return `actions list --scope ${scope}`;
		case 'editor':
			return `actions set --scope ${scope} <id>`;
		case 'menus':
			return `menus show <menu-id>`;
		case 'keys':
			return `keys list --scope ${scope}`;
		case 'import':
			return `actions import --from <source>`;
		default:
			return `actions list --scope ${scope}`;
	}
}

export interface ActionsShellProps {
	tab: ActionsTabId;
}

export function ActionsShell({ tab }: ActionsShellProps) {
	const navigate = useNavigate();
	const [scope, setScope] = useState<ActionsScope>('personal');
	const model = useEffectiveModel();
	const status = useActionsStore((s) => s.status);
	const error = useActionsStore((s) => s.error);

	const hasUserActions = useMemo(
		() => model.actions.some((a) => a.source === 'personal' || a.source === 'project'),
		[model.actions]
	);

	// Blocker 4: the empty state only ever means "read, and genuinely nothing
	// of the user's yet" — a `files === null` model (nothing read from disk
	// yet) must show loading, not flash `empty` first just because it also
	// has zero personal/project actions.
	const stillLoading = (status === 'idle' || status === 'loading') && model.files === null;
	// The first read failed: nothing was read, so neither `empty` nor the
	// list is true. (A failed *re*-read keeps the last model — blocker 5.)
	const neverRead = status === 'error' && model.files === null;

	const iykeCommand = iykeCommandFor(tab, scope);

	async function copyIyke() {
		try {
			await navigator.clipboard.writeText(`iyke ${iykeCommand}`);
		} catch {
			// clipboard access denied — nothing further to do
		}
	}

	function onNavigate(target: ActionsTabId, opts?: { action?: string }) {
		void navigate({
			to: '/settings/actions/$tab',
			params: { tab: target },
			search: opts?.action ? { action: opts.action } : undefined,
		});
	}

	const surfaceProps: ActionsSurfaceProps = { scope, model, onNavigate };

	function renderSurface() {
		if (stillLoading) {
			return <LoadingState data-state="loading" fill heading="Reading actions and keybindings…" />;
		}
		if (neverRead) {
			return (
				<ErrorState
					data-state="error"
					fill
					heading="Couldn't read your actions and keybindings"
					body={error ?? undefined}
					action={{
						label: 'Try again',
						onClick: () => void refreshActionsModel(useShellStore.getState().activeProject?.id ?? null),
					}}
				/>
			);
		}
		if (tab === 'actions') {
			return hasUserActions ? <ActionsListSurface model={model} scope={scope} /> : <EmptyActionsState model={model} />;
		}
		if (tab === 'editor') return <EditorSurface {...surfaceProps} />;
		if (tab === 'menus') return <MenusSurface {...surfaceProps} />;
		if (tab === 'keys') return <KeysSurface {...surfaceProps} />;
		return <ImportSurface {...surfaceProps} />;
	}

	// Every surface's `data-state` names the D-06 state it renders — the one
	// attribute the conformance pass (and `actions.css`'s empty-state rule,
	// item 17) greps for.
	const dataState = stillLoading ? 'loading' : neverRead ? 'error' : tab === 'actions' ? (hasUserActions ? 'actions' : 'empty') : tab;
	// `import` is routable but not one of the 1–4 tabs (item 19) — no tab
	// bar entry highlights while it's the active surface.
	const visibleTab: VisibleActionsTabId | null = isVisibleActionsTab(tab) ? tab : null;

	return (
		<div className="view-ngwa view-acts flex-1 min-h-0 flex flex-col">
			<ActionsHeader tab={tab} scope={scope} onScopeChange={setScope} model={model} />
			<ActionsTabs activeTab={visibleTab} model={model} />

			{/* Blocker 5: an invalid file, or a failed re-read, never replaces this
			    surface with `ErrorState` — the last good model stays visible and the
			    problem shows as a banner above it. */}
			<IssuesBanner model={model} readError={status === 'error' ? error : null} />

			<div className="surface on" data-surface={tab} data-state={dataState}>
				{renderSurface()}
			</div>

			<div className="iykeline">
				<Terminal className="h-3 w-3" />
				<b>iyke</b>
				<span>{iykeCommand}</span>
				<button type="button" className="cp" onClick={() => void copyIyke()}>
					Copy
				</button>
			</div>
		</div>
	);
}
