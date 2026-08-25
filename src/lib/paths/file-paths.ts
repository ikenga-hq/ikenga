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
export const MULTI_SEGMENT_PATH_RE = /^(?:~?\/|~|\.\.?\/)?[a-zA-Z0-9_.@()-][a-zA-Z0-9_./@()-]*$/;

// Single-segment file pattern (no slash, requires .<ext>).
export const SINGLE_SEGMENT_PATH_RE = /^[a-zA-Z0-9_.@()-]+\.[A-Za-z0-9]{1,7}$/;

// Backward-compatible alias for existing consumers.
export const PATH_RE = /^(?:~?\/|~|\.\.?\/)?[a-zA-Z0-9_.@()-][a-zA-Z0-9_./@()-]*$/;

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

/** Build the ordered candidate list for a raw path token. Pure — no fs access.
 *  Order matters: the first candidate that exists on disk wins. */
function pathCandidates(rawPath: string, cwd?: string): string[] {
	const initial = resolvePath(rawPath, cwd);
	const candidates = new Set<string>();
	candidates.add(initial);
	candidates.add(normalizePath(initial));

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

	return Array.from(candidates).slice(0, 10);
}

/**
 * Async fs-validated resolver (T-02). Returns the first candidate that exists
 * on disk, or `null` when none do.
 *
 * `looksLikePath` is deliberately permissive for multi-segment tokens so bare
 * relative directories (`src/terminal`) linkify. That same permissiveness makes
 * prose like `24/7`, `and/or` and `n/a` path-shaped — they are lexically
 * indistinguishable from `src/terminal`. Existence on disk is the only honest
 * discriminator, so callers that decorate UI should gate on this returning
 * non-null rather than on `looksLikePath` alone.
 */
export async function resolveExistingPath(
	rawPath: string,
	cwd?: string,
	fsCheck?: (p: string) => Promise<boolean>
): Promise<string | null> {
	if (!fsCheck) return null;
	const candidateList = pathCandidates(rawPath, cwd);
	const results = await Promise.all(candidateList.map((c) => fsCheck(c).catch(() => false)));
	const matchIdx = results.findIndex(Boolean);
	return matchIdx >= 0 ? candidateList[matchIdx] : null;
}

/**
 * Async fs-validated candidate resolver with ordered candidates (T-02).
 * Returns the first candidate that exists, falling back to the plain
 * synchronous resolution when nothing does.
 */
export async function resolvePathCandidates(
	rawPath: string,
	cwd?: string,
	fsCheck?: (p: string) => Promise<boolean>
): Promise<string> {
	const initial = resolvePath(rawPath, cwd);
	if (!fsCheck) return initial;
	return (await resolveExistingPath(rawPath, cwd, fsCheck)) ?? initial;
}
/**
 * Convert a `file://` URL to a plain filesystem path. Returns non-`file://`
 * input unchanged, so callers can pass a raw path through safely.
 *
 * Handles the two shapes terminals actually emit:
 *   file://hostname/home/u/proj  → /home/u/proj   (authority stripped)
 *   file:///C:/Users/u/proj      → C:/Users/u/proj (leading slash before the
 *                                                   drive letter dropped)
 *
 * That second case is why this exists: without it a Windows shell's OSC 7 sets
 * cwd to `/C:/Users/...`, every relative path then resolves against a location
 * that cannot exist, and terminal path links stop resolving entirely.
 */
export function fileUrlToPath(raw: string): string {
	const trimmed = raw.trim();
	if (!trimmed.startsWith('file://')) return trimmed;

	let path = trimmed.slice('file://'.length);

	// Strip an optional authority: `file://host/p` → `/p`. For `file:///p` the
	// remainder already starts with `/`, so this is a no-op.
	const slashIdx = path.indexOf('/');
	path = slashIdx === -1 ? '' : path.slice(slashIdx);

	// Windows drive letters arrive as `/C:/…`; the leading slash is not part of
	// the path.
	if (path.startsWith('/') && /^[a-zA-Z]:/.test(path.slice(1))) path = path.slice(1);

	try {
		return decodeURIComponent(path);
	} catch {
		// Malformed percent-encoding — better to use the raw path than nothing.
		return path;
	}
}
