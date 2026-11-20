import { useEffect, useMemo, useState } from 'react';
import { AlertCircle, Loader2 } from 'lucide-react';
import * as XLSX from 'xlsx';
import { fsRead } from '@/lib/tauri-cmd';
import { PreviewTooLargePlate } from '../chrome/preview-too-large-plate';
import { isPreviewTooLarge } from '../lib/preview-cap';

interface XlsxViewProps {
	path: string;
}

interface ParsedWorkbook {
	sheetNames: string[];
	sheets: Record<string, Array<Array<string | number | boolean | null>>>;
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
					const wb = XLSX.read(new Uint8Array(res.bytes), { type: 'array' });
					const parsed: ParsedWorkbook = {
						sheetNames: wb.SheetNames,
						sheets: {},
					};
					for (const name of wb.SheetNames) {
						const sheet = wb.Sheets[name]!;
						parsed.sheets[name] = XLSX.utils.sheet_to_json(sheet, {
							header: 1,
							raw: true,
							defval: null,
						}) as Array<Array<string | number | boolean | null>>;
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

	if (state.kind === 'loading') {
		return (
			<div className="flex h-full items-center justify-center text-xs text-muted-foreground">
				<Loader2 className="mr-2 h-4 w-4 animate-spin" /> Parsing workbook…
			</div>
		);
	}
	if (state.kind === 'error') {
		if (isPreviewTooLarge(state.message)) {
			return <PreviewTooLargePlate path={path} message={state.message} />;
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
						{rows.map((row, ri) => (
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
		</div>
	);
}
