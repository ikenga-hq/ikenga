// WP-69 — Window 2 ⋯ (D-09 `popout`, `d9Win2`'s "Pane actions" menu):
//   Move back to main window · Make dispatch target · ─ · Close Window 2
//
// Lazy-loaded by the thin root (Radix menu stays out of the first paint).
// Minimise / restore are the OS window's own chrome. Closing the window
// returns every tab to the main window: the primary's detached-surface
// tracker sees them come back and mounts them (`onSurfacesReturned`).
//
// *Make dispatch target*: the dispatch target lives in the primary window's
// shell store, which a thin window never writes. It asks `main` instead
// (`requestMakeTarget` → `window://make-target`), and the primary selects
// the surface's seat, or its session when unseated.

import { MoreHorizontal } from 'lucide-react';
import { useState } from 'react';

import {
	DropdownMenu,
	DropdownMenuContent,
	DropdownMenuItem,
	DropdownMenuSeparator,
	DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { closeWindow } from '@/lib/tauri-cmd';
import { moveSurfaceBack, requestMakeTarget } from '@/lib/window/window-two';

export default function WindowActions({ label, surfaceId }: { label: string; surfaceId: string | null }) {
	const [error, setError] = useState<string | null>(null);
	const fail = (e: unknown) => setError(e instanceof Error ? e.message : String(e));

	return (
		<DropdownMenu>
			<DropdownMenuTrigger asChild>
				<button
					type="button"
					aria-label="Pane actions"
					title={error ?? 'Pane actions'}
					data-window-actions={label}
					className="grid size-6 shrink-0 place-items-center rounded-sm text-muted-foreground hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
				>
					<MoreHorizontal className="h-3.5 w-3.5" aria-hidden="true" />
				</button>
			</DropdownMenuTrigger>
			<DropdownMenuContent align="end" className="min-w-52">
				<DropdownMenuItem
					disabled={!surfaceId}
					onSelect={() => {
						if (!surfaceId) return;
						setError(null);
						void moveSurfaceBack(label, surfaceId).catch(fail);
					}}
				>
					Move back to main window
				</DropdownMenuItem>
				<DropdownMenuItem
					disabled={!surfaceId}
					onSelect={() => {
						if (!surfaceId) return;
						setError(null);
						void requestMakeTarget(surfaceId).catch(fail);
					}}
				>
					Make dispatch target
				</DropdownMenuItem>
				<DropdownMenuSeparator />
				<DropdownMenuItem
					title="Close Window 2 — its panes return to the main window"
					onSelect={() => {
						setError(null);
						void closeWindow(label).catch(fail);
					}}
				>
					Close Window 2
				</DropdownMenuItem>
			</DropdownMenuContent>
		</DropdownMenu>
	);
}
