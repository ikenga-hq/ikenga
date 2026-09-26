// D-06 `empty` state (WP-57): "No actions of your own yet" — exactly one
// action (New action), per the shared `src/components/states/*` contract
// (D-07 `states` contact sheet: an empty state offers exactly one next
// step). Shown when the effective model has zero personal/project actions —
// built-ins and packages still do their work, so the copy says how many.

import { Sparkles } from 'lucide-react';
import { useNavigate } from '@tanstack/react-router';
import { EmptyState } from '@/components/states';
import type { EffectiveModel } from '@/lib/actions/merge';

export interface EmptyActionsStateProps {
	model: EffectiveModel;
}

export function EmptyActionsState({ model }: EmptyActionsStateProps) {
	const navigate = useNavigate();
	const builtinAndPackageCount = model.actions.filter(
		(a) => a.source === 'builtin' || a.source === 'package'
	).length;
	const menuCount = model.menus.ids.length;

	return (
		<EmptyState
			data-state="empty"
			fill
			icon={Sparkles}
			heading="No actions of your own yet"
			body={
				<>
					Built-in and package actions are already doing their work — {builtinAndPackageCount} of them, in{' '}
					{menuCount} menus. An action of your own is a name, something to run, and the places it should
					appear. Or ask a Chi: "add an action that runs scripts/pulse/build-all.sh from the palette".
				</>
			}
			action={{
				label: 'New action',
				onClick: () => void navigate({ to: '/settings/actions/$tab', params: { tab: 'editor' } }),
			}}
		/>
	);
}
