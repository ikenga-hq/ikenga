// D-08 `artifact-history` variant — version list drawer with side-by-side
// compare (designs/pane-chrome.html?state=artifact-history, `historyBody()`).
//
// See `use-artifact-versions.ts` for why the list is empty-but-real today:
// no git-log or snapshot backend exists in this codebase yet. This panel is
// written against the full shape so it needs no changes once one lands —
// today it always takes the `EmptyState` branch.

import { useState } from 'react';
import { History, X } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { EmptyState } from '@/components/states';
import { ViewerRouter } from '../auto-router';
import { type ArtifactVersion, useArtifactVersions } from './use-artifact-versions';

interface VersionHistoryPanelProps {
	path: string;
	onClose: () => void;
}

export function VersionHistoryPanel({ path, onClose }: VersionHistoryPanelProps) {
	const { versions } = useArtifactVersions(path);
	const [selectedId, setSelectedId] = useState('v-now');

	// No real history source is wired up (see use-artifact-versions.ts) — the
	// hook only ever returns the working-tree entry today.
	if (versions.length <= 1) {
		return (
			<div className="flex h-full w-full flex-col" data-state="artifact-history-empty">
				<HistoryHeader count={0} onClose={onClose} />
				<EmptyState
					data-state="artifact-history-empty"
					fill
					icon={History}
					heading="No version history yet"
					body="Version history needs a git-log or snapshot source — neither is wired up in this build. The file's working-tree contents are always shown in the pane itself."
					action={{ label: 'Close', onClick: onClose }}
				/>
			</div>
		);
	}

	const selected = versions.find((v) => v.id === selectedId) ?? versions[0];
	const isNow = selected.id === 'v-now';

	return (
		<div className="flex h-full w-full" data-state="artifact-history">
			<div className="flex min-w-0 flex-1 flex-col">
				{isNow ? (
					<ViewerRouter path={path} source="pane" chromeless />
				) : (
					<div className="grid h-full min-h-0 grid-cols-2 divide-x divide-border">
						<div className="flex min-h-0 flex-col">
							<CompareSideHeader label={selected.label} tag={selected.when} />
							<div className="min-h-0 flex-1">
								<ViewerRouter path={path} source="pane" chromeless />
							</div>
						</div>
						<div className="flex min-h-0 flex-col">
							<CompareSideHeader label="Working tree" tag="now" now />
							<div className="min-h-0 flex-1">
								<ViewerRouter path={path} source="pane" chromeless />
							</div>
						</div>
					</div>
				)}
			</div>
			<div className="flex w-64 shrink-0 flex-col border-l border-border">
				<HistoryHeader count={versions.length} onClose={onClose} />
				<div className="min-h-0 flex-1 overflow-auto">
					{versions.map((v) => (
						<VersionRow key={v.id} version={v} selected={v.id === selectedId} onSelect={setSelectedId} />
					))}
				</div>
				<div className="flex shrink-0 flex-col gap-1 border-t border-border p-2">
					<Button size="sm" variant="outline" disabled={isNow} className="h-7 text-xs">
						Restore this version
					</Button>
					<Button size="sm" variant="ghost" disabled={isNow} className="h-7 text-xs">
						Copy as new artifact
					</Button>
				</div>
			</div>
		</div>
	);
}

function HistoryHeader({ count, onClose }: { count: number; onClose: () => void }) {
	return (
		<div className="flex shrink-0 items-center gap-2 border-b border-border px-3 py-2 text-xs">
			<span className="font-medium">Version history</span>
			<span className="text-muted-foreground">{count}</span>
			<button
				type="button"
				onClick={onClose}
				aria-label="Close version history"
				className="ml-auto text-muted-foreground hover:text-foreground"
			>
				<X className="h-3.5 w-3.5" />
			</button>
		</div>
	);
}

function CompareSideHeader({ label, tag, now }: { label: string; tag: string; now?: boolean }) {
	return (
		<div
			className={
				now
					? 'flex shrink-0 items-center gap-2 border-b border-border bg-primary/5 px-3 py-1.5 text-xs font-medium'
					: 'flex shrink-0 items-center gap-2 border-b border-border bg-muted/20 px-3 py-1.5 text-xs'
			}
		>
			<span className="truncate">{label}</span>
			<span className="ml-auto text-muted-foreground">{tag}</span>
		</div>
	);
}

function VersionRow({
	version,
	selected,
	onSelect,
}: {
	version: ArtifactVersion;
	selected: boolean;
	onSelect: (id: string) => void;
}) {
	return (
		<button
			type="button"
			onClick={() => onSelect(version.id)}
			className={
				selected
					? 'flex w-full flex-col gap-0.5 border-b border-border bg-accent px-3 py-2 text-left text-xs'
					: 'flex w-full flex-col gap-0.5 border-b border-border px-3 py-2 text-left text-xs hover:bg-muted/40'
			}
		>
			<span className="flex items-center gap-1.5">
				<span className="rounded bg-muted px-1 py-0.5 font-mono text-[10px] uppercase text-muted-foreground">
					{version.kind === 'now' ? 'disk' : version.kind}
				</span>
				<span className="truncate font-medium">{version.label}</span>
			</span>
			<span className="truncate text-muted-foreground">
				{version.when}
				{version.note ? ` · ${version.note}` : ''}
			</span>
		</button>
	);
}
