// /artifacts/by-kind/$kind — per-archetype drill-in.
//
// Lists every artifact in the active project whose `manifest.notes.kind`
// matches `$kind`, sorted by mtime descending. Reached from the Studio
// home's by-archetype tiles (when count > 0); empty tiles still open the
// wizard.
//
// Pure list view — not a folder-grid. The walker collects rows from
// anywhere in the project, so there's no single folder to mount; we just
// render the matching `ArtifactRow`s as clickable items that open the
// loupe.

import { useMemo } from 'react';
import { createFileRoute, useNavigate, useParams } from '@tanstack/react-router';
import { useQuery } from '@tanstack/react-query';
import { ArrowLeft, FileText, Star } from 'lucide-react';

import { Button } from '@/components/ui/button';
import { usePaneStore } from '@/lib/panes/pane-store';
import { projectArtifactsQueryOptions } from '@/lib/queries/project-artifacts';
import { useShellStore } from '@/lib/shell/shell-store';
import { ArchetypeGlyph } from '@/shell/artifact-wizard/archetype-glyph';
import { type Archetype, findArchetype } from '@/shell/artifact-wizard/archetypes';
import type { ArtifactRow } from '@/lib/tauri-cmd';

export const Route = createFileRoute('/artifacts/by-kind/$kind')({
	component: ByKindPage,
});

function ByKindPage() {
	const { kind } = useParams({ from: '/artifacts/by-kind/$kind' });
	const navigate = useNavigate();
	const activeProject = useShellStore(
		(s) => s.projects.find((p) => p.id === s.activeProjectId) ?? null
	);
	const catalogQuery = useQuery(projectArtifactsQueryOptions(activeProject?.root_path ?? null));

	const archetype = findArchetype(kind);
	const rows = useMemo(() => {
		const all = catalogQuery.data?.rows ?? [];
		return all.filter((r) => r.kind === kind).sort((a, b) => b.modified_at - a.modified_at);
	}, [catalogQuery.data, kind]);

	function openLoupe(path: string) {
		const ps = usePaneStore.getState();
		ps.addTab(ps.focusedId, { kind: 'artifact-studio', path, density: 'loupe' });
	}

	function goHome() {
		void navigate({ to: '/artifacts/home' });
	}

	function openWizard() {
		void navigate({
			to: '/projects/new-artifact',
			search: { archetype: kind },
		});
	}

	const label = archetype?.label ?? kind;
	const glyphName = archetype?.glyphName ?? 'Square';

	return (
		<div className="flex h-full flex-col overflow-y-auto">
			<HeaderBar
				label={label}
				kind={kind}
				glyphName={glyphName}
				totalForKind={rows.length}
				totalAll={catalogQuery.data?.counts.all}
				onBack={goHome}
				onNew={openWizard}
			/>

			<div className="mx-auto flex w-full max-w-5xl flex-col gap-3 px-6 py-6">
				{catalogQuery.isLoading && !catalogQuery.data ? (
					<EmptyNotice text="Loading…" />
				) : rows.length === 0 ? (
					<EmptyForKind label={label} archetype={archetype} onNew={openWizard} />
				) : (
					<ul className="flex flex-col rounded border border-border">
						{rows.map((r) => (
							<RowItem key={r.path} row={r} onOpen={() => openLoupe(r.path)} />
						))}
					</ul>
				)}
			</div>
		</div>
	);
}

// ─── Header ──────────────────────────────────────────────────────────────

function HeaderBar({
	label,
	kind,
	glyphName,
	totalForKind,
	totalAll,
	onBack,
	onNew,
}: {
	label: string;
	kind: string;
	glyphName: string;
	totalForKind: number;
	totalAll: number | undefined;
	onBack: () => void;
	onNew: () => void;
}) {
	return (
		<div className="flex shrink-0 items-center gap-3 border-b border-border px-6 py-3">
			<Button variant="ghost" size="sm" onClick={onBack} aria-label="Back to home">
				<ArrowLeft className="h-4 w-4" />
			</Button>
			<ArchetypeGlyph name={glyphName} className="h-4 w-4 text-muted-foreground" />
			<div className="flex flex-col min-w-0">
				<span className="text-sm font-semibold text-foreground">{label}</span>
				<span className="font-mono text-[10px] text-muted-foreground">
					{totalForKind} of {totalAll ?? '…'}
					{typeof totalAll === 'number' && ` artifact${totalAll === 1 ? '' : 's'}`} ·{' '}
					<span className="opacity-70">kind:{kind}</span>
				</span>
			</div>
			<div className="ml-auto">
				<Button size="sm" onClick={onNew}>
					+ New {label.toLowerCase()}
				</Button>
			</div>
		</div>
	);
}

// ─── Rows ────────────────────────────────────────────────────────────────

function RowItem({ row, onOpen }: { row: ArtifactRow; onOpen: () => void }) {
	const folder = row.path.replace(/\/+[^/]+$/, '');
	return (
		<li className="border-b border-border last:border-b-0">
			<button
				type="button"
				onClick={onOpen}
				className="flex w-full items-center gap-2 px-3 py-2 text-left text-sm hover:bg-accent"
				title={row.path}
			>
				{row.starred ? (
					<Star className="h-3.5 w-3.5 shrink-0 fill-amber-500 text-amber-500" />
				) : (
					<FileText className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
				)}
				<div className="flex-1 min-w-0">
					<div className="truncate text-foreground">{row.name}</div>
					<div className="truncate font-mono text-[10px] text-muted-foreground/70">{folder}</div>
				</div>
				{row.version && (
					<span className="font-mono text-[10px] text-muted-foreground">v{row.version}</span>
				)}
				<span className="font-mono text-[10px] text-muted-foreground/70">
					{relativeAt(row.modified_at)}
				</span>
			</button>
		</li>
	);
}

function EmptyForKind({
	label,
	archetype,
	onNew,
}: {
	label: string;
	archetype: Archetype | null;
	onNew: () => void;
}) {
	return (
		<div className="rounded border border-dashed border-border px-4 py-8 text-center">
			<p className="text-sm text-muted-foreground">
				No <span className="font-medium text-foreground">{label}</span> artifacts in this project
				yet.
			</p>
			{archetype && <p className="mt-1 text-xs text-muted-foreground">{archetype.description}</p>}
			<div className="mt-3">
				<Button size="sm" onClick={onNew}>
					+ New {label.toLowerCase()}
				</Button>
			</div>
		</div>
	);
}

function EmptyNotice({ text }: { text: string }) {
	return (
		<div className="rounded border border-dashed border-border px-4 py-3 text-center text-xs text-muted-foreground">
			{text}
		</div>
	);
}

// ─── Helpers ─────────────────────────────────────────────────────────────

function relativeAt(ms: number): string {
	const delta = Date.now() - ms;
	const min = Math.round(delta / 60_000);
	if (min < 1) return 'now';
	if (min < 60) return `${min}m`;
	const hr = Math.round(min / 60);
	if (hr < 24) return `${hr}h`;
	const day = Math.round(hr / 24);
	if (day < 7) return `${day}d`;
	const wk = Math.round(day / 7);
	if (wk < 5) return `${wk}w`;
	const mo = Math.round(day / 30);
	return `${mo}mo`;
}
