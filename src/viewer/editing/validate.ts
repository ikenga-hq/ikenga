// Structured-format validation before save (plans/file-editing F1, Shape 3).
//
// JSON and JSON Lines use the built-in JSON.parse. YAML (`yaml`) and TOML
// (`smol-toml`) parsers are loaded lazily, the first time such a file is saved,
// so they stay out of the main bundle. CSV is saved as text with no check (F1),
// and JSON5 has no parser here, so it is saved unvalidated and the toolbar says
// so.
//
// Positions are best effort: V8 puts "line L column C" (or "position N") in its
// JSON.parse messages, but WebKit — the desktop webview on Linux and macOS —
// gives none, so there the error shows without a "Go to line" link.

export type ValidationResult =
	| { ok: true }
	| { ok: false; message: string; line?: number; col?: number };

export type Validator = (text: string) => Promise<ValidationResult>;

export type ValidationKind = 'json' | 'jsonl' | 'yaml' | 'toml' | 'unvalidated' | 'none';

const OK: ValidationResult = { ok: true };

/** Which check a path gets on save. `unvalidated` = a structured format we
 *  cannot parse here (JSON5); `none` = plain text. */
export function validationKindFor(path: string): ValidationKind {
	const lower = path.toLowerCase();
	if (lower.endsWith('.jsonl') || lower.endsWith('.ndjson')) return 'jsonl';
	if (lower.endsWith('.json5')) return 'unvalidated';
	if (lower.endsWith('.json')) return 'json';
	if (lower.endsWith('.yaml') || lower.endsWith('.yml')) return 'yaml';
	if (lower.endsWith('.toml')) return 'toml';
	return 'none';
}

/** The validator for `path`, or null when its content is not checked. */
export function validatorFor(path: string): Validator | null {
	switch (validationKindFor(path)) {
		case 'json':
			return async (text) => validateJson(text);
		case 'jsonl':
			return async (text) => validateJsonLines(text);
		case 'yaml':
			return validateYaml;
		case 'toml':
			return validateToml;
		default:
			return null;
	}
}

export function validateJson(text: string): ValidationResult {
	try {
		JSON.parse(text);
		return OK;
	} catch (err) {
		const message = errMessage(err);
		return { ok: false, message: `Invalid JSON: ${message}`, ...jsonPosition(message, text) };
	}
}

export function validateJsonLines(text: string): ValidationResult {
	const lines = text.split('\n');
	for (let i = 0; i < lines.length; i++) {
		const line = lines[i];
		if (line.trim() === '') continue;
		try {
			JSON.parse(line);
		} catch (err) {
			const message = errMessage(err);
			const pos = jsonPosition(message, line);
			return {
				ok: false,
				message: `Invalid JSON on line ${i + 1}: ${message}`,
				line: i + 1,
				col: pos.col,
			};
		}
	}
	return OK;
}

export async function validateYaml(text: string): Promise<ValidationResult> {
	const { parseAllDocuments } = await import('yaml');
	// All documents, so a multi-document `---` stream is valid. Warnings
	// (unknown tags, …) do not block a save; errors do.
	const docs = parseAllDocuments(text);
	const errors = Array.isArray(docs) ? docs.flatMap((d) => d.errors) : [];
	const streamErrors = (docs as { errors?: unknown[] }).errors ?? [];
	const first = (errors[0] ?? streamErrors[0]) as
		| { message?: string; linePos?: Array<{ line: number; col: number }> }
		| undefined;
	if (!first) return OK;
	const pos = first.linePos?.[0];
	return {
		ok: false,
		message: `Invalid YAML: ${firstLine(first.message ?? 'parse error')}`,
		line: pos?.line,
		col: pos?.col,
	};
}

export async function validateToml(text: string): Promise<ValidationResult> {
	const { parse } = await import('smol-toml');
	try {
		parse(text);
		return OK;
	} catch (err) {
		const e = err as { line?: number; column?: number };
		return {
			ok: false,
			message: `Invalid TOML: ${firstLine(errMessage(err))}`,
			line: typeof e.line === 'number' ? e.line : undefined,
			col: typeof e.column === 'number' ? e.column : undefined,
		};
	}
}

/** Line/column from a V8 JSON.parse message; nothing from WebKit's. */
export function jsonPosition(message: string, text: string): { line?: number; col?: number } {
	const lc = /line (\d+) column (\d+)/.exec(message);
	if (lc) return { line: Number(lc[1]), col: Number(lc[2]) };
	const p = /position (\d+)/.exec(message);
	if (!p) return {};
	const offset = Math.min(Number(p[1]), text.length);
	const before = text.slice(0, offset);
	const line = before.split('\n').length;
	const col = offset - before.lastIndexOf('\n');
	return { line, col };
}

function errMessage(err: unknown): string {
	return err instanceof Error ? err.message : String(err);
}

function firstLine(s: string): string {
	const i = s.indexOf('\n');
	return (i === -1 ? s : s.slice(0, i)).trim();
}
