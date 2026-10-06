// Pure text-document helpers for the shared editing surface (plans/file-editing
// Shape 1, 2 and 5). No React, no I/O — unit-tested in text-document.test.ts.
//
// Why a decode/encode pair instead of a bare TextDecoder: CodeMirror hands back
// LF-only text, and the old renderers decoded with `fatal: false` and let the
// decoder strip a BOM. A save through that path silently rewrote CRLF files to
// LF, dropped the BOM, and turned any non-UTF-8 byte into U+FFFD on disk. Here
// the editor always works on LF text, and `encodeForSave` puts the file's own
// line ending and BOM back; files that are not clean UTF-8 never reach the
// editor at all.

/** Files above this size open read-only (Shape 5). Same on desktop and in a
 *  browser. 2 MiB is well under the daemon's RPC body limit even with JSON
 *  escaping, and keeps CodeMirror responsive. */
export const MAX_EDITABLE_BYTES = 2 * 1024 * 1024;

export type Eol = '\n' | '\r\n';

export interface DocumentMeta {
	eol: Eol;
	bom: boolean;
}

export type DecodedDocument =
	| { ok: true; text: string; meta: DocumentMeta; size: number }
	| { ok: false; reason: string; size: number };

const BOM = '﻿';

/** Human size for messages: "512 B", "1.4 KB", "3.2 MB". */
export function formatBytes(n: number): string {
	if (n < 1024) return `${n} B`;
	if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
	return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

/** Lossy decode for *viewing* only — never for a buffer that will be saved. */
export function decodeForView(bytes: number[] | Uint8Array): string {
	return new TextDecoder('utf-8', { fatal: false }).decode(toU8(bytes));
}

/**
 * Decode file bytes for editing. Refuses (with a reason the UI shows) when the
 * file is too large, contains a NUL byte (binary), or is not valid UTF-8 —
 * editing any of those would corrupt the file on save.
 */
export function decodeForEdit(
	bytes: number[] | Uint8Array,
	maxBytes: number = MAX_EDITABLE_BYTES
): DecodedDocument {
	const u8 = toU8(bytes);
	const size = u8.byteLength;
	if (size > maxBytes) {
		return {
			ok: false,
			size,
			reason: `This file is ${formatBytes(size)}, too large to edit here (limit ${formatBytes(maxBytes)}).`,
		};
	}
	if (u8.indexOf(0) !== -1) {
		return { ok: false, size, reason: 'This file looks binary, so it opens read-only.' };
	}
	let raw: string;
	try {
		raw = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(u8);
	} catch {
		return {
			ok: false,
			size,
			reason: 'This file is not valid UTF-8, so it opens read-only to avoid corrupting it.',
		};
	}
	const bom = raw.startsWith(BOM);
	if (bom) raw = raw.slice(1);
	const eol = dominantEol(raw);
	const text = raw.includes('\r\n') ? raw.replace(/\r\n/g, '\n') : raw;
	return { ok: true, text, meta: { eol, bom }, size };
}

/** Re-apply the file's line ending and BOM to LF editor text. */
export function encodeForSave(text: string, meta: DocumentMeta): string {
	const body = meta.eol === '\r\n' ? text.replace(/\r?\n/g, '\r\n') : text;
	return meta.bom ? BOM + body : body;
}

/** Normalise any decoded text the way `decodeForEdit` does (BOM off, CRLF →
 *  LF) so a re-read can be compared with the editor's base. */
export function normaliseText(raw: string): string {
	const s = raw.startsWith(BOM) ? raw.slice(1) : raw;
	return s.includes('\r\n') ? s.replace(/\r\n/g, '\n') : s;
}

/** True when the on-disk text no longer matches what the editor loaded and is
 *  not simply our own last write. A plain string compare: exact, and needs no
 *  `crypto.subtle` (absent on plain-http daemon origins). */
export function isConflict(base: string, disk: string, lastSaved: string | null): boolean {
	return disk !== base && disk !== lastSaved;
}

function dominantEol(text: string): Eol {
	let crlf = 0;
	let lf = 0;
	for (let i = text.indexOf('\n'); i !== -1; i = text.indexOf('\n', i + 1)) {
		if (i > 0 && text.charCodeAt(i - 1) === 13) crlf++;
		else lf++;
	}
	return crlf > lf ? '\r\n' : '\n';
}

function toU8(bytes: number[] | Uint8Array): Uint8Array {
	return bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
}
