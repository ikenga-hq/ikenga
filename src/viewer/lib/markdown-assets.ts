// Relative images inside a rendered Markdown document.
//
// react-markdown renders `![](./chart.png)` as a plain <img src="./chart.png">,
// which a browser resolves against the *app's* origin, not the document's
// folder, so it never loads. The viewer instead mounts the document's folder on
// the viewer server (the same mount the HTML viewer uses) and rewrites each
// relative source to a URL under that mount. The mount root IS the document's
// folder, so a source that climbs out of it is refused here and could not be
// served by the host either.

import { encodeUrlPath } from './path';

/** Cheap test for "this document references an image", so a document with no
 *  images never mounts anything. */
export function hasImageRefs(markdown: string): boolean {
	return /!\[[^\]]*\]\(|<img\b/i.test(markdown);
}

/** http:, https:, data:, blob:, any scheme, or protocol-relative (//host). */
function isExternal(src: string): boolean {
	return /^[a-z][a-z0-9+.-]*:/i.test(src) || src.startsWith('//');
}

export type ImageSource =
	/** Leave the source exactly as written (absolute http(s), data:, ...). */
	| { kind: 'external'; src: string }
	/** A path inside the document folder, percent-encoded, no leading slash. */
	| { kind: 'relative'; rel: string }
	/** Climbs out of the document folder, is rooted, or is unusable. */
	| { kind: 'blocked' };

/** Classify a markdown image source. Pure; the mount URL is added by the caller. */
export function classifyImageSrc(raw: string): ImageSource {
	const src = raw.trim();
	if (src === '') return { kind: 'blocked' };
	if (isExternal(src)) return { kind: 'external', src };
	if (src.startsWith('/') || src.startsWith('\\') || src.startsWith('~'))
		return { kind: 'blocked' };

	const pathPart = src.split(/[?#]/, 1)[0] ?? '';
	let decoded: string;
	try {
		decoded = decodeURIComponent(pathPart);
	} catch {
		return { kind: 'blocked' };
	}
	const out: string[] = [];
	for (const seg of decoded.replace(/\\/g, '/').split('/')) {
		if (seg === '' || seg === '.') continue;
		if (seg === '..') {
			if (out.length === 0) return { kind: 'blocked' };
			out.pop();
			continue;
		}
		out.push(seg);
	}
	if (out.length === 0) return { kind: 'blocked' };
	return { kind: 'relative', rel: encodeUrlPath(out.join('/')) };
}

/** Resolver handed to <Markdown>: the final src, `null` for a refused source,
 *  `undefined` while the folder is still being mounted. */
export type ImageResolver = (src: string) => string | null | undefined;

/** Build a resolver over a mount base URL (`.../v/<token>/`), or `null` when
 *  the mount is not ready yet. */
export function makeImageResolver(base: string | null): ImageResolver {
	return (src) => {
		const c = classifyImageSrc(src);
		if (c.kind === 'external') return c.src;
		if (c.kind === 'blocked') return null;
		return base === null ? undefined : `${base}${c.rel}`;
	};
}
