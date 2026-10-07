// Credential-stripping for route paths. A leaf module (no pane imports) so
// both the reducer and url-sync can use it without an import cycle.

/** Query params that carry a credential and must never be written back into
 *  the address bar (or a pane path, which is persisted). `?token=` is the T0
 *  link credential — `getAuthToken` strips it on first read and nothing may
 *  re-add it. The other URL-borne secrets (pair code, invite token) ride in
 *  the hash, which these helpers never carry at all. */
const SENSITIVE_PARAMS = ['token'] as const;

/**
 * `path` as it may appear in the address bar: the hash dropped and every
 * {@link SENSITIVE_PARAMS} removed from the query. A path with no sensitive
 * param keeps its query byte-for-byte (no re-serialising), so this never
 * churns an otherwise-equal path into a "different" one.
 */
export function sanitizeRoutePath(path: string): string {
	const hashAt = path.indexOf('#');
	const noHash = hashAt >= 0 ? path.slice(0, hashAt) : path;
	const qAt = noHash.indexOf('?');
	if (qAt < 0) return noHash || '/';
	const pathname = noHash.slice(0, qAt) || '/';
	const params = new URLSearchParams(noHash.slice(qAt + 1));
	if (!SENSITIVE_PARAMS.some((k) => params.has(k))) return noHash;
	for (const k of SENSITIVE_PARAMS) params.delete(k);
	const rest = params.toString();
	return rest ? `${pathname}?${rest}` : pathname;
}
