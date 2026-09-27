import { readdirSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import {
	astEquals,
	contextKeysOf,
	evaluateAst,
	evaluateWhen,
	globMatch,
	globMatchPath,
	normalizeAst,
	normalizeWhen,
	parseWhen,
	serializeWhen,
	specificity,
	tryParseWhen,
	WHEN_MAX_DEPTH,
	WHEN_MAX_LENGTH,
	type WhenNode,
	WhenSyntaxError,
	whenSpecificity,
} from './when';

const norm = (s: string) => normalizeWhen(s);

// One input per G-ACTIONS §4.1 production (and the §4.4 legacy words), plus
// the §7.3 derived package-key `when`s.
const PRODUCTIONS: string[] = [
	'', // empty ≡ always
	'global',
	'not-input',
	'terminal-focus',
	'always',
	'true',
	'false',
	'inputFocus', // context-key
	'paneKind == \'artifact\'', // comparison ==, single-quoted string
	'paneKind != "terminal"', // comparison !=, double-quoted string
	'inputFocus == true', // value true
	'inputFocus != false', // value false
	"resource =~ '*.{ts,rs}'", // =~ glob
	'!inputFocus', // unary
	'!!inputFocus',
	'!paneKind == \'terminal\'', // ! binds to the whole comparison
	'filesFocus && paneFocus', // and-expr
	'filesFocus || paneFocus', // or-expr
	'(filesFocus || paneFocus) && inputFocus', // parenthesized primary
	'filesFocus || paneFocus && inputFocus',
	'!(filesFocus && paneFocus)',
	"resource == 'it\\'s' && resource != \"a\\\"b\\\\c\"", // escapes
	'  paneFocus   &&\tfilesFocus ', // whitespace insignificant
	'a1 && bB2', // camelCase keys with digits
	// §7.3 derived package `when`s (G-71)
	'filesFocus',
	"filesFocus && resource =~ '*.ts'",
	"paneKind == 'artifact'",
	'sessionFocus',
	'ngwaItemFocus',
	"ngwaItemFocus && (ngwaItemKind == 'k1' || ngwaItemKind == 'k2' || ngwaItemKind == 'k3')",
	// §4.6 / §10.2 frozen entries
	'!inputFocus && !paletteOpen',
	'paletteOpen',
	'dispatchFocus',
	'terminalFocus',
];

describe('parseWhen — every §4.1 production parses', () => {
	it.each(PRODUCTIONS)('parses %j', (src) => {
		expect(() => parseWhen(src)).not.toThrow();
	});

	it('treats empty / whitespace / undefined / null as TRUE (always)', () => {
		for (const s of ['', '   ', undefined, null]) expect(parseWhen(s)).toEqual({ type: 'true' });
	});

	it('parses `always` and `true` to TRUE, `false` to FALSE', () => {
		expect(parseWhen('always')).toEqual({ type: 'true' });
		expect(parseWhen('true')).toEqual({ type: 'true' });
		expect(parseWhen('false')).toEqual({ type: 'false' });
	});

	it('parses comparisons with string and boolean values', () => {
		expect(parseWhen("paneKind == 'artifact'")).toEqual({ type: 'eq', key: 'paneKind', value: 'artifact' });
		expect(parseWhen('paneKind != "x"')).toEqual({ type: 'ne', key: 'paneKind', value: 'x' });
		expect(parseWhen('inputFocus == true')).toEqual({ type: 'eq', key: 'inputFocus', value: true });
		expect(parseWhen("resource =~ '*.ts'")).toEqual({ type: 'glob', key: 'resource', pattern: '*.ts' });
	});

	it('`!` applies to the whole following primary, comparisons included', () => {
		expect(parseWhen("!paneKind == 'terminal'")).toEqual({
			type: 'not',
			operand: { type: 'eq', key: 'paneKind', value: 'terminal' },
		});
	});

	it('binds && tighter than ||, and parentheses override', () => {
		expect(parseWhen('a || b && c')).toEqual({
			type: 'or',
			operands: [{ type: 'key', key: 'a' }, { type: 'and', operands: [{ type: 'key', key: 'b' }, { type: 'key', key: 'c' }] }],
		});
		expect(parseWhen('(a || b) && c')).toEqual({
			type: 'and',
			operands: [{ type: 'or', operands: [{ type: 'key', key: 'a' }, { type: 'key', key: 'b' }] }, { type: 'key', key: 'c' }],
		});
	});

	it('unescapes \\\' \\" and \\\\ inside strings', () => {
		expect(parseWhen("resource == 'it\\'s'")).toEqual({ type: 'eq', key: 'resource', value: "it's" });
		expect(parseWhen('resource == "a\\"b\\\\c"')).toEqual({ type: 'eq', key: 'resource', value: 'a"b\\c' });
	});
});

describe('parseWhen — rejections (E_WHEN_SYNTAX)', () => {
	const BAD = [
		'global && paneFocus', // legacy word inside a larger expression
		'paneFocus || not-input',
		'terminal-focus && a',
		'!global',
		'InputFocus', // keys start lowercase
		'a &&',
		'&& a',
		'(a',
		'a)',
		"a == b", // unquoted value
		'a == ',
		'a =~ b', // glob must be quoted
		'a =~ true',
		"'x' == a", // lhs must be a key
		'true == a',
		"always == 'x'",
		"a == 'unterminated",
		'a @ b',
		'a & b',
		'a | b',
		'a = b',
		'a b',
		'()',
	];
	it.each(BAD)('rejects %j', (src) => {
		expect(() => parseWhen(src)).toThrow(WhenSyntaxError);
		const r = tryParseWhen(src);
		expect(r.ok).toBe(false);
		if (!r.ok) expect(r.error.code).toBe('E_WHEN_SYNTAX');
	});

	it(`rejects more than ${WHEN_MAX_LENGTH} characters`, () => {
		const long = Array.from({ length: 100 }, (_, i) => `k${i}`).join(' && ');
		expect(long.length).toBeGreaterThan(WHEN_MAX_LENGTH);
		expect(() => parseWhen(long)).toThrow(WhenSyntaxError);
	});

	it(`rejects nesting deeper than ${WHEN_MAX_DEPTH}`, () => {
		const ok = `${'('.repeat(WHEN_MAX_DEPTH)}a${')'.repeat(WHEN_MAX_DEPTH)}`;
		expect(() => parseWhen(ok)).not.toThrow();
		const deep = `${'('.repeat(WHEN_MAX_DEPTH + 1)}a${')'.repeat(WHEN_MAX_DEPTH + 1)}`;
		expect(() => parseWhen(deep)).toThrow(WhenSyntaxError);
		expect(() => parseWhen(`${'!'.repeat(WHEN_MAX_DEPTH + 1)}a`)).toThrow(WhenSyntaxError);
	});
});

describe('legacy aliases (§4.4)', () => {
	it('normalize to their DEC-62 forms', () => {
		expect(norm('global')).toBe('');
		expect(norm('not-input')).toBe('!inputFocus');
		expect(norm('terminal-focus')).toBe('terminalFocus');
		expect(norm('  not-input  ')).toBe('!inputFocus');
	});

	it('evaluate exactly as the pre-v2 clauses did', () => {
		// `global` — true everywhere, including inside inputs.
		expect(evaluateWhen('global', { inputFocus: true })).toBe(true);
		expect(evaluateWhen('global', { inputFocus: false })).toBe(true);
		// `not-input` — `!isTypingTarget(...)`, i.e. `!inputFocus`.
		expect(evaluateWhen('not-input', { inputFocus: true })).toBe(false);
		expect(evaluateWhen('not-input', { inputFocus: false })).toBe(true);
		// `terminal-focus` — `terminalFocus`; the dispatcher never fires its
		// (hosted) commands, which is what kept it false there (registry test).
		expect(evaluateWhen('terminal-focus', { terminalFocus: false })).toBe(false);
		expect(evaluateWhen('terminal-focus', { terminalFocus: true })).toBe(true);
	});
});

describe('normalization (§4.5)', () => {
	it.each([
		['always', ''],
		['true', ''],
		['false', 'false'],
		['!!a', 'a'],
		['!!!a', '!a'],
		["!(k == 'v')", "k != 'v'"],
		["!(k != 'v')", "k == 'v'"],
		["!k == 'v'", "k != 'v'"],
		['!true', 'false'],
		['!false', ''],
		['!always', 'false'],
		['a && (b && c)', 'a && b && c'],
		['(a || b) || c', 'a || b || c'],
		['a && always', 'a'],
		['a && true && b', 'a && b'],
		['a || false', 'a'],
		['a && false', 'false'],
		['a || true', ''],
		['a || always', ''],
		['a && a', 'a'],
		['a && b && a', 'a && b'],
		['b && a', 'a && b'],
		['c || b || a', 'a || b || c'],
		['c && (b || a)', '(a || b) && c'],
		['a || b && c', 'a || b && c'],
		['c && b || a', 'a || b && c'],
		['!(b && a)', '!(a && b)'],
		['!(b || a)', '!(a || b)'],
		["resource =~ '*.ts' && filesFocus", "filesFocus && resource =~ '*.ts'"],
		['paneKind == "artifact"', "paneKind == 'artifact'"],
		["resource == 'it\\'s'", "resource == 'it\\'s'"],
		['resource == "a\\\\b"', "resource == 'a\\\\b'"],
		['inputFocus == true', 'inputFocus == true'],
		['!(inputFocus == true)', 'inputFocus != true'],
		['!(a && b) && !(b && a)', '!(a && b)'],
		['  a   &&   b ', 'a && b'],
	])('%j → %j', (input, expected) => {
		expect(norm(input)).toBe(expected);
	});

	it('is syntactic: no De Morgan, no distribution', () => {
		expect(norm('a && (b || c)')).not.toBe(norm('(a && b) || (a && c)'));
		expect(norm('!(a && b)')).not.toBe(norm('!a || !b'));
	});

	it('gives differently-typed but equivalent strings the same normal form', () => {
		expect(norm('!inputFocus && !paletteOpen')).toBe(norm('(!paletteOpen) && !inputFocus'));
		expect(norm('!inputFocus')).toBe(norm('not-input'));
		expect(norm("filesFocus && resource =~ '*.ts'")).toBe(norm("resource =~ \"*.ts\" && (filesFocus && always)"));
	});
});

describe('round-trip (§4.5, WP-49 DoD)', () => {
	it.each(PRODUCTIONS)('serialize(parse(%j)) re-parses to the same AST and is idempotent', (src) => {
		const n: WhenNode = normalizeAst(parseWhen(src));
		const once = serializeWhen(n);
		// serialize(normalized) parses back to exactly that AST …
		expect(astEquals(parseWhen(once), n)).toBe(true);
		// … which is already normal …
		expect(astEquals(normalizeAst(parseWhen(once)), n)).toBe(true);
		// … and serialization is idempotent.
		expect(serializeWhen(normalizeAst(parseWhen(once)))).toBe(once);
		expect(normalizeWhen(once)).toBe(once);
	});

	it('serializes TRUE as the empty `when` and FALSE as `false`', () => {
		expect(serializeWhen({ type: 'true' })).toBe('');
		expect(serializeWhen({ type: 'false' })).toBe('false');
	});
});

describe('evaluation (§4.5)', () => {
	it('reads an unknown key as undefined', () => {
		expect(evaluateWhen('nope', {})).toBe(false);
		expect(evaluateWhen('!nope', {})).toBe(true);
	});

	it('treats a bare key as truthy when true or a non-empty string', () => {
		expect(evaluateWhen('k', { k: true })).toBe(true);
		expect(evaluateWhen('k', { k: false })).toBe(false);
		expect(evaluateWhen('k', { k: 'x' })).toBe(true);
		expect(evaluateWhen('k', { k: '' })).toBe(false);
	});

	it('compares string forms; undefined equals nothing', () => {
		expect(evaluateWhen("paneKind == 'artifact'", { paneKind: 'artifact' })).toBe(true);
		expect(evaluateWhen("paneKind == 'artifact'", { paneKind: 'artifact-studio' })).toBe(false);
		expect(evaluateWhen('inputFocus == true', { inputFocus: true })).toBe(true);
		expect(evaluateWhen("inputFocus == 'true'", { inputFocus: true })).toBe(true);
		expect(evaluateWhen('inputFocus == false', { inputFocus: false })).toBe(true);
		expect(evaluateWhen("paneKind == 'x'", {})).toBe(false);
		expect(evaluateWhen("paneKind != 'x'", {})).toBe(true);
		// `!(k == v)` and `k != v` agree on undefined, which is why the
		// normalizer may rewrite one into the other.
		expect(evaluateWhen("!(paneKind == 'x')", {})).toBe(evaluateWhen("paneKind != 'x'", {}));
	});

	it('`=~` is false for an undefined key', () => {
		expect(evaluateWhen("resource =~ '*'", {})).toBe(false);
	});

	it('evaluates &&, || and ! with the §4.1 precedence', () => {
		const ctx = { a: true, b: false, c: true };
		expect(evaluateWhen('a && b || c', ctx)).toBe(true);
		expect(evaluateWhen('a && (b || c)', ctx)).toBe(true);
		expect(evaluateWhen('!a || b', ctx)).toBe(false);
		expect(evaluateWhen('!(a && b)', ctx)).toBe(true);
	});

	it('never fires an unparsable `when`', () => {
		expect(evaluateWhen('global && a', { a: true })).toBe(false);
		expect(evaluateWhen('a ==', { a: true })).toBe(false);
	});

	it('evaluates the §7.3 derived package whens', () => {
		const w = "ngwaItemFocus && (ngwaItemKind == 'mcp' || ngwaItemKind == 'skill')";
		expect(evaluateWhen(w, { ngwaItemFocus: true, ngwaItemKind: 'skill' })).toBe(true);
		expect(evaluateWhen(w, { ngwaItemFocus: true, ngwaItemKind: 'engine' })).toBe(false);
		expect(evaluateWhen(w, { ngwaItemFocus: false, ngwaItemKind: 'skill' })).toBe(false);
		const f = "filesFocus && resource =~ '*.{ts,rs}'";
		expect(evaluateWhen(f, { filesFocus: true, resource: '/p/src/lib.rs' })).toBe(true);
		expect(evaluateWhen(f, { filesFocus: true, resource: '/p/src/lib.py' })).toBe(false);
	});

	it('evaluateAst walks a normalized tree', () => {
		expect(evaluateAst(normalizeAst(parseWhen('a && !b')), { a: true, b: false })).toBe(true);
	});
});

describe('glob dialect (§4.2)', () => {
	it('`*` does not cross `/`, `**` does', () => {
		expect(globMatch('src/*.ts', 'src/a.ts')).toBe(true);
		expect(globMatch('src/*.ts', 'src/x/a.ts')).toBe(false);
		expect(globMatch('src/**', 'src/x/a.ts')).toBe(true);
		expect(globMatch('src/**/*.ts', 'src/a.ts')).toBe(true);
		expect(globMatch('src/**/*.ts', 'src/x/y/a.ts')).toBe(true);
		expect(globMatch('src/**/*.ts', 'lib/a.ts')).toBe(false);
	});

	it('supports `?`, classes and braces (nested)', () => {
		expect(globMatch('a?.ts', 'ab.ts')).toBe(true);
		expect(globMatch('a?.ts', 'a/.ts')).toBe(false);
		expect(globMatch('[a-c]x', 'bx')).toBe(true);
		expect(globMatch('[a-c]x', 'dx')).toBe(false);
		expect(globMatch('*.{ts,rs}', 'a.rs')).toBe(true);
		expect(globMatch('*.{ts,{md,mdx}}', 'a.mdx')).toBe(true);
		expect(globMatch('*.{ts,rs}', 'a.py')).toBe(false);
	});

	it('is case-sensitive and treats regex metacharacters literally', () => {
		expect(globMatch('*.ts', 'a.TS')).toBe(false);
		expect(globMatch('a+(b).ts', 'a+(b).ts')).toBe(true);
		expect(globMatch('a.ts', 'abts')).toBe(false);
		expect(globMatch('\\*.ts', '*.ts')).toBe(true);
		expect(globMatch('\\*.ts', 'a.ts')).toBe(false);
	});

	it('matches the basename for a glob without `/`, the project-relative path with one', () => {
		expect(globMatchPath('*.ts', '/home/u/p/src/c.ts')).toBe(true);
		expect(globMatchPath('src/**/*.rs', '/home/u/p/src/a/b.rs', '/home/u/p')).toBe(true);
		expect(globMatchPath('src/**/*.rs', '/home/u/p/src/a/b.rs', '/home/u/p/')).toBe(true);
		expect(globMatchPath('src/**/*.rs', '/home/u/other/src/b.rs', '/home/u/p')).toBe(false);
		expect(globMatchPath('*.ts', 'C:\\p\\src\\c.ts')).toBe(true);
		expect(globMatchPath('src/*.ts', 'C:\\p\\src\\c.ts', 'C:\\p')).toBe(true);
	});

	it('stays linear on pathological patterns', () => {
		const start = Date.now();
		expect(globMatch('*a*a*a*a*a*a*a*a*a*b', 'a'.repeat(200))).toBe(false);
		expect(Date.now() - start).toBeLessThan(1000);
	});
});

describe('specificity (§2.3, B-1)', () => {
	it.each([
		['', 0],
		['always', 0],
		['!inputFocus', 1],
		["filesFocus && resource =~ '*.ts'", 2],
		['a || b', 1],
		['a || b && c', 1],
		['(a || b) && c', 2],
		['!(a && b)', 1],
		["ngwaItemFocus && (ngwaItemKind == 'k1')", 2],
		["ngwaItemFocus && (ngwaItemKind == 'k1' || ngwaItemKind == 'k2' || ngwaItemKind == 'k3')", 2],
		['!inputFocus && !paletteOpen', 2],
	])('%j scores %i', (w, score) => {
		expect(whenSpecificity(w)).toBe(score);
		expect(specificity(normalizeAst(parseWhen(w)))).toBe(score);
	});

	it('never ranks a disjunction above a narrower `when`', () => {
		expect(whenSpecificity('a || b')).toBeLessThanOrEqual(whenSpecificity('a'));
	});
});

describe('contextKeysOf', () => {
	it('lists every key an expression names', () => {
		expect(
			contextKeysOf(parseWhen("ngwaItemFocus && (ngwaItemKind == 'a' || !paneFocus) && resource =~ '*'")).sort()
		).toEqual(['ngwaItemFocus', 'ngwaItemKind', 'paneFocus', 'resource']);
	});
});

describe('no eval / Function / user-text RegExp in lib/keymap (DEC-62)', () => {
	it('holds for every non-test source file', () => {
		const dir = dirname(fileURLToPath(import.meta.url));
		const files = readdirSync(dir).filter((f) => f.endsWith('.ts') && !f.includes('.test.'));
		expect(files).toContain('when.ts');
		for (const f of files) {
			const src = readFileSync(join(dir, f), 'utf8');
			expect(src, f).not.toMatch(/\beval\s*\(/);
			expect(src, f).not.toMatch(/\bnew\s+Function\b/);
			expect(src, f).not.toMatch(/(^|[^.\w])Function\s*\(/);
			expect(src, f).not.toMatch(/\bnew\s+RegExp\b/);
			expect(src, f).not.toMatch(/(^|[^.\w])RegExp\s*\(/);
		}
	});
});
