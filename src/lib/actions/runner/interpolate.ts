// WP-53 — `{{name}}` interpolation over the six run variables (G-ACTIONS
// §8.2, DEC-63.4), escaped by the context the value lands in:
//
//   • shell  — each value becomes ONE argument, quoted for the executing
//              shell (POSIX single quotes; PowerShell single quotes on
//              Windows, which is what `action_exec` runs there). Never spliced
//              raw, so `{{file.path}}` of `a'; rm -rf ~ #` stays one word.
//   • uri    — percent-encoded as a URI component, except when the whole
//              template is a single variable (then the value is the URL).
//   • raw    — as-is (`chi` / `skill` prompts; `iyke` values travel in the
//              JSON body instead and never touch the route).
//
// The scanner mirrors the Rust validator's `template_variables`
// (`src-tauri/src/actions/schema.rs`): `{{` up to the next `}}`, the name
// verbatim — so `{{ file.path }}` is an unknown variable, not silently left
// alone. A missing value interpolates as `""` (§8.2).
//
// Pure: no store, no Tauri. `gatherRunVariables` in `index.ts` fills the
// values from the live shell.

import { RUN_VARIABLES, type RunVariable } from '../types';

export type RunVariables = Record<RunVariable, string>;

/** Where interpolated values land — decides the escaping. */
export type InterpolationMode = 'shell-posix' | 'shell-powershell' | 'uri' | 'raw';

export class UnknownVariableError extends Error {
	readonly variable: string;

	constructor(variable: string) {
		super(`\`{{${variable}}}\` is not a run variable (${RUN_VARIABLES.join(', ')})`);
		this.name = 'UnknownVariableError';
		this.variable = variable;
	}
}

export function isRunVariable(name: string): name is RunVariable {
	return (RUN_VARIABLES as readonly string[]).includes(name);
}

/** An all-empty variable set. */
export function emptyRunVariables(): RunVariables {
	return {
		'file.path': '',
		'file.name': '',
		selection: '',
		'project.root': '',
		'pane.url': '',
		branch: '',
	};
}

interface Segment {
	/** Literal text before the variable (or the tail when `name` is null). */
	text: string;
	name: string | null;
}

function segments(template: string): Segment[] {
	const out: Segment[] = [];
	let rest = template;
	for (;;) {
		const start = rest.indexOf('{{');
		if (start < 0) break;
		const after = rest.slice(start + 2);
		const end = after.indexOf('}}');
		if (end < 0) break;
		out.push({ text: rest.slice(0, start), name: after.slice(0, end) });
		rest = after.slice(end + 2);
	}
	out.push({ text: rest, name: null });
	return out;
}

/** Every `{{name}}` in a template, verbatim (same rule as the Rust validator). */
export function templateVariables(template: string): string[] {
	return segments(template)
		.map((segment) => segment.name)
		.filter((name): name is string => name !== null);
}

/** POSIX `sh`: single-quote, and close-escape-reopen every `'`. */
export function quotePosix(value: string): string {
	return `'${value.replace(/'/g, `'\\''`)}'`;
}

/**
 * PowerShell verbatim string. PowerShell also treats the typographic single
 * quotes (U+2018..U+201B) as quote characters, so each of them is doubled
 * too — otherwise `‘` would end the string.
 */
export function quotePowerShell(value: string): string {
	return `'${value.replace(/['‘’‚‛]/g, (quote) => quote + quote)}'`;
}

function escapeFor(mode: InterpolationMode, value: string): string {
	switch (mode) {
		case 'shell-posix':
			return quotePosix(value);
		case 'shell-powershell':
			return quotePowerShell(value);
		case 'uri':
			return encodeURIComponent(value);
		case 'raw':
			return value;
	}
}

/**
 * Interpolates `template`. Throws `UnknownVariableError` for a name outside
 * the six (a file that passed the validator never has one; a Test run of an
 * unsaved draft can).
 */
export function interpolate(
	template: string,
	variables: Partial<RunVariables>,
	mode: InterpolationMode
): string {
	const parts = segments(template);
	for (const part of parts) {
		if (part.name !== null && !isRunVariable(part.name)) throw new UnknownVariableError(part.name);
	}
	// §8.2 `open`: a url that is exactly one variable is taken whole.
	if (mode === 'uri' && parts.length === 2 && parts[0].text === '' && parts[1].text === '') {
		const name = parts[0].name as RunVariable;
		return variables[name] ?? '';
	}
	let out = '';
	for (const part of parts) {
		out += part.text;
		if (part.name !== null) out += escapeFor(mode, variables[part.name as RunVariable] ?? '');
	}
	return out;
}

/** Whether any of `templates` names `variable` (to skip costly lookups). */
export function mentionsVariable(variable: RunVariable, ...templates: (string | undefined)[]): boolean {
	return templates.some((template) => template != null && templateVariables(template).includes(variable));
}

/** Basename of a POSIX or Windows path (`file.name`, DEC-63.4). */
export function basename(path: string): string {
	const trimmed = path.replace(/[\\/]+$/, '');
	const cut = Math.max(trimmed.lastIndexOf('/'), trimmed.lastIndexOf('\\'));
	return cut >= 0 ? trimmed.slice(cut + 1) : trimmed;
}
