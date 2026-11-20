import { isDeviceSession } from './device-session';
import { isBrowserHost } from './index';
import { isT1Session } from './t1-session';

/**
 * The URL of the daemon's streamed download route (`GET /api/fs/download`).
 *
 * The route authorizes and narrows exactly like `fs_read` and answers with
 * `Content-Disposition: attachment`, so a plain navigation to it saves the file
 * through the browser's own save dialog and nothing is buffered in JS.
 *
 * `inline` asks the daemon for an inline disposition; it is honoured only for
 * passive types (pdf, png, jpeg, gif, webp, plain text). HTML and SVG are
 * always served as attachments.
 */
export function fsDownloadUrl(path: string, opts: { inline?: boolean } = {}): string {
	const q = new URLSearchParams({ path });
	if (opts.inline) q.set('inline', '1');
	return `/api/fs/download?${q.toString()}`;
}

/**
 * True when a navigation to {@link fsDownloadUrl} authenticates: a browser tab
 * whose credential is a cookie (a T1 session or a paired device). A single-user
 * bearer-token tab cannot attach the header to a navigation, so the caller must
 * fall back to another path there.
 */
export function canDownloadByNavigation(): boolean {
	return isBrowserHost() && (isT1Session() || isDeviceSession());
}
