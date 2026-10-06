import { describe, expect, it } from 'vitest';
import {
	jsonPosition,
	validateJson,
	validateJsonLines,
	validateToml,
	validateYaml,
	validationKindFor,
	validatorFor,
} from './validate';

describe('validationKindFor / validatorFor', () => {
	it('maps extensions to checks', () => {
		expect(validationKindFor('/a/b.json')).toBe('json');
		expect(validationKindFor('/a/b.JSONL')).toBe('jsonl');
		expect(validationKindFor('/a/b.yml')).toBe('yaml');
		expect(validationKindFor('/a/b.yaml')).toBe('yaml');
		expect(validationKindFor('/a/b.toml')).toBe('toml');
		expect(validationKindFor('/a/b.json5')).toBe('unvalidated');
		expect(validationKindFor('/a/b.csv')).toBe('none');
		expect(validationKindFor('/a/b.ts')).toBe('none');
	});

	it('gives no validator for CSV, JSON5 or plain code', () => {
		expect(validatorFor('/a.csv')).toBeNull();
		expect(validatorFor('/a.json5')).toBeNull();
		expect(validatorFor('/a.py')).toBeNull();
		expect(validatorFor('/a.json')).not.toBeNull();
	});
});

describe('JSON', () => {
	it('accepts valid JSON', () => {
		expect(validateJson('{"a": [1, 2]}')).toEqual({ ok: true });
	});

	it('rejects invalid JSON with the parse error', () => {
		const r = validateJson('{"a": 1,\n "b": }');
		expect(r.ok).toBe(false);
		if (r.ok) return;
		expect(r.message).toMatch(/^Invalid JSON: /);
	});

	it('derives line/col from a V8 position', () => {
		expect(jsonPosition('Unexpected token } in JSON at position 9', '{"a": 1,\n}')).toEqual({
			line: 2,
			col: 1,
		});
		expect(jsonPosition('… in JSON at position 7 (line 1 column 8)', 'x')).toEqual({
			line: 1,
			col: 8,
		});
		// WebKit gives no position: message only.
		expect(jsonPosition("JSON Parse error: Expected '}'", '{')).toEqual({});
	});
});

describe('JSON Lines', () => {
	it('accepts one document per line, blank lines allowed', () => {
		expect(validateJsonLines('{"a":1}\n\n[2]\n')).toEqual({ ok: true });
	});

	it('reports the failing line number', () => {
		const r = validateJsonLines('{"a":1}\n{"b":\n{"c":3}');
		expect(r.ok).toBe(false);
		if (r.ok) return;
		expect(r.line).toBe(2);
		expect(r.message).toMatch(/line 2/);
	});
});

describe('YAML', () => {
	it('accepts a multi-document stream', async () => {
		expect(await validateYaml('a: 1\n---\nb: [1, 2]\n')).toEqual({ ok: true });
	});

	it('accepts an empty file', async () => {
		expect(await validateYaml('')).toEqual({ ok: true });
	});

	it('rejects invalid YAML with line/col', async () => {
		const r = await validateYaml('a: 1\n---\nb: [1, 2\nc: 3\n');
		expect(r.ok).toBe(false);
		if (r.ok) return;
		expect(r.message).toMatch(/^Invalid YAML: /);
		expect(r.message).not.toContain('\n');
		expect(r.line).toBeGreaterThanOrEqual(3);
		expect(r.col).toBeGreaterThanOrEqual(1);
	});
});

describe('TOML', () => {
	it('accepts valid TOML', async () => {
		expect(await validateToml('title = "x"\n[owner]\nname = "y"\n')).toEqual({ ok: true });
	});

	it('rejects invalid TOML with line/col', async () => {
		const r = await validateToml('a = 1\nb = [1,\nc');
		expect(r.ok).toBe(false);
		if (r.ok) return;
		expect(r.message).toMatch(/^Invalid TOML: /);
		expect(r.line).toBe(3);
		expect(r.col).toBe(1);
	});
});
