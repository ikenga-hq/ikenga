import { useCallback } from 'react';
import { LayoutGrid } from 'lucide-react';
import { ListRow } from '@/components/ui/list-row';
import { usePaneStore } from '@/lib/panes/pane-store';
import { usePkgActivityBarEntries } from '@/lib/pkg/use-activity-bar-entries';
import type { ExplorerSectionContext } from '../section-registry';

export const viewsContextMenu = [
	{ id: 'open', label: 'Open', run: () => {} },
	{ id: 'open-side', label: 'Open to the Side', run: () => {} },
	{ id: 'pin-rail', label: 'Pin to rail', run: () => {} },
	{ id: 'open-ngwa', label: 'Open pkg in Ngwa', run: () => {} },
];

export function ViewsSection(_ctx: ExplorerSectionContext) {
	const { entries, loaded } = usePkgActivityBarEntries();

	const openView = useCallback((route: string) => {
		const { focusedId, addTab } = usePaneStore.getState();
		addTab(focusedId, { kind: 'route', path: route });
	}, []);

	const openNgwa = useCallback(() => {
		const { focusedId, addTab } = usePaneStore.getState();
		addTab(focusedId, { kind: 'route', path: '/ngwa/installed' });
	}, []);

	if (loaded && entries.length === 0) {
		return (
			<div className="p-4 text-center">
				<h3 className="text-sm font-semibold">No contributed views</h3>
				<p className="text-xs text-muted-foreground mt-1 mb-3">
					Packages contribute views to the Explorer.
				</p>
				<button
					type="button"
					onClick={openNgwa}
					className="text-xs bg-primary text-primary-foreground px-3 py-1.5 rounded hover:bg-primary/90 transition-colors"
				>
					Browse packages
				</button>
			</div>
		);
	}

	return (
		<div className="py-1">
			{entries.map((entry) => (
				<ListRow
					key={entry.id}
					size="sm"
					onActivate={() => openView(entry.route)}
					title={entry.label}
					className="w-full gap-1.5 px-2"
				>
					<LayoutGrid className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
					<span className="flex-1 truncate text-xs">{entry.label}</span>
				</ListRow>
			))}
		</div>
	);
}
