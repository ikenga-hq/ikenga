/**
 * Shared file-path detection + sync resolution.
 *
 * Extracted from `components/markdown.tsx` so the markdown renderer and the
 * xterm terminal link provider share one definition of "this token looks like
 * a file path" — they must agree, or a path that linkifies in one surface
 * would fail to linkify in the other (and vice versa).
 *
 * Everything here is pure (no React, no pane store). The async
 * monorepo-disambiguation walk and the `FilePathPill` component stay in
 * `markdown.tsx` — they're markdown-surface concerns layered on top of these
 * primitives.
 */

import { getHomeSync } from '@/lib/home';

// Multi-segment path pattern (contains slash, optional tilde / relative prefix).
export const MULTI_SEGMENT_PATH_RE = /^(?:~?\/|~|\.\.?\/)?[a-zA-Z0-9_.@-][a-zA-Z0-9_./@-]*$/;

// Single-segment file pattern (no slash, requires .<ext>).
export const SINGLE_SEGMENT_PATH_RE = /^[a-zA-Z0-9_.@-]+\.[A-Za-z0-9]{1,7}$/;

// Backward-compatible alias for existing consumers.
export const PATH_RE = /^(?:~?\/|~|\.\.?\/)?[a-zA-Z0-9_.@-][a-zA-Z0-9_./@-]*$/;

// Restrict single-segment (no slash) paths to known dev/doc/asset extensions so
// things like `e.g.` or `Mr.A` don't get mistaken for files. Multi-segment
// paths (with `/`) fall through the regex with the standard checks. Lowercase
// for case-insensitive comparison.
export const KNOWN_EXTENSIONS = new Set([
	'md',
	'mdx',
	'txt',
	'rst',
	'ts',
	'tsx',
	'js',
	'jsx',
	'mjs',
	'cjs',
	'd.ts',
	'json',
	'json5',
	'jsonc',
	'yaml',
	'yml',
	'toml',
	'xml',
	'py',
	'go',
	'rs',
	'rb',
	'java',
	'kt',
	'swift',
	'c',
	'cpp',
	'cc',
	'cxx',
	'h',
	'hpp',
	'hh',
	'css',
	'scss',
	'sass',
	'less',
	'html',
	'htm',
	'svg',
	'sh',
	'bash',
	'zsh',
	'fish',
	'ps1',
	'sql',
	'graphql',
	'gql',
	'proto',
	'env',
	'lock',
	'log',
	'ini',
	'conf',
	'cfg',
	'png',
	'jpg',
	'jpeg',
	'gif',
	'webp',
	'avif',
	'pdf',
	'mp3',
	'mp4',
	'mov',
	'webm',
	'wav',
	'csv',
	'tsv',
	'xlsx',
	'xls',
	'ipynb',
	'pen',
]);

export function looksLikePath(s: string): boolean {
	if (!s) return false;
	const trimmed = s.trim();
	if (trimmed.length < 2 || trimmed.length > 256) return false;
	// Skip URLs, emails, and bare words.
	if (trimmed.includes('://')) return false;
	// Reject malformed double heads like `//foo` or `~~/foo`
	if (trimmed.startsWith('//') || trimmed.startsWith('~~')) return false;

	// Multi-segment path (directory or file in subdirectory)
	if (trimmed.includes('/')) {
		return MULTI_SEGMENT_PATH_RE.test(trimmed);
	}

	// Single-segment token: must have an extension and match a known extension
	if (!trimmed.includes('.')) return false;
	if (!SINGLE_SEGMENT_PATH_RE.test(trimmed)) return false;

	const ext = trimmed.split('.').pop()?.toLowerCase() ?? '';
	return KNOWN_EXTENSIONS.has(ext);
}

/** Best-effort synchronous resolution: expand `~`, join relative paths against
 *  `cwd`. Returns the input unchanged when it can't be resolved (absolute paths
 *  pass through; relative paths with no `cwd` pass through as-is). */
export function resolvePath(p: string, cwd?: string): string {
	let path = p.trim();
	const home = getHomeSync();
	if (path.startsWith('~/') && home) {
		path = path.replace(/^~\//, `${home}/`);
	} else if (path.startsWith('~') && home) {
		path = home + path.slice(1);
	} else if (!path.startsWith('/') && !/^[a-zA-Z]:[/\\]/.test(path) && cwd) {
		path = `${cwd.replace(/[/\\]$/, '')}/${path}`;
	}
	return path;
}
