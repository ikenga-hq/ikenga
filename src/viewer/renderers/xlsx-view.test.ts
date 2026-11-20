import { describe, expect, it } from 'vitest';
import * as XLSX from 'xlsx';
import { CSV_ROW_CAP } from './csv-view';
import { rowCapNote, sheetRows, XLSX_READ_OPTS } from './xlsx-view';

describe('xlsx formatting', () => {
	it('shows dates and formatted numbers as text, not serials', () => {
		const ws = XLSX.utils.aoa_to_sheet([['when', 'amount']]);
		ws['A2'] = { t: 'd', v: new Date(Date.UTC(2026, 10, 20)), z: 'yyyy-mm-dd' };
		ws['B2'] = { t: 'n', v: 1234.5, z: '#,##0.00' };
		ws['!ref'] = 'A1:B2';
		const bytes = XLSX.write({ SheetNames: ['S'], Sheets: { S: ws } }, { type: 'array', bookType: 'xlsx', cellDates: true });
		const wb = XLSX.read(new Uint8Array(bytes), XLSX_READ_OPTS);
		const rows = sheetRows(wb.Sheets['S']!);
		expect(rows[1]![0]).toContain('2026');
		expect(rows[1]![1]).toBe('1,234.50');
	});
});

describe('rowCapNote', () => {
	it('is null when everything fits', () => {
		expect(rowCapNote(10, CSV_ROW_CAP)).toBeNull();
		expect(rowCapNote(CSV_ROW_CAP, CSV_ROW_CAP)).toBeNull();
	});
	it('names the cap and the total when truncated', () => {
		const note = rowCapNote(100_000, CSV_ROW_CAP)!;
		expect(note).toContain('first');
		expect(note).toContain(CSV_ROW_CAP.toLocaleString());
		expect(note).toContain((100_000).toLocaleString());
	});
});
