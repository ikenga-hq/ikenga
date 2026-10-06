import { describe, expect, it } from 'vitest';
import {
	decodeForEdit,
	encodeForSave,
	formatBytes,
	isConflict,
	MAX_EDITABLE_BYTES,
	normaliseText,
} from './text-document';

const enc = (s: string) => new TextEncoder().encode(s);

describe('decodeForEdit / encodeForSave', () => {
	it('round-trips plain LF text unchanged', () => {
		const d = decodeForEdit(enc('a\nb\n'));
		expect(d.ok).toBe(true);
		if (!d.ok) return;
		expect(d.text).toBe('a\nb\n');
		expect(d.meta).toEqual({ eol: '\n', bom: false });
		expect(encodeForSave(d.text, d.meta)).toBe('a\nb\n');
	});

	it('edits CRLF files as LF and writes CRLF back', () => {
		const d = decodeForEdit(enc('one\r\ntwo\r\n'));
		if (!d.ok) throw new Error('expected ok');
		expect(d.text).toBe('one\ntwo\n');
		expect(d.meta.eol).toBe('\r\n');
		// CodeMirror hands back LF; a new line typed in the editor is LF too.
		expect(encodeForSave(`${d.text}three\n`, d.meta)).toBe('one\r\ntwo\r\nthree\r\n');
	});

	it('preserves a UTF-8 BOM', () => {
		const bytes = new Uint8Array([0xef, 0xbb, 0xbf, ...enc('x: 1\n')]);
		const d = decodeForEdit(bytes);
		if (!d.ok) throw new Error('expected ok');
		expect(d.text).toBe('x: 1\n');
		expect(d.meta.bom).toBe(true);
		const out = enc(encodeForSave(d.text, d.meta));
		expect(Array.from(out.slice(0, 3))).toEqual([0xef, 0xbb, 0xbf]);
	});

	it('blocks invalid UTF-8 with a reason', () => {
		const d = decodeForEdit(new Uint8Array([0x61, 0xff, 0x62]));
		expect(d.ok).toBe(false);
		if (d.ok) return;
		expect(d.reason).toMatch(/not valid UTF-8/);
	});

	it('blocks a file containing a NUL byte as binary', () => {
		const d = decodeForEdit(new Uint8Array([0x61, 0x00, 0x62]));
		expect(d.ok).toBe(false);
		if (d.ok) return;
		expect(d.reason).toMatch(/binary/);
	});

	it('blocks a file above the size limit, naming size and limit', () => {
		const d = decodeForEdit(new Uint8Array(MAX_EDITABLE_BYTES + 1).fill(0x61));
		expect(d.ok).toBe(false);
		if (d.ok) return;
		expect(d.reason).toMatch(/too large to edit here \(limit 2\.0 MB\)/);
		expect(d.size).toBe(MAX_EDITABLE_BYTES + 1);
	});

	it('accepts a file exactly at the limit', () => {
		expect(decodeForEdit(new Uint8Array(MAX_EDITABLE_BYTES).fill(0x61)).ok).toBe(true);
	});

	it('accepts a plain number[] (the fs_read wire shape)', () => {
		const d = decodeForEdit(Array.from(enc('hé')));
		expect(d.ok && d.text).toBe('hé');
	});
});

describe('isConflict', () => {
	it('is false when the disk still matches the base', () => {
		expect(isConflict('a', 'a')).toBe(false);
	});
	it('is true when the disk moved on', () => {
		expect(isConflict('a', 'c')).toBe(true);
	});
	// Regression: a stale "our last write" (A1) once excused a disk that had
	// been reverted to it after the editor adopted a newer base (A2), so a save
	// built on A2 overwrote the revert without asking.
	it('is true when the disk went back to an older text than the base', () => {
		expect(isConflict('A2', 'A1')).toBe(true);
	});
});

describe('helpers', () => {
	it('normaliseText strips a BOM and CRLF', () => {
		expect(normaliseText('﻿a\r\nb')).toBe('a\nb');
	});
	it('formatBytes', () => {
		expect(formatBytes(10)).toBe('10 B');
		expect(formatBytes(1536)).toBe('1.5 KB');
		expect(formatBytes(3 * 1024 * 1024)).toBe('3.0 MB');
	});
});
