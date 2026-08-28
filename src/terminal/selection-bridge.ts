export interface SelectionRange {
	filePath: string;
	startLine: number;
	endLine: number;
	text: string;
}

export interface DiagnosticItem {
	severity: 'error' | 'warning' | 'info';
	line: number;
	message: string;
	code?: string;
}

export function formatSelectionContext(range: SelectionRange): string {
	const header = `@${range.filePath}#L${range.startLine}-${range.endLine}`;
	return `${header}\n\`\`\`\n${range.text}\n\`\`\``;
}

export function formatDiagnosticsContext(filePath: string, diagnostics: DiagnosticItem[]): string {
	if (!diagnostics || diagnostics.length === 0) return '';

	const items = diagnostics
		.map(
			(d) =>
				`  - L${d.line} [${d.severity.toUpperCase()}] ${d.message}${d.code ? ` (${d.code})` : ''}`
		)
		.join('\n');

	return `Diagnostics for @${filePath}:\n${items}`;
}
