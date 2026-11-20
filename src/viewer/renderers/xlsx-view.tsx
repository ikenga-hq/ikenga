import { useEffect, useMemo, useState } from 'react';
import { AlertCircle, Loader2 } from 'lucide-react';
import * as XLSX from 'xlsx';
import { fsRead } from '@/lib/tauri-cmd';
import { CannotPreviewPlate } from '../chrome/cannot-preview-plate';
import { isPreviewTooLarge } from '../lib/preview-cap';
import { CSV_ROW_CAP, renderedRowCount } from './csv-view';

interface XlsxViewProps {
	path: string;
}

interface ParsedWorkbook {
	sheetNames: string[];
	sheets: Record<string, Array<Array<string | number | boolean | null>>>;
}

/** Read options: real dates (not serial numbers), and the number formats the
 *  workbook was saved with. Exported for the test. */
export const XLSX_READ_OPTS = { type: 'array', cellDates: true } as const;

/** Sheet → rows as the *formatted* text Excel shows (`raw: false` uses each
 *  cell's `w` field), so dates and currency read as they do in a spreadsheet. */
export function sheetRows(sheet: XLSX.WorkSheet): Array<Array<string | number | boolean | null>> {
	return XLSX.utils.sheet_to_json(sheet, {
		header: 1,
		raw: false,
		defval: null,
	}) as Array<Array<string | number | boolean | null>>;
}

/** The "showing first N of M rows" note, or null when everything is shown. */
export function rowCapNote(total: number, shown: number): string | null {
	const n = renderedRowCount(total, shown);
	if (n >= total) return null;
	return `Showing first ${n.toLocaleString()} of ${total.toLocaleString()} rows`;
}

// Renders the active sheet as a raw HTML table. Critical for CFO workflows
// that surface bank statement XLSX files — keep it read-only, single
// active-cell highlight, no edit gestures (those land post-alpha).
export function XlsxView({ path }: XlsxViewProps) {
	const [state, setState] = useState<
		{ kind: 'loading' } | { kind: 'ready'; wb: ParsedWorkbook } | { kind: 'error'; message: string }
	>({ kind: 'loading' });
	const [activeSheet, setActiveSheet] = useState<string>('');

	useEffect(() => {
		let cancelled = false;
		setState({ kind: 'loading' });
		fsRead(path)
			.then((res) => {
				if (cancelled) return;
				try {
					const wb = XLSX.read(new Uint8Array(res.bytes), XLSX_READ_OPTS);
					const parsed: ParsedWorkbook = {
						sheetNames: wb.SheetNames,
						sheets: {},
					};
					for (const name of wb.SheetNames) {
						const sheet = wb.Sheets[name]!;
						parsed.sheets[name] = sheetRows(sheet);
					}
					setState({ kind: 'ready', wb: parsed });
					setActiveSheet(parsed.sheetNames[0] ?? '');
				} catch (err) {
					setState({
						kind: 'error',
						message: err instanceof Error ? err.message : String(err),
					});
				}
			})
			.catch((err) => {
				if (cancelled) return;
				setState({
					kind: 'error',
					message: err instanceof Error ? err.message : String(err),
				});
			});
		return () => {
			cancelled = true;
		};
	}, [path]);

	const rows = useMemo(() => {
		if (state.kind !== 'ready' || !activeSheet) return [];
		return state.wb.sheets[activeSheet] ?? [];
	}, [state, activeSheet]);
	const visibleRows = useMemo(
		() => rows.slice(0, renderedRowCount(rows.length, CSV_ROW_CAP)),
		[rows]
	);
	const capNote = rowCapNote(rows.length, CSV_ROW_CAP);

	if (state.kind === 'loading') {
		return (
			<div className="flex h-full items-center justify-center text-xs text-muted-foreground">
				<Loader2 className="mr-2 h-4 w-4 animate-spin" /> Parsing workbook…
			</div>
		);
	}
	if (state.kind === 'error') {
		if (isPreviewTooLarge(state.message)) {
			return <CannotPreviewPlate path={path} message={state.message} />;
		}
		return (
			<div className="flex h-full items-start justify-center p-6 text-xs text-destructive">
				<AlertCircle className="mr-2 mt-0.5 h-4 w-4 shrink-0" />
				<span className="break-all">{state.message}</span>
			</div>
		);
	}

	return (
		<div className="flex h-full flex-col">
			<div className="flex shrink-0 gap-1 overflow-x-auto border-b border-border bg-muted/20 px-2 py-1.5">
				{state.wb.sheetNames.map((name) => (
					<button
						key={name}
						onClick={() => setActiveSheet(name)}
						className={
							activeSheet === name
								? 'rounded bg-primary px-2.5 py-1 text-[11px] font-medium text-primary-foreground'
								: 'rounded px-2.5 py-1 text-[11px] text-muted-foreground hover:bg-muted hover:text-foreground'
						}
					>
						{name}
					</button>
				))}
			</div>
			<div className="flex-1 overflow-auto">
				<table className="border-collapse font-mono text-[11px]">
					<tbody>
						{visibleRows.map((row, ri) => (
							<tr key={ri} className={ri === 0 ? 'bg-muted/40 font-semibold' : ''}>
								<td className="sticky left-0 border border-border bg-muted/30 px-2 py-1 text-right text-muted-foreground tabular-nums">
									{ri + 1}
								</td>
								{row.map((cell, ci) => (
									<td key={ci} className="border border-border px-2 py-1 align-top whitespace-pre">
										{cell === null || cell === undefined ? '' : String(cell)}
									</td>
								))}
							</tr>
						))}
					</tbody>
				</table>
			</div>
			{capNote && (
				<div
					data-state="xlsx-row-cap"
					className="shrink-0 border-t border-border bg-muted/20 px-3 py-1.5 text-[11px] text-muted-foreground"
				>
					{capNote}
				</div>
			)}
		</div>
	);
}
