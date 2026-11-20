import { isDeviceSession } from './device-session';
import { currentShare, isBrowserHost, transportToken } from './index';
import { downloadServerFile, openLocalPath, saveBlobAs } from './shims';
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
export function fsDownloadUrl(
	path: string,
	opts: { inline?: boolean; share?: string } = {}
): string {
	const q = new URLSearchParams({ path });
	if (opts.inline) q.set('inline', '1');
	// A navigation cannot send the `X-Ikenga-Share` header, so a share member
	// selects the share through `?share=` (the broker reads either).
	if (opts.share) q.set('share', opts.share);
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

/**
 * Save the file at `path` — the one download path every control uses.
 *
 * - Desktop: unchanged, the OS opens the file in its default app.
 * - Browser with a cookie credential (T1 session or paired device): a
 *   navigation to {@link fsDownloadUrl}. The daemon streams it with
 *   `Content-Disposition: attachment`, so nothing is buffered in JS and the
 *   browser's own save dialog does the rest. Inside a share the share is
 *   selected with `?share=`.
 * - Browser with a single-user bearer token: a navigation cannot attach the
 *   header, so the same streamed route is fetched with it and the bytes are
 *   saved from a Blob. (Not `fs_read`: that is the preview read, capped at
 *   25 MiB and sent as JSON.) The file is held in memory for the save.
 *
 * Rejects only on the fallback paths; a navigation reports its own failure in
 * the browser's download UI.
 */
export async function downloadFile(path: string): Promise<void> {
	if (!isBrowserHost()) {
		await openLocalPath(path, { kind: 'file' });
		return;
	}
	const name = path.split(/[\\/]/).filter(Boolean).pop() ?? 'download';
	if (!canDownloadByNavigation()) {
		const token = transportToken();
		if (!token) {
			await downloadServerFile(path);
			return;
		}
		const res = await fetch(fsDownloadUrl(path), {
			headers: { Authorization: `Bearer ${token}` },
		});
		if (!res.ok) {
			const body = (await res.json().catch(() => null)) as { error?: unknown } | null;
			throw new Error(
				typeof body?.error === 'string' ? body.error : `${res.status} ${res.statusText}`
			);
		}
		saveBlobAs(await res.blob(), name);
		return;
	}
	const url = fsDownloadUrl(path, { share: currentShare()?.projectKey });
	const a = document.createElement('a');
	a.href = url;
	// `download` keeps the SPA in place even if the server answers an error.
	a.download = name;
	a.rel = 'noopener';
	document.body.appendChild(a);
	a.click();
	a.remove();
}
