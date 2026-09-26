// D-06 Menus tab (WP-59): "Add action…" — search/pick a not-yet-placed
// action and append it to the current menu. `CommandDialog` (the same
// cmdk-backed picker the ⌘K palette itself is built from) already renders
// its own `Command` root around `children` — this only ever supplies
// `CommandInput`/`CommandList`/… as those children, never a second nested
// `Command`, which would fight cmdk's single-context filtering. Each item's
// `value` is a name+id composite so cmdk's default fuzzy filter matches on
// either; Radix unmounts `DialogContent` on close, so the search text and
// cmdk's internal filter state reset for free on the next open.

import { CommandDialog, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList } from '@/components/ui/command';
import type { EffectiveAction } from '@/lib/actions/store';
import { ActionIcon } from '../shared/action-icon';

const SOURCE_LABEL: Record<string, string> = {
	builtin: 'Built-in',
	package: 'Package',
	personal: 'Yours',
	project: 'Yours',
};

export interface AddActionDialogProps {
	open: boolean;
	onOpenChange: (open: boolean) => void;
	/** Every action not already a row in the current menu, in model order. */
	candidates: readonly EffectiveAction[];
	onPick: (actionId: string) => void;
}

export function AddActionDialog({ open, onOpenChange, candidates, onPick }: AddActionDialogProps) {
	return (
		<CommandDialog open={open} onOpenChange={onOpenChange} title="Add action…" description="Search actions to add to this menu">
			<CommandInput placeholder="Search actions by name, id or description…" />
			<CommandList>
				<CommandEmpty>No matching actions.</CommandEmpty>
				<CommandGroup heading={`${candidates.length} available`}>
					{candidates.map((action) => (
						<CommandItem key={action.id} value={`${action.name} ${action.id} ${action.description}`} onSelect={() => onPick(action.id)}>
							<ActionIcon icon={action.icon} className="h-3.5 w-3.5 shrink-0" />
							<span className="flex min-w-0 flex-1 flex-col">
								<span className="truncate">{action.name}</span>
								<span className="truncate text-[10px] text-muted-foreground">{action.id}</span>
							</span>
							<span className="text-[10px] text-muted-foreground">{SOURCE_LABEL[action.source] ?? action.source}</span>
						</CommandItem>
					))}
				</CommandGroup>
			</CommandList>
		</CommandDialog>
	);
}
