// Tiny path helpers. Tauri ships `@tauri-apps/api/path` but it's async and
// platform-aware; for the viewer we only ever deal with already-resolved
// absolute POSIX-style paths, so the JS-side string ops are sufficient and
// avoid an extra IPC round-trip on every render.

export function dirname(path: string): string {
	const norm = path.replace(/\\/g, '/');
	const idx = norm.lastIndexOf('/');
	if (idx <= 0) return '/';
	return norm.slice(0, idx);
}

export function basename(path: string): string {
	const norm = path.replace(/\\/g, '/');
	const idx = norm.lastIndexOf('/');
	return idx === -1 ? norm : norm.slice(idx + 1);
}

/** Percent-encode each segment of a relative URL path, keeping the `/`
 *  separators. A name like `Report #2 50%.html` must not reach the URL raw:
 *  `#` starts a fragment, `?` a query, and `%` a bad escape. Both viewer
 *  servers (desktop `viewer_server`, daemon `server/viewer.rs`) decode the
 *  path exactly once, so this does not double-encode. */
export function encodeUrlPath(rel: string): string {
	return rel.split('/').map(encodeURIComponent).join('/');
}

export function extname(path: string): string {
	const base = basename(path);
	const dot = base.lastIndexOf('.');
	if (dot === -1 || dot === 0) return '';
	return base.slice(dot).toLowerCase();
}
