// D-06 `import` state (WP-61): the three import sources — a package's own key
// requests, a VS Code `keybindings.json`, and a teammate's project file — as
// one add/skip/clash diff, per `plans/shell-ux-rearchitecture/drafts/
// actions-schema.md` §5 ("import is a writer, not a layer") and §14 item 24.
// Nothing is written until "Add" (D-06's own footer line, kept verbatim).
//
// The three sources are genuinely different shapes of input (a live model
// read, a foreign JSONC file, a pair of Ikenga's own strict-JSON files), so
// the classification/translation logic for each lives in its own
// `@/lib/actions/import/{package,vscode,project}.ts` module (frozen tables in
// `vscode-map.ts`); this file is only the surface that picks a source, shows
// its diff, and calls the matching `apply*Import`.

import { useMemo, useState } from 'react';
import { Box, Download, TriangleAlert, Users } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { open as openFileDialog } from '@/lib/transport/dialog-shim';
import { fsRead } from '@/lib/tauri-cmd';
import type { ActionsSurfaceProps } from '../types';
import { buildPackageDiff, applyPackageImport, type PackageImportRow } from '@/lib/actions/import/package';
import {
	buildVSCodeDiff,
	applyVSCodeImport,
	parseVSCodeKeybindingsText,
	type VSCodeImportRow,
} from '@/lib/actions/import/vscode';
import {
	buildProjectDiff,
	applyProjectImport,
	readTeammateProjectFile,
	type ProjectImportDiff,
} from '@/lib/actions/import/project';
import type { ImportDiffRow } from '@/lib/actions/import/vscode-map';

type SourceId = 'pkg' | 'vscode' | 'team';

const SOURCES: ReadonlyArray<{ id: SourceId; label: string; sub: string; Icon: typeof Box }> = [
	{ id: 'pkg', label: 'From a package', sub: 'Key requests from your installed packages', Icon: Box },
	{ id: 'vscode', label: 'From VS Code keybindings', sub: 'A keybindings.json file you choose', Icon: Download },
	{ id: 'team', label: "From a teammate's project file", sub: 'Another checkout’s .ikenga/actions.json', Icon: Users },
];

function DiffRowView({ row }: { row: ImportDiffRow }) {
	const sign = row.kind === 'add' ? '+' : row.kind === 'skip' ? '·' : '!';
	return (
		<div className={`imp-row ${row.kind}`}>
			<span className="imp-sign" aria-hidden="true">
				{sign}
			</span>
			<span className="imp-mid">
				<span className="imp-title">{row.title}</span>
				<span className="imp-detail">{row.detail}</span>
			</span>
		</div>
	);
}

function countAdds(rows: readonly ImportDiffRow[]): number {
	return rows.filter((r) => r.kind === 'add').length;
}

export function ImportSurface({ scope, model, onNavigate }: ActionsSurfaceProps) {
	const [source, setSource] = useState<SourceId>('pkg');
	const [applying, setApplying] = useState(false);
	const [error, setError] = useState<string | null>(null);

	// ── "From a package" — always available: a live read of the model. ──────
	const packageRows = useMemo<PackageImportRow[]>(() => buildPackageDiff(model), [model]);

	// ── "From VS Code keybindings" — the user picks a file, then we diff. ───
	const [vsCodePath, setVsCodePath] = useState<string | null>(null);
	const [vsCodeRows, setVsCodeRows] = useState<VSCodeImportRow[] | null>(null);
	const [vsCodeError, setVsCodeError] = useState<string | null>(null);
	const [vsCodeLoading, setVsCodeLoading] = useState(false);

	// ── "From a teammate's project file" — same shape, project-scope only. ──
	const [teamPath, setTeamPath] = useState<string | null>(null);
	const [teamDiff, setTeamDiff] = useState<ProjectImportDiff | null>(null);
	const [teamError, setTeamError] = useState<string | null>(null);
	const [teamLoading, setTeamLoading] = useState(false);
	const projectAvailable = Boolean(model.projectRoot);

	async function pickVSCodeFile() {
		setVsCodeError(null);
		setError(null);
		const picked = await openFileDialog({
			multiple: false,
			filters: [{ name: 'keybindings.json', extensions: ['json'] }],
		});
		const path = Array.isArray(picked) ? (picked[0] ?? null) : picked;
		if (!path) return;
		setVsCodeLoading(true);
		try {
			const bytes = (await fsRead(path)).bytes;
			const text = new TextDecoder('utf-8', { fatal: false }).decode(new Uint8Array(bytes));
			const rules = parseVSCodeKeybindingsText(text);
			setVsCodePath(path);
			setVsCodeRows(buildVSCodeDiff(rules));
		} catch (err) {
			setVsCodePath(null);
			setVsCodeRows(null);
			setVsCodeError(err instanceof Error ? err.message : String(err));
		} finally {
			setVsCodeLoading(false);
		}
	}

	async function pickTeamFile() {
		setTeamError(null);
		setError(null);
		const picked = await openFileDialog({
			multiple: false,
			filters: [{ name: 'actions.json', extensions: ['json'] }],
		});
		const path = Array.isArray(picked) ? (picked[0] ?? null) : picked;
		if (!path) return;
		setTeamLoading(true);
		try {
			const teamSource = await readTeammateProjectFile(path);
			setTeamPath(path);
			setTeamDiff(buildProjectDiff(teamSource, model));
		} catch (err) {
			setTeamPath(null);
			setTeamDiff(null);
			setTeamError(err instanceof Error ? err.message : String(err));
		} finally {
			setTeamLoading(false);
		}
	}

	const addCount =
		source === 'pkg'
			? countAdds(packageRows)
			: source === 'vscode'
				? countAdds(vsCodeRows ?? [])
				: countAdds(teamDiff ? [...teamDiff.actionRows, ...teamDiff.bindingRows] : []);

	async function handleAdd() {
		setError(null);
		setApplying(true);
		try {
			if (source === 'pkg') {
				await applyPackageImport(packageRows, scope);
			} else if (source === 'vscode') {
				await applyVSCodeImport(vsCodeRows ?? [], scope);
			} else if (teamDiff) {
				await applyProjectImport(teamDiff);
			}
			onNavigate('actions');
		} catch (err) {
			setError(err instanceof Error ? err.message : String(err));
		} finally {
			setApplying(false);
		}
	}

	function renderDiff() {
		if (source === 'pkg') {
			if (packageRows.length === 0) {
				return <div className="imp-empty">No installed package requests a key right now.</div>;
			}
			return (
				<>
					<p className="imp-lead">Reviewed against your {model.keymap.entries.length} bindings. Nothing is written until you add.</p>
					{packageRows.map((row) => (
						<DiffRowView key={row.key} row={row} />
					))}
				</>
			);
		}
		if (source === 'vscode') {
			if (vsCodeLoading) return <div className="imp-empty">Reading…</div>;
			if (vsCodeError) {
				return (
					<div className="imp-empty" role="alert">
						{vsCodeError}
					</div>
				);
			}
			if (!vsCodeRows) {
				return (
					<div className="imp-empty">
						<Button variant="outline" size="sm" onClick={() => void pickVSCodeFile()}>
							<Download className="h-3.5 w-3.5" />
							Choose keybindings.json…
						</Button>
					</div>
				);
			}
			return (
				<>
					<p className="imp-lead">
						{vsCodePath} · maps the frozen id-map core onto your {scope} keybindings. Everything else is shown as not
						imported.
					</p>
					{vsCodeRows.map((row) => (
						<DiffRowView key={row.key} row={row} />
					))}
				</>
			);
		}
		// team
		if (!projectAvailable) {
			return <div className="imp-empty">No active project has a filesystem root — nowhere to import a project file into.</div>;
		}
		if (teamLoading) return <div className="imp-empty">Reading…</div>;
		if (teamError) {
			return (
				<div className="imp-empty" role="alert">
					{teamError}
				</div>
			);
		}
		if (!teamDiff) {
			return (
				<div className="imp-empty">
					<Button variant="outline" size="sm" onClick={() => void pickTeamFile()}>
						<Users className="h-3.5 w-3.5" />
						Choose a teammate's actions.json…
					</Button>
				</div>
			);
		}
		return (
			<>
				<p className="imp-lead">
					{teamPath} · lands as project actions and keybindings — actions run once this project is trusted (DEC-55);
					keybindings are held until this project's keybindings are trusted (DEC-65).
				</p>
				{teamDiff.actionRows.length > 0 && <div className="imp-subhead">Actions</div>}
				{teamDiff.actionRows.map((row) => (
					<DiffRowView key={row.key} row={row} />
				))}
				{teamDiff.bindingRows.length > 0 && <div className="imp-subhead">Keybindings</div>}
				{teamDiff.bindingRows.map((row) => (
					<DiffRowView key={row.key} row={row} />
				))}
				{teamDiff.actionRows.length === 0 && teamDiff.bindingRows.length === 0 && (
					<div className="imp-empty">That file has nothing to import.</div>
				)}
			</>
		);
	}

	return (
		<div data-state="import" className="imp-root">
			<div className="imp-wrap">
				<div className="imp-srccol" id="importSrc" role="tablist" aria-label="Import sources">
					{SOURCES.map((s) => (
						<button
							key={s.id}
							type="button"
							role="tab"
							aria-selected={source === s.id}
							className="imp-srcrow"
							onClick={() => setSource(s.id)}
						>
							<s.Icon className="h-3.5 w-3.5 mt-0.5 shrink-0" aria-hidden="true" />
							<span>
								<span className="imp-srclabel">{s.label}</span>
								<span className="imp-srcsub">{s.sub}</span>
							</span>
						</button>
					))}
				</div>
				<div className="imp-diffcol">{renderDiff()}</div>
			</div>
			{error && (
				<div className="vhead-error" role="alert">
					<TriangleAlert className="h-3.5 w-3.5" style={{ display: 'inline', marginRight: 4 }} aria-hidden="true" />
					{error}
				</div>
			)}
			<div className="imp-formfoot">
				<Button size="lg" disabled={addCount === 0 || applying} onClick={() => void handleAdd()}>
					{applying ? 'Adding…' : `Add ${addCount} ${addCount === 1 ? 'item' : 'items'}`}
				</Button>
				<Button variant="ghost" size="lg" onClick={() => onNavigate('actions')}>
					Cancel
				</Button>
				<span className="imp-hint">Nothing is written until you add. Conflicts keep your binding and import unbound.</span>
			</div>
		</div>
	);
}
