// D-08 `renderers` — the CSV table renderer (designs/pane-chrome.html
// `RENDERERS` id `csv`, marked `new` there: "text/csv falls through to
// CodeView today. A table renderer is a proposal, not shipped." — WP-44
// ships it). Sticky header, also used for `.tsv` (comma vs tab delimiter).
//
// No CSV-parsing dependency is added (installs are out of scope for this
// WP) — the parser below is a small hand-rolled RFC 4180 reader: quoted
// fields, embedded commas/newlines, and `""` as an escaped quote.

import { useEffect, useState } from 'react';
import { AlertCircle } from 'lucide-react';
import { ErrorState, LoadingState } from '@/components/states';
import { fsRead } from '@/lib/tauri-cmd';

interface CsvViewProps {
	path: string;
}

export type ParsedCsv = { header: string[]; rows: string[][] };

/** Exported for `csv-view.test.ts` — the parser is the highest-risk part of
 *  this renderer (quoted fields, embedded delimiters/newlines); everything
 *  else is a plain `<table>`. */
export function parseDelimited(text: string, delimiter: string): ParsedCsv {
	const rows: string[][] = [];
	let row: string[] = [];
	let field = '';
	let inQuotes = false;
	let i = 0;
	const n = text.length;

	const pushField = () => {
		row.push(field);
		field = '';
	};
	const pushRow = () => {
		pushField();
		rows.push(row);
		row = [];
	};

	while (i < n) {
		const ch = text[i];
		if (inQuotes) {
			if (ch === '"') {
				if (text[i + 1] === '"') {
					field += '"';
					i += 2;
					continue;
				}
				inQuotes = false;
				i++;
				continue;
			}
			field += ch;
			i++;
			continue;
		}
		if (ch === '"') {
			inQuotes = true;
			i++;
			continue;
		}
		if (ch === delimiter) {
			pushField();
			i++;
			continue;
		}
		if (ch === '\r') {
			i++;
			continue;
		}
		if (ch === '\n') {
			pushRow();
			i++;
			continue;
		}
		field += ch;
		i++;
	}
	// Trailing field/row (files without a final newline).
	if (field.length > 0 || row.length > 0) pushRow();
	// Drop a single trailing wholly-empty row (common with a final newline).
	if (rows.length > 0 && rows[rows.length - 1].length === 1 && rows[rows.length - 1][0] === '') {
		rows.pop();
	}

	const [header = [], ...body] = rows;
	return { header, rows: body };
}

/** Rows rendered per step. The table has no windowing (no virtualization
 *  dependency is in scope), so a multi-hundred-thousand-row export would
 *  otherwise put every row in the DOM at once; beyond this the footer says
 *  "showing N of M" and offers "Show more" (WP-44 review F2). */
export const CSV_ROW_CAP = 2000;

/** How many rows to render: `shown`, clamped to the row count. Exported for
 *  `csv-view.test.ts`. */
export function renderedRowCount(total: number, shown: number): number {
	return Math.max(0, Math.min(total, shown));
}

export function CsvView({ path }: CsvViewProps) {
	const [shown, setShown] = useState(CSV_ROW_CAP);
	const delimiter = path.toLowerCase().endsWith('.tsv') ? '\t' : ',';
	const [state, setState] = useState<
		| { kind: 'loading' }
		| { kind: 'ready'; data: ParsedCsv }
		| { kind: 'error'; message: string }
	>({ kind: 'loading' });

	useEffect(() => {
		let cancelled = false;
		setState({ kind: 'loading' });
		setShown(CSV_ROW_CAP);
		fsRead(path)
			.then((res) => {
				if (cancelled) return;
				const text = new TextDecoder('utf-8', { fatal: false }).decode(new Uint8Array(res.bytes));
				setState({ kind: 'ready', data: parseDelimited(text, delimiter) });
			})
			.catch((err) => {
				if (cancelled) return;
				setState({ kind: 'error', message: err instanceof Error ? err.message : String(err) });
			});
		return () => {
			cancelled = true;
		};
	}, [path, delimiter]);

	if (state.kind === 'loading') {
		return <LoadingState data-state="loading" fill heading="Loading…" />;
	}
	if (state.kind === 'error') {
		return (
			<ErrorState
				data-state="error"
				fill
				icon={AlertCircle}
				heading="Couldn't read this file"
				body={<span className="break-all">{state.message}</span>}
			/>
		);
	}

	const { header, rows } = state.data;
	const visible = renderedRowCount(rows.length, shown);

	if (header.length === 0 && rows.length === 0) {
		return (
			<div className="flex h-full items-center justify-center text-xs text-muted-foreground">
				Empty file
			</div>
		);
	}

	return (
		<div className="flex h-full flex-col">
			<div className="min-h-0 flex-1 overflow-auto">
				<table className="w-full border-collapse text-xs">
					<thead className="sticky top-0 z-10 bg-muted/90 backdrop-blur">
						<tr>
							{header.map((cell, i) => (
								<th
									key={i}
									className="whitespace-nowrap border-b border-border px-2 py-1.5 text-left font-medium text-muted-foreground"
								>
									{cell}
								</th>
							))}
						</tr>
					</thead>
					<tbody>
						{rows.slice(0, visible).map((r, ri) => (
							<tr key={ri} className="odd:bg-transparent even:bg-muted/10 hover:bg-accent/40">
								{header.map((_, ci) => (
									<td key={ci} className="whitespace-nowrap border-b border-border/60 px-2 py-1 font-mono">
										{r[ci] ?? ''}
									</td>
								))}
							</tr>
						))}
					</tbody>
				</table>
			</div>
			<div className="flex shrink-0 items-center gap-3 border-t border-border bg-muted/20 px-3 py-1 font-mono text-[10px] text-muted-foreground">
				<span>
					{visible < rows.length
						? `showing first ${visible.toLocaleString()} of ${rows.length.toLocaleString()} rows`
						: `${rows.length.toLocaleString()} rows`}
				</span>
				<span>{header.length} columns</span>
				{visible < rows.length && (
					<button
						type="button"
						className="underline-offset-2 hover:text-foreground hover:underline"
						onClick={() => setShown((n) => n + CSV_ROW_CAP)}
					>
						Show {Math.min(CSV_ROW_CAP, rows.length - visible).toLocaleString()} more
					</button>
				)}
			</div>
		</div>
	);
}
