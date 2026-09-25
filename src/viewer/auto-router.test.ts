// WP-44 — unit tests for `pickRenderer`, the viewer's mime/extension →
// renderer dispatch. Focused on the D-08 delta (CSV/JSON routing, and that
// JSON Lines is deliberately excluded from the new JSON tree renderer) plus
// a couple of pre-existing branches so a reorder doesn't silently regress
// them. Not run in this session (DEC-50 — see the WP-44 report).

import { describe, expect, it } from 'vitest';
import { CodeView, PdfView, pickRenderer, XlsxView } from './auto-router';
import { AudioView } from './renderers/audio-view';
import { CsvView } from './renderers/csv-view';
import { HtmlFrame } from './renderers/html-frame';
import { ImageView } from './renderers/image-view';
import { JsonView } from './renderers/json-view';
import { MarkdownView } from './renderers/markdown-view';
import { PenView } from './renderers/pen-view';
import { UnknownView } from './renderers/unknown-view';
import { VideoView } from './renderers/video-view';

describe('pickRenderer — new in WP-44', () => {
	it('routes .csv (and a resolved text/csv mime) to CsvView, not CodeView', () => {
		expect(pickRenderer('text/csv', 'data/royalties-2026.csv')).toBe(CsvView);
		expect(pickRenderer('application/octet-stream', 'data/royalties-2026.csv')).toBe(CsvView);
	});

	it('routes .tsv to CsvView too', () => {
		expect(pickRenderer('text/tab-separated-values', 'data/export.tsv')).toBe(CsvView);
	});

	it('routes .json (and a resolved application/json mime) to JsonView', () => {
		expect(pickRenderer('application/json', 'config/tsconfig.json')).toBe(JsonView);
		expect(pickRenderer('text/plain', 'config/settings.json5')).toBe(JsonView);
	});

	it('keeps .jsonl in CodeView — it is many JSON documents, not one tree', () => {
		expect(pickRenderer('application/json', 'logs/events.jsonl')).toBe(CodeView);
	});

	it('still routes .pen ahead of the generic JSON check (Pencil files are JSON on disk)', () => {
		expect(pickRenderer('application/x-pencil', 'design/board.pen')).toBe(PenView);
	});
});

describe('pickRenderer — pre-existing branches unaffected', () => {
	it.each([
		['text/html', 'artifacts/pulse-dashboard.html', HtmlFrame],
		['application/pdf', 'legal/contract.pdf', PdfView],
		['image/png', 'design/carve-motif.png', ImageView],
		['video/mp4', 'captures/session-3.mp4', VideoView],
		['audio/mp4', 'captures/voice-note.m4a', AudioView],
		['text/markdown', 'README.md', MarkdownView],
		['application/vnd.openxmlformats-officedocument.spreadsheetml.sheet', 'finance/statement-q1.xlsx', XlsxView],
		['text/x-typescript', 'src/lib/shell-store.ts', CodeView],
		['application/octet-stream', 'dist/Ikenga.AppImage', UnknownView],
	])('%s / %s → the expected renderer', (mime, path, expected) => {
		expect(pickRenderer(mime, path)).toBe(expected);
	});
});
