// The preview size cap (WP-06). `fs_read` hands back a whole file as a JSON
// number array, so the daemon and the desktop command refuse a file over
// FS_PREVIEW_MAX_BYTES before reading a byte. The refusal text starts with
// `preview_too_large: <size> > <limit>` on both surfaces (the browser
// transport prefixes the command name and also copies `error_data.code` onto
// the error), so a message match is enough for a renderer that only kept the
// message of a failed read.

/** Mirror of `FS_PREVIEW_MAX_BYTES` in `src-tauri/src/server/shared/fs.rs` (25 MiB). */
export const FS_PREVIEW_MAX_BYTES = 25 * 1024 * 1024;

/** The stable code of the refusal (Rust `PREVIEW_TOO_LARGE`). */
export const PREVIEW_TOO_LARGE_CODE = 'preview_too_large';

function textOf(e: unknown): string {
	if (typeof e === 'string') return e;
	if (e instanceof Error) return e.message;
	return '';
}

/** True when `e` (an Error, a message, or a typed rejection object) is the too-large refusal. */
export function isPreviewTooLarge(e: unknown): boolean {
	if (e && typeof e === 'object' && (e as { code?: unknown }).code === PREVIEW_TOO_LARGE_CODE) {
		return true;
	}
	return textOf(e).includes(PREVIEW_TOO_LARGE_CODE + ':');
}

/** The size in bytes the refusal reports, or `null` when it carries none. */
export function previewTooLargeSize(e: unknown): number | null {
	if (e && typeof e === 'object') {
		const size = (e as { size?: unknown }).size;
		if (typeof size === 'number') return size;
	}
	const m = /preview_too_large:\s*(\d+)/.exec(textOf(e));
	return m ? Number(m[1]) : null;
}

export function formatBytes(n: number): string {
	if (n < 1024) return `${n} B`;
	if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
	if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
	return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}
