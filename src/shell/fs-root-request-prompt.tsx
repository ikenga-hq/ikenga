// Allow / Deny for a folder a bridge caller asked to open (`POST
// /iyke/fs-roots/add`, Rust `iyke/fs_roots_routes.rs`). The shell emits
// `iyke://fs-roots-add-request` only for a folder the allowlist doesn't
// already cover, and adds it only if the user clicks Allow here. Closing the
// dialog any other way (Esc, clicking outside) counts as Deny. Requests that
// arrive while one is open wait their turn. Desktop app only: the bridge is
// the desktop's localhost server.

import { FolderOpen } from 'lucide-react';
import { useEffect, useState } from 'react';

import { Button } from '@/components/ui/button';
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from '@/components/ui/dialog';
import { type FsRootAddRequest, iykeFsRootsRequestDone, listen } from '@/lib/tauri-cmd';
import { isTauri } from '@/lib/transport';

export const FS_ROOTS_ADD_REQUEST_EVENT = 'iyke://fs-roots-add-request';

export function FsRootRequestPrompt() {
	const [queue, setQueue] = useState<FsRootAddRequest[]>([]);

	useEffect(() => {
		if (!isTauri()) return;
		let cancelled = false;
		let unlisten: (() => void) | undefined;
		listen<FsRootAddRequest>(FS_ROOTS_ADD_REQUEST_EVENT, (e) => {
			setQueue((q) => [...q, e.payload]);
		})
			.then((fn) => {
				if (cancelled) fn();
				else unlisten = fn;
			})
			.catch((err) => console.warn('[fs-root-request] listen failed', err));
		return () => {
			cancelled = true;
			unlisten?.();
		};
	}, []);

	const current = queue[0];

	function answer(allow: boolean) {
		if (!current) return;
		setQueue((q) => q.slice(1));
		// A request the shell already gave up on (no answer in time) has no
		// pending entry any more; that rejection is expected and harmless.
		iykeFsRootsRequestDone(current.request_id, allow).catch((err) =>
			console.warn('[fs-root-request] answer not delivered', err)
		);
	}

	return (
		<Dialog
			open={current !== undefined}
			onOpenChange={(open) => {
				if (!open) answer(false);
			}}
		>
			{current && (
				<DialogContent showCloseButton={false} data-testid="fs-root-request">
					<DialogHeader>
						<DialogTitle>Let Ikenga open this folder?</DialogTitle>
						<DialogDescription>
							A tool connected to Ikenga (an agent or the iyke command line) wants to open files in
							this folder.
						</DialogDescription>
					</DialogHeader>
					<div className="space-y-2 text-sm">
						<div className="flex items-start gap-2 rounded-md border border-border bg-background p-3">
							<FolderOpen className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground" />
							<div className="min-w-0 space-y-1">
								<div className="break-all font-mono text-xs" data-testid="fs-root-request-folder">
									{current.resolved}
								</div>
								{current.path !== current.resolved && (
									<div className="break-all text-[11px] text-muted-foreground">
										Asked for as <span className="font-mono">{current.path}</span>
									</div>
								)}
							</div>
						</div>
						{current.reason && (
							<p className="text-xs" data-testid="fs-root-request-reason">
								<span className="text-muted-foreground">Reason given: </span>
								{current.reason}
							</p>
						)}
						<p className="text-xs text-muted-foreground">
							Allowing it lets Ikenga open everything inside this folder. You can remove it later in
							Settings → Storage → Folders you can open.
						</p>
						{queue.length > 1 && (
							<p className="text-[11px] text-muted-foreground">
								{queue.length - 1} more request{queue.length > 2 ? 's' : ''} waiting.
							</p>
						)}
					</div>
					<DialogFooter>
						<Button variant="outline" onClick={() => answer(false)}>
							Deny
						</Button>
						<Button onClick={() => answer(true)}>Allow</Button>
					</DialogFooter>
				</DialogContent>
			)}
		</Dialog>
	);
}
