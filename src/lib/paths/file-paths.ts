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
export const MULTI_SEGMENT_PATH_RE =
	/^(?:~?\/|~|\.\.?\/)?[a-zA-Z0-9_.@()-][a-zA-Z0-9_./@()-]*$/;

// Single-segment file pattern (no slash, requires .<ext>).
export const SINGLE_SEGMENT_PATH_RE =
	/^[a-zA-Z0-9_.@()-]+\.[A-Za-z0-9]{1,7}$/;

// Backward-compatible alias for existing consumers.
export const PATH_RE =
	/^(?:~?\/|~|\.\.?\/)?[a-zA-Z0-9_.@()-][a-zA-Z0-9_./@()-]*$/;

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

/** Checks whether parentheses in a path token are properly matched and balanced. */
export function hasBalancedParens(s: string): boolean {
	let depth = 0;
	for (let i = 0; i < s.length; i++) {
		if (s[i] === '(') depth++;
		else if (s[i] === ')') {
			depth--;
			if (depth < 0) return false;
		}
	}
	return depth === 0;
}

export function looksLikePath(s: string): boolean {
	if (!s) return false;
	const trimmed = s.trim();
	if (trimmed.length < 2 || trimmed.length > 256) return false;
	// Skip URLs, emails, and bare words.
	if (trimmed.includes('://')) return false;
	// Reject malformed double heads like `//foo` or `~~/foo`
	if (trimmed.startsWith('//') || trimmed.startsWith('~~')) return false;
	// Verify balanced parentheses
	if (!hasBalancedParens(trimmed)) return false;

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

/** Normalizes path segments without hitting the filesystem (resolving `.` and `..`). */
export function normalizePath(p: string): string {
	const isAbs = p.startsWith('/') || /^[a-zA-Z]:[/\\]/.test(p);
	const segments = p.split(/[/\\]+/);
	const stack: string[] = [];
	for (const seg of segments) {
		if (!seg || seg === '.') continue;
		if (seg === '..') {
			if (stack.length > 0 && stack[stack.length - 1] !== '..') {
				stack.pop();
			} else if (!isAbs) {
				stack.push('..');
			}
		} else {
			stack.push(seg);
		}
	}
	const joined = stack.join('/');
	if (isAbs) {
		if (/^[a-zA-Z]:/.test(p)) {
			const drive = p.match(/^[a-zA-Z]:/)?.[0] ?? '';
			return `${drive}/${joined.slice(drive.length).replace(/^\/+/, '')}`;
		}
		return `/${joined}`;
	}
	return joined || '.';
}

/**
 * Async fs-validated candidate resolver with ordered candidates (T-02).
 * Prioritizes:
 * 1. Direct synchronous resolvePath(rawPath, cwd)
 * 2. Normalized path resolution (resolving `.` and `..`)
 * 3. Path with trailing punctuation trimmed
 * 4. Leading `./` stripped
 */
export async function resolvePathCandidates(
	rawPath: string,
	cwd?: string,
	fsCheck?: (p: string) => Promise<boolean>
): Promise<string> {
	const initial = resolvePath(rawPath, cwd);
	if (!fsCheck) return initial;

	const candidates = new Set<string>();
	candidates.add(initial);

	const normalized = normalizePath(initial);
	candidates.add(normalized);

	const trimmedPunctuation = initial.replace(/[.,;:]+$/, '');
	if (trimmedPunctuation !== initial) {
		candidates.add(trimmedPunctuation);
		candidates.add(normalizePath(trimmedPunctuation));
	}

	if (rawPath.startsWith('./') && cwd) {
		const withoutDotSlash = resolvePath(rawPath.slice(2), cwd);
		candidates.add(withoutDotSlash);
		candidates.add(normalizePath(withoutDotSlash));
	}

	const candidateList = Array.from(candidates).slice(0, 10);
	const results = await Promise.all(candidateList.map((c) => fsCheck(c).catch(() => false)));
	const matchIdx = results.findIndex(Boolean);
	return matchIdx >= 0 ? candidateList[matchIdx] : initial;
}
