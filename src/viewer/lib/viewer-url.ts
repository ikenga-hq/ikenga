// D-08 pane `⋯` menu — "Open in browser" / "Copy viewer URL". Only HTML
// artifacts have a real viewer URL: the axum viewer server
// (`src-tauri/src/commands/viewer.rs`) mounts a static root and serves it
// over `http://localhost:<port>`, and `HtmlFrame` (renderers/html-frame.tsx)
// is the only renderer that uses it. Every other renderer reads the file
// straight off disk (`fsRead`) and has no server-backed URL to open or copy.
//
// Mirrors HtmlFrame's own resolution (fsRead → pickViewerRoot → viewerServe →
// viewerPort) rather than reaching into its component state, so the menu
// action works even before — or after — HtmlFrame's own mount is live. The
// tradeoff: each call mounts its own viewer-server root (a cheap, harmless
// token registration that lives until the app restarts, same as any other
// `viewerServe` caller that doesn't pair it with `viewerStop`).

import { fsRead, viewerPort, viewerServe } from '@/lib/tauri-cmd';
import { pickViewerRoot } from './relative-root';

/** Default bound port, matched to `html-frame.tsx`'s own fallback — only hit
 *  if `viewer_port` ever returns `null` (server failed to bind). */
const FALLBACK_PORT = 47821;

export function isHtmlArtifactPath(path: string): boolean {
	const lower = path.toLowerCase();
	return lower.endsWith('.html') || lower.endsWith('.htm');
}

export async function resolveHtmlViewerUrl(path: string): Promise<string> {
	const res = await fsRead(path);
	const html = new TextDecoder('utf-8', { fatal: false }).decode(new Uint8Array(res.bytes));
	const { root, file } = pickViewerRoot(path, html);
	const [handle, port] = await Promise.all([viewerServe(root), viewerPort()]);
	return `http://localhost:${port ?? FALLBACK_PORT}${handle.url}${file}`;
}
