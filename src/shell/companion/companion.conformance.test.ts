// C6 — ADR-021 / spec §5.4 conformance: the Companion renders STATE, never
// model prose. This is the "lint rule banning markdown/transcript renderers
// under src/shell/companion/**" the ADR asks for, run as a test so it gates
// CI with no new tooling. It scans every non-test source file in this
// directory (so a new file is covered the day it lands) and checks the
// ADR-021 conformance checklist items that are machine-checkable.

import { readdirSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, expectTypeOf, it } from 'vitest';
import type { ResolvedTarget } from './resolve-target';

const DIR = dirname(fileURLToPath(import.meta.url));

const SOURCES = readdirSync(DIR)
	.filter((f) => /\.(ts|tsx)$/.test(f) && !/\.test\.(ts|tsx)$/.test(f))
	.map((f) => ({ file: f, text: readFileSync(join(DIR, f), 'utf8') }));

/** Every module specifier a file imports (static, dynamic, re-export). */
function importsOf(text: string): string[] {
	const out: string[] = [];
	const re = /(?:import|export)\s[^'"]*?from\s*['"]([^'"]+)['"]|import\(\s*['"]([^'"]+)['"]\s*\)/g;
	for (const m of text.matchAll(re)) out.push(m[1] ?? m[2]);
	return out;
}

/** Comments are prose about the rule, not code that breaks it. */
function stripComments(text: string): string {
	return text.replace(/\/\*[\s\S]*?\*\//g, '').replace(/(^|[^:])\/\/.*$/gm, '$1');
}

// Markdown / transcript / chat renderers — third-party and in-repo.
const BANNED_IMPORTS: RegExp[] = [
	/^react-markdown/,
	/^remark/,
	/^rehype/,
	/^streamdown/,
	/^marked/,
	/^markdown-it/,
	/^@mdx-js\//,
	/^@lobehub\/ui/,
	/^@\/components\/markdown$/, // in-repo markdown renderer
	/^@\/viewer\/renderers\/markdown/, // in-repo markdown formatter
	/^@\/terminal\/transcript-replay$/, // replayed transcripts open as pane tabs, never here
	/^@\/chat\//, // the retired chat surface (ADR-019)
];

describe('ADR-021 conformance — src/shell/companion/**', () => {
	it('scans a non-empty set of Companion sources', () => {
		const names = SOURCES.map((s) => s.file);
		expect(names).toEqual(
			expect.arrayContaining([
				'companion.tsx',
				'companion-store.ts',
				'dispatch-bar.tsx',
				'target-picker.tsx',
				'session-tabs.tsx',
				'collapsed-strip.tsx',
				'resolve-target.ts',
			])
		);
	});

	it('imports no markdown, transcript or chat renderer', () => {
		const offenders = SOURCES.flatMap(({ file, text }) =>
			importsOf(text)
				.filter((spec) => BANNED_IMPORTS.some((re) => re.test(spec)))
				.map((spec) => `${file} imports ${spec}`)
		);
		expect(offenders).toEqual([]);
	});

	it('renders no assistant messages, turns or chat history', () => {
		const patterns: Array<[string, RegExp]> = [
			["role === 'assistant'", /role\s*===?\s*['"]assistant['"]/],
			['message.role', /\bmessages?\.role\b/],
			[
				'.map over messages / turns / history',
				/\b(messages|turns|history|transcript)\s*\.\s*map\s*\(/i,
			],
			['dangerouslySetInnerHTML', /dangerouslySetInnerHTML/],
			['a Markdown component', /<\s*(Markdown|ReactMarkdown|Streamdown|Transcript\w*)\b/],
		];
		const offenders = SOURCES.flatMap(({ file, text }) => {
			const code = stripComments(text);
			return patterns.filter(([, re]) => re.test(code)).map(([what]) => `${file}: ${what}`);
		});
		expect(offenders).toEqual([]);
	});

	it('the dispatch bar is the only text input in the Companion', () => {
		const inputs = SOURCES.flatMap(({ file, text }) => {
			const code = stripComments(text);
			const n =
				(code.match(/<input\b/g)?.length ?? 0) +
				(code.match(/<textarea\b/g)?.length ?? 0) +
				(code.match(/contentEditable/g)?.length ?? 0);
			return n ? [`${file}×${n}`] : [];
		});
		expect(inputs).toEqual(['dispatch-bar.tsx×1']);
	});

	// Mounted children live outside this directory, so the scan above cannot
	// see their inputs. Pin how the Companion mounts them (C12); the rendered
	// check — exactly one textbox in the live DOM — is in companion.test.tsx.
	it('mounts MissionControl only in its `embedded` (input-less) form', () => {
		const mounts = SOURCES.flatMap(({ file, text }) =>
			// Raw text, not stripComments(): a `/*` inside a string (a glob, a
			// path) would make the block-comment strip swallow real JSX.
			[...text.matchAll(/<MissionControl\b[^>]*>/g)].map((m) => `${file}: ${m[0]}`)
		);
		expect(mounts.length).toBeGreaterThan(0);
		expect(mounts.filter((m) => !/\bembedded\b/.test(m))).toEqual([]);
	});

	it('does not mount the scoped PermissionInbox HUD (§5.6 cards only)', () => {
		const offenders = SOURCES.flatMap(({ file, text }) =>
			importsOf(text).includes('@/terminal/permission-inbox') || /<PermissionInbox\b/.test(text)
				? [file]
				: []
		);
		expect(offenders).toEqual([]);
	});

	it('holds no response / streamed-text state', () => {
		const offenders = SOURCES.flatMap(({ file, text }) => {
			const code = stripComments(text);
			return /\b(set)?(response|reply|answer|completion|streamedText|assistantText)\s*[:=(]/i.test(
				code
			)
				? [file]
				: [];
		});
		expect(offenders).toEqual([]);
	});

	it('send() is fire-and-forget: it resolves to void, never to a response', () => {
		expectTypeOf<ReturnType<ResolvedTarget['send']>>().toEqualTypeOf<Promise<void>>();
	});

	it('the checks bite: a file that renders markdown prose is caught', () => {
		const bad = `import ReactMarkdown from 'react-markdown';
			export const X = ({ messages }) => messages.map((m) => m.role === 'assistant' && <ReactMarkdown>{m.text}</ReactMarkdown>);`;
		expect(importsOf(bad).some((s) => BANNED_IMPORTS.some((re) => re.test(s)))).toBe(true);
		expect(/\b(messages|turns|history|transcript)\s*\.\s*map\s*\(/i.test(bad)).toBe(true);
		expect(/<\s*(Markdown|ReactMarkdown|Streamdown|Transcript\w*)\b/.test(bad)).toBe(true);
	});
});
