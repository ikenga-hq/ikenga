import { describe, expect, it } from 'vitest';
import {
	FS_PREVIEW_MAX_BYTES,
	formatBytes,
	isPreviewTooLarge,
	previewTooLargeSize,
} from './preview-cap';

const msg = `preview_too_large: ${FS_PREVIEW_MAX_BYTES + 1} > ${FS_PREVIEW_MAX_BYTES}`;

describe('preview cap helpers', () => {
	it('recognises the refusal on desktop (bare) and web (command-prefixed) text', () => {
		expect(isPreviewTooLarge(msg)).toBe(true);
		expect(isPreviewTooLarge(new Error(`fs_read: ${msg}`))).toBe(true);
		expect(previewTooLargeSize(new Error(`fs_read: ${msg}`))).toBe(FS_PREVIEW_MAX_BYTES + 1);
	});

	it('recognises the typed web error (error_data copied onto the Error)', () => {
		const e = Object.assign(new Error('x'), { code: 'preview_too_large', size: 123 });
		expect(isPreviewTooLarge(e)).toBe(true);
		expect(previewTooLargeSize(e)).toBe(123);
	});

	it('does not claim other failures', () => {
		expect(isPreviewTooLarge('read failed: not found')).toBe(false);
		expect(isPreviewTooLarge(new Error('HTTP RPC error: 500'))).toBe(false);
		expect(isPreviewTooLarge(undefined)).toBe(false);
		expect(previewTooLargeSize('nope')).toBeNull();
	});

	it('formats sizes', () => {
		expect(formatBytes(512)).toBe('512 B');
		expect(formatBytes(FS_PREVIEW_MAX_BYTES)).toBe('25.0 MB');
	});
});
