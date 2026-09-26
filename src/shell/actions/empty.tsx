// D-06 `empty` state (WP-57): "No actions of your own yet". Item 15 (D-06
// conformance): the generic `EmptyState` primitive (small icon, caption-sized
// heading) doesn't match `designs/actions.html`'s `.emptyview` markup — ember
// dots, a display-font `<h2>`, a large primary button, and the Chi prompt as
// its own mono note below the body copy — so this builds that markup
// directly, reusing the shell's existing `.ember-dots` motif (the same one
// `LoadingState` uses) rather than introducing a second one. Still exactly
// one action (New action), per the D-07 `states` contact sheet's rule that
// an empty state offers exactly one next step; `data-state="empty"` is kept
// on the surface root so the conformance pass can still grep for it.

import { Plus } from 'lucide-react';
import { useNavigate } from '@tanstack/react-router';
import { Button } from '@/components/ui/button';
import type { EffectiveModel } from '@/lib/actions/store';

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
		<div data-state="empty" className="emptyview">
			<span className="ember-dots" aria-hidden="true">
				<i />
				<i />
				<i />
			</span>
			<h2>No actions of your own yet</h2>
			<p className="note">
				Built-in and package actions are already doing their work — {builtinAndPackageCount} of them, in{' '}
				{menuCount} menus. An action of your own is a name, something to run, and the places it should appear.
			</p>
			<Button
				size="lg"
				onClick={() => void navigate({ to: '/settings/actions/$tab', params: { tab: 'editor' } })}
			>
				<Plus className="h-4 w-4" />
				New action
			</Button>
			<span className="note mono">
				Or ask a Chi: "add an action that runs scripts/pulse/build-all.sh from the palette"
			</span>
		</div>
	);
}
