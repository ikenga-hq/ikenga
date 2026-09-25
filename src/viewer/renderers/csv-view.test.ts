// WP-44 — unit tests for the CSV/TSV parser backing `CsvView`. Written per
// DEC-50/G-51 (targeted units for new code); not run in this session
// (DEC-50 — no build/test until the branch closes, see the WP-44 report).

import { describe, expect, it } from 'vitest';
import { CSV_ROW_CAP, parseDelimited, renderedRowCount } from './csv-view';

describe('renderedRowCount', () => {
	it('caps a large file at the step size and never exceeds the total', () => {
		expect(renderedRowCount(500_000, CSV_ROW_CAP)).toBe(CSV_ROW_CAP);
		expect(renderedRowCount(500_000, CSV_ROW_CAP * 2)).toBe(CSV_ROW_CAP * 2);
		expect(renderedRowCount(12, CSV_ROW_CAP)).toBe(12);
		expect(renderedRowCount(0, CSV_ROW_CAP)).toBe(0);
	});
});

describe('parseDelimited', () => {
	it('splits a simple comma-delimited body from its header', () => {
		const { header, rows } = parseDelimited('a,b,c\n1,2,3\n4,5,6\n', ',');
		expect(header).toEqual(['a', 'b', 'c']);
		expect(rows).toEqual([
			['1', '2', '3'],
			['4', '5', '6'],
		]);
	});

	it('handles a quoted field containing the delimiter', () => {
		const { header, rows } = parseDelimited('name,note\n"Doe, Jane",ok\n', ',');
		expect(header).toEqual(['name', 'note']);
		expect(rows).toEqual([['Doe, Jane', 'ok']]);
	});

	it('handles a quoted field containing an embedded newline', () => {
		const { rows } = parseDelimited('a,b\n"line1\nline2",x\n', ',');
		expect(rows).toEqual([['line1\nline2', 'x']]);
	});

	it('un-escapes a doubled quote inside a quoted field', () => {
		const { rows } = parseDelimited('a\n"she said ""hi"""\n', ',');
		expect(rows).toEqual([['she said "hi"']]);
	});

	it('tolerates \\r\\n line endings', () => {
		const { header, rows } = parseDelimited('a,b\r\n1,2\r\n', ',');
		expect(header).toEqual(['a', 'b']);
		expect(rows).toEqual([['1', '2']]);
	});

	it('does not emit a trailing empty row for a final newline', () => {
		const { rows } = parseDelimited('a,b\n1,2\n', ',');
		expect(rows).toEqual([['1', '2']]);
	});

	it('parses a file with no trailing newline', () => {
		const { header, rows } = parseDelimited('a,b\n1,2', ',');
		expect(header).toEqual(['a', 'b']);
		expect(rows).toEqual([['1', '2']]);
	});

	it('supports tab as the delimiter for .tsv', () => {
		const { header, rows } = parseDelimited('a\tb\n1\t2\n', '\t');
		expect(header).toEqual(['a', 'b']);
		expect(rows).toEqual([['1', '2']]);
	});

	it('drops a genuinely blank trailing line', () => {
		const { rows } = parseDelimited('a,b\n1,2\n\n', ',');
		expect(rows).toEqual([['1', '2']]);
	});

	it('returns an empty header and no rows for an empty file', () => {
		const { header, rows } = parseDelimited('', ',');
		expect(header).toEqual([]);
		expect(rows).toEqual([]);
	});
});
