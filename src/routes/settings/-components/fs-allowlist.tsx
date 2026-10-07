// Folders this server lets you open — the fs allowlist every file, project and
// actions command checks (Rust `fs_roots`; on the daemon `server::rpc_fs_roots`).
// Imported by Settings → Storage. Your own list, with add / remove / reset;
// and, for an admin of a multi-user (T1) server, anyone's list by username
// (the broker routes those calls, `server::broker::fs_roots_admin`).
//
// Not the same thing as a project's extra roots (Settings → Projects): those
// say which folders a project shows; this says which folders may be opened.

import { FolderOpen, FolderPlus, RotateCcw, Trash2 } from 'lucide-react';
import { useCallback, useEffect, useState } from 'react';

import { Button } from '@/components/ui/button';
import { type AccessStatus, accessStatus } from '@/lib/access/client';
import {
	askForFoldersCopy,
	cannotEditRootsReason,
	isAbsoluteServerPath,
	isPermissionError,
} from '@/lib/fs-allowlist';
import { fsRootsAdd, fsRootsList, fsRootsRemove, fsRootsReset } from '@/lib/tauri-cmd';

const message = (err: unknown) => (err instanceof Error ? err.message : String(err));

interface RootsEditorProps {
	/** Someone else's list (an admin's view); omitted for the caller's own. */
	principal?: string;
	/** Set when this caller can't change the list: shown instead of the controls. */
	readOnlyReason?: string | null;
	askCopy: string;
	resetHint: string;
}

/** One folder list: rows with remove, an add-by-path field, reset. */
export function RootsEditor({ principal, readOnlyReason, askCopy, resetHint }: RootsEditorProps) {
	const [roots, setRoots] = useState<string[] | null>(null);
	const [loadError, setLoadError] = useState<string | null>(null);
	const [draft, setDraft] = useState('');
	const [busy, setBusy] = useState(false);
	const [actionError, setActionError] = useState<string | null>(null);
	const [refused, setRefused] = useState<string | null>(null);

	useEffect(() => {
		let cancelled = false;
		setRoots(null);
		setLoadError(null);
		fsRootsList(principal)
			.then((list) => {
				if (!cancelled) setRoots(Array.isArray(list) ? list : []);
			})
			.catch((err) => {
				if (!cancelled) setLoadError(message(err));
			});
		return () => {
			cancelled = true;
		};
	}, [principal]);

	const run = useCallback(async (op: () => Promise<string[]>) => {
		setBusy(true);
		setActionError(null);
		try {
			setRoots(await op());
			return true;
		} catch (err) {
			const m = message(err);
			if (isPermissionError(m)) setRefused(m);
			else setActionError(m);
			return false;
		} finally {
			setBusy(false);
		}
	}, []);

	async function add() {
		const path = draft.trim();
		if (!isAbsoluteServerPath(path)) {
			setActionError("Give the folder's full path, starting with /.");
			return;
		}
		if (await run(() => fsRootsAdd(path, principal))) setDraft('');
	}

	if (loadError) {
		return (
			<p className="text-xs text-red-600" role="alert" data-testid="fs-allowlist-error">
				Couldn't read the folder list — {loadError}
			</p>
		);
	}
	if (roots === null) {
		return <p className="text-xs text-muted-foreground">Loading…</p>;
	}
	const blocked = refused ?? readOnlyReason ?? null;

	return (
		<div className="space-y-2">
			<ul className="space-y-1 rounded-md border border-border bg-background">
				{roots.map((root) => (
					<li
						key={root}
						className="flex items-center justify-between gap-2 border-b border-border px-3 py-2 text-sm last:border-b-0"
					>
						<span className="flex min-w-0 items-center gap-2">
							<FolderOpen className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
							<span className="truncate font-mono text-xs" data-testid="fs-allowlist-root">
								{root}
							</span>
						</span>
						{!blocked && (
							<Button
								variant="ghost"
								size="sm"
								disabled={busy}
								onClick={() => void run(() => fsRootsRemove(root, principal))}
								className="h-7 px-2 text-muted-foreground hover:text-red-700"
								aria-label={`Remove ${root}`}
							>
								<Trash2 className="h-3.5 w-3.5" />
							</Button>
						)}
					</li>
				))}
				{roots.length === 0 && (
					<li className="px-3 py-3 text-xs text-muted-foreground" data-testid="fs-allowlist-empty">
						No folders — nothing can be opened until one is added.
					</li>
				)}
			</ul>
			{blocked ? (
				<p className="text-xs text-muted-foreground" data-testid="fs-allowlist-ask">
					{askCopy} <span className="text-[11px]">({blocked})</span>
				</p>
			) : (
				<>
					<div className="flex items-center gap-2">
						<input
							type="text"
							value={draft}
							onChange={(e) => setDraft(e.target.value)}
							onKeyDown={(e) => {
								if (e.key === 'Enter') void add();
							}}
							placeholder="/home/you/projects"
							aria-label="Folder to add"
							spellCheck={false}
							className="flex-1 rounded-md border border-border bg-transparent px-2 py-1 font-mono text-xs outline-none"
						/>
						<Button
							variant="outline"
							size="sm"
							disabled={busy || !draft.trim()}
							onClick={() => void add()}
						>
							<FolderPlus className="mr-1 h-3.5 w-3.5" />
							Add folder
						</Button>
						<Button
							variant="ghost"
							size="sm"
							disabled={busy}
							onClick={() => void run(() => fsRootsReset(principal))}
							title={resetHint}
						>
							<RotateCcw className="mr-1 h-3.5 w-3.5" />
							Reset
						</Button>
					</div>
					{actionError && (
						<p className="font-mono text-[11px] text-red-600" role="alert">
							{actionError}
						</p>
					)}
				</>
			)}
		</div>
	);
}

/** An admin's view of someone else's list on a T1 server, by username. */
function OtherPersonRoots({ status }: { status: AccessStatus }) {
	const [draft, setDraft] = useState('');
	const [shown, setShown] = useState<string | null>(null);
	return (
		<div className="space-y-2 border-t border-border pt-3" data-testid="fs-allowlist-admin">
			<p className="text-xs font-medium">Someone else's folders</p>
			<p className="text-xs text-muted-foreground">
				As an admin you can change which folders anyone on this server can open. Each person's
				workspace runs as their own system user, so a folder only shows them what that user can
				read.
			</p>
			<div className="flex items-center gap-2">
				<input
					type="text"
					value={draft}
					onChange={(e) => setDraft(e.target.value)}
					onKeyDown={(e) => {
						if (e.key === 'Enter' && draft.trim()) setShown(draft.trim());
					}}
					placeholder="username"
					aria-label="Username"
					spellCheck={false}
					className="w-48 rounded-md border border-border bg-transparent px-2 py-1 text-xs outline-none"
				/>
				<Button
					variant="outline"
					size="sm"
					disabled={!draft.trim()}
					onClick={() => setShown(draft.trim())}
				>
					Show folders
				</Button>
			</div>
			{shown && (
				<RootsEditor
					key={shown}
					principal={shown}
					askCopy={askForFoldersCopy(status)}
					resetHint="Back to their home folder"
				/>
			)}
		</div>
	);
}

export function FsAllowlistSectionBody() {
	// `undefined` while asking; `null` when there is no access status (the desktop app, or
	// a server that doesn't answer it) — the list still works, the server decides.
	const [status, setStatus] = useState<AccessStatus | null | undefined>(undefined);

	useEffect(() => {
		let cancelled = false;
		accessStatus()
			.then((s) => {
				if (!cancelled) setStatus(s && Array.isArray(s.caps) ? s : null);
			})
			.catch(() => {
				if (!cancelled) setStatus(null);
			});
		return () => {
			cancelled = true;
		};
	}, []);

	if (status === undefined) {
		return <p className="px-4 py-3 text-xs text-muted-foreground">Loading…</p>;
	}
	const multiUser = status?.tier === 't1';
	const isAdmin = multiUser && status.principal.isAdmin && status.adminStrength;
	return (
		<div className="space-y-3 px-4 py-3">
			<p className="text-xs text-muted-foreground">
				The folders you can open here — in the file browser, the folder picker and as projects.
				{multiUser ? ' Your list started with your home folder.' : ''}
			</p>
			<RootsEditor
				readOnlyReason={cannotEditRootsReason(status)}
				askCopy={askForFoldersCopy(status)}
				resetHint={multiUser ? 'Back to your home folder' : 'Back to no folders'}
			/>
			{isAdmin && status && <OtherPersonRoots status={status} />}
		</div>
	);
}
