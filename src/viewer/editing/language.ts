// Path → `@ikenga/ui-lib` CodeEditor language.
//
// ui-lib's `Language` is a closed set of five ('html' | 'tsx' | 'css' | 'json'
// | 'markdown'). The shell must not import `@codemirror/*` itself — it has no
// direct dependency, and a second copy of `@codemirror/state` breaks the
// editor at runtime — so every other format (YAML, TOML, CSV, Python, Rust,
// shell, plain text, …) falls back to 'json'. lang-json adds highlighting and
// bracket matching but no keymap or completion source, which makes it the
// least surprising plain-text stand-in ('markdown' would continue list markup
// on Enter; 'css' would pop property completions). Real modes for those
// formats belong in ui-lib (a 'text' language plus @codemirror/language-data).

import type { CodeEditorProps } from '@ikenga/ui-lib';
import { extname } from '../lib/path';

export type EditorLanguage = CodeEditorProps['language'];

const EXT_LANGUAGE: Record<string, EditorLanguage> = {
	'.ts': 'tsx',
	'.tsx': 'tsx',
	'.js': 'tsx',
	'.jsx': 'tsx',
	'.mjs': 'tsx',
	'.cjs': 'tsx',
	'.mts': 'tsx',
	'.cts': 'tsx',
	'.json': 'json',
	'.jsonl': 'json',
	'.json5': 'json',
	'.html': 'html',
	'.htm': 'html',
	'.svg': 'html',
	'.xml': 'html',
	'.css': 'css',
	'.scss': 'css',
	'.less': 'css',
	'.md': 'markdown',
	'.mdx': 'markdown',
};

export function editorLanguageFor(path: string): EditorLanguage {
	return EXT_LANGUAGE[extname(path)] ?? 'json';
}
