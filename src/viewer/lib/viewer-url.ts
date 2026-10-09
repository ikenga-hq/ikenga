// D-08 pane `⋯` menu — "Open in browser" / "Copy viewer URL". Only HTML
// artifacts have a real viewer URL: the axum viewer server
// (`src-tauri/src/commands/viewer.rs`) mounts a static root and serves it
// over `http://localhost:<port>`, and `HtmlFrame` (renderers/html-frame.tsx)
// is the only renderer that uses it. Every other renderer reads the file
// straight off disk (`fsRead`) and has no server-backed URL to open or copy.
//
// Mirrors HtmlFrame's own resolution (fsRead → resolveViewerRoot → viewerServe →
// viewerPort) rather than reaching into its component state, so the menu
// action works even before — or after — HtmlFrame's own mount is live. The
// tradeoff: each call mounts its own viewer-server root (a cheap, harmless
// token registration that lives until the app restarts, same as any other
// `viewerServe` caller that doesn't pair it with `viewerStop`).

import { fsRead, isRemoteWebSession, viewerPort, viewerServe } from '@/lib/tauri-cmd';
import { resolveViewerRoot } from './viewer-root';

/** Default bound port, matched to `html-frame.tsx`'s own fallback — only hit
 *  if `viewer_port` ever returns `null` (server failed to bind). */
const FALLBACK_PORT = 47821;

export function isHtmlArtifactPath(path: string): boolean {
	const lower = path.toLowerCase();
	return lower.endsWith('.html') || lower.endsWith('.htm');
}

export async function resolveHtmlViewerUrl(path: string): Promise<string> {
	// In-app previews only in a browser session (founder decision, gap audit
	// rank 8): the menu rows are hidden, and a URL minted here would register a
	// mount no pane ever stops. Refuse rather than leak one.
	if (isRemoteWebSession()) {
		throw new Error('Not available on this server: viewer URLs are only offered in the desktop app');
	}
	const res = await fsRead(path);
	const html = new TextDecoder('utf-8', { fatal: false }).decode(new Uint8Array(res.bytes));
	const { root, file } = await resolveViewerRoot(path, html);
	const [handle, port] = await Promise.all([viewerServe(root, path), viewerPort()]);
	return `http://localhost:${port ?? FALLBACK_PORT}${handle.url}${file}`;
}
