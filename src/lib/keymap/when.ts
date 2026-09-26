// The `when` language (G-ACTIONS §4, DEC-62) — a VS Code subset: context keys,
// `!`, `&&`, `||`, `==`, `!=`, `=~` (glob) and parentheses.
//
// A hand-written tokenizer and recursive-descent parser produce an AST;
// evaluation is a whitelist walk over that AST. Nothing in this file (or
// anywhere in `lib/keymap`) uses `eval`, `Function`, or a `RegExp` built from
// user text — the `=~` glob is matched by the hand-written matcher at the
// bottom of this file.
//
// Three consumers read this module:
// - the dispatcher (WP-54) evaluates a rule's `when` against the live context
//   (`context-keys.ts`, the only producer of context values);
// - `conflicts()` (`registry.ts`, DEC-59) compares **normalized** forms, never
//   the string the user typed;
// - the effective-keymap resolution (WP-52/WP-54, §2.3) ranks precedence by
//   `specificity()`.
//
// The pre-DEC-62 closed union (`'global' | 'not-input' | 'terminal-focus'`)
// is still accepted — as a whole expression only (§4.4) — and normalizes to
// `always` / `!inputFocus` / `terminalFocus`.

// ─── Public types ─────────────────────────────────────────────────────────

/** A `when` expression in DEC-62 grammar (§4.1). Empty / absent ≡ `always`.
 *  Kept as a named alias so call sites that used the old closed union still
 *  read naturally; any string is accepted and validated by `parseWhen`. */
export type WhenClause = string;

/** The three pre-DEC-62 clause words (`when.ts:11` on 5b). Accepted by the
 *  parser as a whole expression only; writers never emit them (§4.4). */
export type LegacyWhenClause = 'global' | 'not-input' | 'terminal-focus';

export const LEGACY_WHEN_ALIASES: Readonly<Record<LegacyWhenClause, string>> = {
	global: 'always',
	'not-input': '!inputFocus',
	'terminal-focus': 'terminalFocus',
};

/** A context value (§4.3): booleans for focus keys, strings for `paneKind`,
 *  `resource`, … ; `undefined` when a key has no value (or is unknown). */
export type ContextValue = boolean | string | undefined;

/** The snapshot a `when` evaluates against. Unknown keys read `undefined`. */
export type WhenContext = Readonly<Record<string, ContextValue>>;

export type WhenNode =
	| { type: 'true' }
	| { type: 'false' }
	| { type: 'key'; key: string }
	| { type: 'eq'; key: string; value: string | boolean }
	| { type: 'ne'; key: string; value: string | boolean }
	| { type: 'glob'; key: string; pattern: string }
	| { type: 'not'; operand: WhenNode }
	| { type: 'and'; operands: WhenNode[] }
	| { type: 'or'; operands: WhenNode[] };

export const WHEN_MAX_LENGTH = 512;
export const WHEN_MAX_DEPTH = 32;

/** Thrown by `parseWhen` — the FE mirror of the validator's `E_WHEN_SYNTAX`. */
export class WhenSyntaxError extends Error {
	readonly code = 'E_WHEN_SYNTAX';
	constructor(
		message: string,
		readonly source: string,
		readonly offset: number
	) {
		super(`${message} (at ${offset} in ${JSON.stringify(source)})`);
		this.name = 'WhenSyntaxError';
	}
}

// ─── Tokenizer ────────────────────────────────────────────────────────────

type Token =
	| { t: '(' | ')' | '!' | '&&' | '||' | '==' | '!=' | '=~'; at: number }
	| { t: 'ident'; v: string; at: number }
	| { t: 'string'; v: string; at: number };

const RESERVED = new Set(['always', 'true', 'false', 'global']);

function isLower(c: string): boolean {
	return c >= 'a' && c <= 'z';
}
function isIdentChar(c: string): boolean {
	return isLower(c) || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9');
}
function isSpace(c: string): boolean {
	return c === ' ' || c === '\t' || c === '\n' || c === '\r';
}

function tokenize(src: string): Token[] {
	const out: Token[] = [];
	let i = 0;
	while (i < src.length) {
		const c = src[i];
		if (isSpace(c)) {
			i++;
			continue;
		}
		const two = src.slice(i, i + 2);
		if (two === '&&' || two === '||' || two === '==' || two === '!=' || two === '=~') {
			out.push({ t: two, at: i });
			i += 2;
			continue;
		}
		if (c === '(' || c === ')' || c === '!') {
			out.push({ t: c, at: i });
			i++;
			continue;
		}
		if (c === "'" || c === '"') {
			const quote = c;
			const start = i;
			i++;
			let v = '';
			let closed = false;
			while (i < src.length) {
				const d = src[i];
				if (d === '\\' && i + 1 < src.length && (src[i + 1] === quote || src[i + 1] === '\\')) {
					v += src[i + 1];
					i += 2;
					continue;
				}
				if (d === quote) {
					closed = true;
					i++;
					break;
				}
				v += d;
				i++;
			}
			if (!closed) throw new WhenSyntaxError('unterminated string', src, start);
			out.push({ t: 'string', v, at: start });
			continue;
		}
		if (isLower(c)) {
			const start = i;
			while (i < src.length && isIdentChar(src[i])) i++;
			out.push({ t: 'ident', v: src.slice(start, i), at: start });
			continue;
		}
		throw new WhenSyntaxError(`unexpected character ${JSON.stringify(c)}`, src, i);
	}
	return out;
}

// ─── Parser ───────────────────────────────────────────────────────────────

class Parser {
	private pos = 0;
	private depth = 0;
	constructor(
		private readonly src: string,
		private readonly toks: Token[]
	) {}

	parse(): WhenNode {
		const node = this.orExpr();
		const next = this.toks[this.pos];
		if (next) throw new WhenSyntaxError(`unexpected token ${JSON.stringify(next.t)}`, this.src, next.at);
		return node;
	}

	private peek(): Token | undefined {
		return this.toks[this.pos];
	}

	private fail(message: string): never {
		const tok = this.peek();
		throw new WhenSyntaxError(message, this.src, tok ? tok.at : this.src.length);
	}

	private enter() {
		if (++this.depth > WHEN_MAX_DEPTH) this.fail(`nesting deeper than ${WHEN_MAX_DEPTH}`);
	}

	private orExpr(): WhenNode {
		const operands = [this.andExpr()];
		while (this.peek()?.t === '||') {
			this.pos++;
			operands.push(this.andExpr());
		}
		return operands.length === 1 ? operands[0] : { type: 'or', operands };
	}

	private andExpr(): WhenNode {
		const operands = [this.unary()];
		while (this.peek()?.t === '&&') {
			this.pos++;
			operands.push(this.unary());
		}
		return operands.length === 1 ? operands[0] : { type: 'and', operands };
	}

	private unary(): WhenNode {
		if (this.peek()?.t === '!') {
			this.pos++;
			this.enter();
			const operand = this.unary();
			this.depth--;
			return { type: 'not', operand };
		}
		return this.primary();
	}

	private primary(): WhenNode {
		const tok = this.peek();
		if (!tok) this.fail('unexpected end of expression');
		if (tok.t === '(') {
			this.pos++;
			this.enter();
			const inner = this.orExpr();
			this.depth--;
			if (this.peek()?.t !== ')') this.fail('expected ")"');
			this.pos++;
			return inner;
		}
		if (tok.t !== 'ident') this.fail(`unexpected token ${JSON.stringify(tok.t)}`);
		this.pos++;
		const word = tok.v;
		if (word === 'always' || word === 'true') return { type: 'true' };
		if (word === 'false') return { type: 'false' };
		if (RESERVED.has(word)) {
			// `global` inside a larger expression (§4.1: legacy words are whole-input only).
			throw new WhenSyntaxError(`"${word}" is reserved`, this.src, tok.at);
		}
		const op = this.peek();
		if (op && (op.t === '==' || op.t === '!=')) {
			this.pos++;
			const rhs = this.peek();
			if (!rhs) this.fail('expected a value');
			this.pos++;
			let value: string | boolean;
			if (rhs.t === 'string') value = rhs.v;
			else if (rhs.t === 'ident' && (rhs.v === 'true' || rhs.v === 'false')) value = rhs.v === 'true';
			else throw new WhenSyntaxError('expected a quoted string, true or false', this.src, rhs.at);
			return { type: op.t === '==' ? 'eq' : 'ne', key: word, value };
		}
		if (op && op.t === '=~') {
			this.pos++;
			const rhs = this.peek();
			if (!rhs || rhs.t !== 'string') this.fail('expected a quoted glob after =~');
			this.pos++;
			return { type: 'glob', key: word, pattern: rhs.v };
		}
		return { type: 'key', key: word };
	}
}

function isLegacyWord(s: string): s is LegacyWhenClause {
	return s === 'global' || s === 'not-input' || s === 'terminal-focus';
}

/**
 * Parse a `when` string into its AST (not normalized). Empty / whitespace /
 * `undefined` parse to TRUE (`always`). A legacy word is accepted only as the
 * whole trimmed input and is replaced by its DEC-62 form (§4.4). Throws
 * `WhenSyntaxError` on any grammar violation, on more than 512 characters,
 * or on nesting deeper than 32.
 */
export function parseWhen(input: string | undefined | null): WhenNode {
	const src = input ?? '';
	if (src.length > WHEN_MAX_LENGTH) {
		throw new WhenSyntaxError(`longer than ${WHEN_MAX_LENGTH} characters`, src, WHEN_MAX_LENGTH);
	}
	const trimmed = src.trim();
	if (trimmed === '') return { type: 'true' };
	if (isLegacyWord(trimmed)) return parseWhen(LEGACY_WHEN_ALIASES[trimmed]);
	return new Parser(src, tokenize(src)).parse();
}

/** `parseWhen` that returns the error instead of throwing. */
export function tryParseWhen(
	input: string | undefined | null
): { ok: true; ast: WhenNode } | { ok: false; error: WhenSyntaxError } {
	try {
		return { ok: true, ast: parseWhen(input) };
	} catch (err) {
		if (err instanceof WhenSyntaxError) return { ok: false, error: err };
		throw err;
	}
}

/** Every context key an expression names — the seam the validator's
 *  `W_UNKNOWN_CONTEXT_KEY` / `W_FOCUS_IN_PLACEMENT` checks read. */
export function contextKeysOf(node: WhenNode): string[] {
	const out = new Set<string>();
	const walk = (n: WhenNode) => {
		switch (n.type) {
			case 'key':
			case 'eq':
			case 'ne':
			case 'glob':
				out.add(n.key);
				return;
			case 'not':
				walk(n.operand);
				return;
			case 'and':
			case 'or':
				n.operands.forEach(walk);
				return;
			default:
				return;
		}
	};
	walk(node);
	return [...out];
}

// ─── Serialization and normalization (§4.5) ───────────────────────────────

function quote(s: string): string {
	return `'${s.replace(/\\/g, '\\\\').replace(/'/g, "\\'")}'`;
}

function serializeValue(v: string | boolean): string {
	return typeof v === 'boolean' ? String(v) : quote(v);
}

function serializeInner(n: WhenNode, parent: 'and' | 'or' | 'not' | 'top'): string {
	switch (n.type) {
		case 'true':
			return 'true';
		case 'false':
			return 'false';
		case 'key':
			return n.key;
		case 'eq':
			return `${n.key} == ${serializeValue(n.value)}`;
		case 'ne':
			return `${n.key} != ${serializeValue(n.value)}`;
		case 'glob':
			return `${n.key} =~ ${quote(n.pattern)}`;
		case 'not': {
			const o = n.operand;
			const inner = serializeInner(o, 'not');
			return o.type === 'and' || o.type === 'or' ? `!(${inner})` : `!${inner}`;
		}
		case 'and':
			return n.operands.map((o) => serializeInner(o, 'and')).join(' && ');
		case 'or': {
			const s = n.operands.map((o) => serializeInner(o, 'or')).join(' || ');
			return parent === 'and' ? `(${s})` : s;
		}
	}
}

/**
 * Serialize an AST (§4.5 step 5): strings single-quoted with `\'` / `\\`
 * escapes; an `||` inside `&&` is parenthesized; `!` prefixes an atom or a
 * parenthesized group. A top-level TRUE serializes as `''` (the absent
 * `when`); FALSE as `false`. Intended for normalized trees, but safe on any.
 */
export function serializeWhen(node: WhenNode): string {
	if (node.type === 'true') return '';
	return serializeInner(node, 'top');
}

/** Normalize an AST per §4.5 steps 2–4 (bottom-up, syntactic — no De Morgan,
 *  no distribution). */
export function normalizeAst(node: WhenNode): WhenNode {
	switch (node.type) {
		case 'true':
		case 'false':
		case 'key':
		case 'eq':
		case 'ne':
		case 'glob':
			return node;
		case 'not': {
			const o = normalizeAst(node.operand);
			if (o.type === 'not') return o.operand; // !!x → x (o.operand already normalized)
			if (o.type === 'true') return { type: 'false' };
			if (o.type === 'false') return { type: 'true' };
			if (o.type === 'eq') return { type: 'ne', key: o.key, value: o.value };
			if (o.type === 'ne') return { type: 'eq', key: o.key, value: o.value };
			return { type: 'not', operand: o };
		}
		case 'and':
		case 'or': {
			const kind = node.type;
			const identity = kind === 'and' ? 'true' : 'false';
			const absorbing = kind === 'and' ? 'false' : 'true';
			const flat: WhenNode[] = [];
			for (const raw of node.operands) {
				const o = normalizeAst(raw);
				if (o.type === kind) flat.push(...o.operands);
				else flat.push(o);
			}
			if (flat.some((o) => o.type === absorbing)) return { type: absorbing };
			const seen = new Map<string, WhenNode>();
			for (const o of flat) {
				if (o.type === identity) continue;
				// Canonical serialization *as it appears inside this parent*
				// (an `||` operand of `&&` carries its parentheses) — the sort
				// and dedupe key of §4.5 step 3–4.
				const k = serializeInner(o, kind);
				if (!seen.has(k)) seen.set(k, o);
			}
			const operands = [...seen.entries()]
				.sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
				.map(([, o]) => o);
			if (operands.length === 0) return { type: identity };
			if (operands.length === 1) return operands[0];
			return { type: kind, operands };
		}
	}
}

/**
 * The canonical form of a `when` (§4.5): parse (legacy words replaced,
 * `always`/`true` → TRUE, `false` → FALSE), normalize, serialize. `always`
 * and absent both normalize to `''`. This — never the typed string — is what
 * DEC-59 compares. Throws `WhenSyntaxError` on an invalid expression.
 */
export function normalizeWhen(input: string | undefined | null): string {
	return serializeWhen(normalizeAst(parseWhen(input)));
}

/** Structural AST equality (the round-trip tests use it). ASTs are plain
 *  data with a fixed field order, so a JSON comparison is exact. */
export function astEquals(a: WhenNode, b: WhenNode): boolean {
	return JSON.stringify(a) === JSON.stringify(b);
}

// ─── Specificity (§2.3, B-1) ──────────────────────────────────────────────

/**
 * Specificity of a **normalized** AST: TRUE = 0; an atom (a bare key, a
 * comparison, or `!` applied to one) = 1; `&&` = the sum of its operands;
 * `||` = the minimum; `!( … )` of a group = 1. So a disjunction never outranks
 * a narrower `when`. FALSE scores 0 (it never matches anyway).
 */
export function specificity(node: WhenNode): number {
	switch (node.type) {
		case 'true':
		case 'false':
			return 0;
		case 'key':
		case 'eq':
		case 'ne':
		case 'glob':
		case 'not':
			return 1;
		case 'and':
			return node.operands.reduce((sum, o) => sum + specificity(o), 0);
		case 'or':
			return Math.min(...node.operands.map(specificity));
	}
}

/** `specificity(normalizeAst(parseWhen(when)))`. */
export function whenSpecificity(when: string | undefined | null): number {
	return specificity(normalizeAst(parseWhen(when)));
}

// ─── Evaluation (§4.5) ────────────────────────────────────────────────────

export interface EvalOptions {
	/** Active project root (absolute). A glob containing `/` matches the
	 *  value's path relative to it (§4.2); without a root, the value as-is. */
	projectRoot?: string;
}

function stringForm(v: ContextValue): string | undefined {
	if (v === undefined) return undefined;
	return typeof v === 'boolean' ? String(v) : v;
}

/** Whitelist AST walk. Unknown keys read `undefined`. A bare key is truthy
 *  when `true` or a non-empty string; `==` / `!=` compare string forms and
 *  `undefined` equals nothing (so `!=` is true for it); `=~` is false when
 *  the key is undefined. */
export function evaluateAst(node: WhenNode, ctx: WhenContext, opts?: EvalOptions): boolean {
	switch (node.type) {
		case 'true':
			return true;
		case 'false':
			return false;
		case 'key': {
			const v = ctx[node.key];
			return v === true || (typeof v === 'string' && v.length > 0);
		}
		case 'eq':
		case 'ne': {
			const v = stringForm(ctx[node.key]);
			const equal = v !== undefined && v === stringForm(node.value);
			return node.type === 'eq' ? equal : !equal;
		}
		case 'glob': {
			const v = stringForm(ctx[node.key]);
			if (v === undefined) return false;
			return globMatchPath(node.pattern, v, opts?.projectRoot);
		}
		case 'not':
			return !evaluateAst(node.operand, ctx, opts);
		case 'and':
			return node.operands.every((o) => evaluateAst(o, ctx, opts));
		case 'or':
			return node.operands.some((o) => evaluateAst(o, ctx, opts));
	}
}

// Parsed-AST cache: dispatch evaluates the same few dozen `when`s on every
// keypress. Bounded so a stream of distinct user strings can't grow it.
const AST_CACHE = new Map<string, WhenNode | WhenSyntaxError>();
const AST_CACHE_MAX = 512;

function cachedAst(when: string): WhenNode | WhenSyntaxError {
	const hit = AST_CACHE.get(when);
	if (hit) return hit;
	const r = tryParseWhen(when);
	const value = r.ok ? normalizeAst(r.ast) : r.error;
	if (AST_CACHE.size >= AST_CACHE_MAX) AST_CACHE.clear();
	AST_CACHE.set(when, value);
	return value;
}

/**
 * Evaluate a `when` string against a context snapshot. An expression that
 * does not parse evaluates **false** (and logs once per string) — a broken
 * rule never fires, it never fires everywhere. Legacy clauses evaluate
 * exactly as before: `global` → true; `not-input` → `!inputFocus`, which is
 * `!isTypingTarget(...)` when the context comes from `context-keys.ts`;
 * `terminal-focus` → `terminalFocus` (its only default rules are hosted
 * `terminal.*` commands the frame dispatcher never fires — §4.4/§4.6 — which
 * is what kept `evaluateWhen('terminal-focus')` false at the dispatcher).
 */
export function evaluateWhen(
	when: string | undefined | null,
	ctx: WhenContext,
	opts?: EvalOptions
): boolean {
	const ast = cachedAst(when ?? '');
	if (ast instanceof WhenSyntaxError) {
		if (!warned.has(when ?? '')) {
			warned.add(when ?? '');
			console.warn(`[keymap] invalid when: ${ast.message}`);
		}
		return false;
	}
	return evaluateAst(ast, ctx, opts);
}
const warned = new Set<string>();

/** True when `target` is a real text-entry surface: a native `input` /
 *  `textarea`, or anything marked `contenteditable="true"`. This is exactly
 *  the `inputFocus` context key (§4.3) — xterm's helper textarea counts. */
export function isTypingTarget(target: EventTarget | null): boolean {
	if (typeof HTMLElement === 'undefined' || !(target instanceof HTMLElement)) return false;
	return target.matches('input, textarea, [contenteditable="true"]');
}

// ─── Glob dialect (`=~`, §4.2) ────────────────────────────────────────────
//
// `*` (not across `/`), `**` (across `/`), `?` (one char, not `/`), `[…]`
// classes with ranges, `{a,b}` alternatives (nestable). No negation, no
// regex. `\x` escapes `x`. Case-sensitive; `/` separators on every platform
// (a Windows `\` in the value is converted before matching).

const MAX_BRACE_EXPANSIONS = 256;

function expandBraces(pattern: string): string[] {
	// Find the first top-level `{` with a matching `}` and at least one `,`.
	let depth = 0;
	let open = -1;
	for (let i = 0; i < pattern.length; i++) {
		const c = pattern[i];
		if (c === '\\') {
			i++;
			continue;
		}
		if (c === '{') {
			if (depth === 0) open = i;
			depth++;
		} else if (c === '}' && depth > 0) {
			depth--;
			if (depth === 0 && open >= 0) {
				const body = pattern.slice(open + 1, i);
				const alts = splitTopLevelCommas(body);
				if (alts.length < 2) {
					open = -1;
					continue;
				}
				const head = pattern.slice(0, open);
				const tail = pattern.slice(i + 1);
				const out: string[] = [];
				for (const alt of alts) {
					for (const rest of expandBraces(alt + tail)) {
						out.push(head + rest);
						if (out.length >= MAX_BRACE_EXPANSIONS) return out;
					}
				}
				return out;
			}
		}
	}
	return [pattern];
}

function splitTopLevelCommas(body: string): string[] {
	const out: string[] = [];
	let depth = 0;
	let start = 0;
	for (let i = 0; i < body.length; i++) {
		const c = body[i];
		if (c === '\\') {
			i++;
			continue;
		}
		if (c === '{') depth++;
		else if (c === '}') depth--;
		else if (c === ',' && depth === 0) {
			out.push(body.slice(start, i));
			start = i + 1;
		}
	}
	out.push(body.slice(start));
	return out;
}

type GlobTok =
	| { k: 'lit'; c: string }
	| { k: 'any1' }
	| { k: 'star' }
	| { k: 'globstar' } // `**` — any run of chars, `/` included
	| { k: 'globstarSlash' } // `**/` — zero or more whole segments
	| { k: 'class'; ranges: Array<[string, string]> };

function compileGlob(p: string): GlobTok[] {
	const out: GlobTok[] = [];
	for (let i = 0; i < p.length; i++) {
		const c = p[i];
		if (c === '\\' && i + 1 < p.length) {
			out.push({ k: 'lit', c: p[++i] });
		} else if (c === '*') {
			if (p[i + 1] === '*') {
				if (p[i + 2] === '/') {
					out.push({ k: 'globstarSlash' });
					i += 2;
				} else {
					out.push({ k: 'globstar' });
					i += 1;
				}
			} else {
				out.push({ k: 'star' });
			}
		} else if (c === '?') {
			out.push({ k: 'any1' });
		} else if (c === '[') {
			const close = p.indexOf(']', i + 2);
			if (close < 0) {
				out.push({ k: 'lit', c });
				continue;
			}
			const body = p.slice(i + 1, close);
			const ranges: Array<[string, string]> = [];
			for (let j = 0; j < body.length; j++) {
				if (body[j + 1] === '-' && j + 2 < body.length) {
					ranges.push([body[j], body[j + 2]]);
					j += 2;
				} else {
					ranges.push([body[j], body[j]]);
				}
			}
			out.push({ k: 'class', ranges });
			i = close;
		} else {
			out.push({ k: 'lit', c });
		}
	}
	return out;
}

function matchTokens(toks: GlobTok[], s: string): boolean {
	// Memoized DP over (token index, string index) — linear in |toks|·|s|,
	// so no pattern can backtrack exponentially.
	const memo = new Map<number, boolean>();
	const width = s.length + 1;
	const go = (ti: number, si: number): boolean => {
		const key = ti * width + si;
		const hit = memo.get(key);
		if (hit !== undefined) return hit;
		let r: boolean;
		if (ti === toks.length) {
			r = si === s.length;
		} else {
			const t = toks[ti];
			switch (t.k) {
				case 'lit':
					r = si < s.length && s[si] === t.c && go(ti + 1, si + 1);
					break;
				case 'any1':
					r = si < s.length && s[si] !== '/' && go(ti + 1, si + 1);
					break;
				case 'class':
					r =
						si < s.length &&
						s[si] !== '/' &&
						t.ranges.some(([lo, hi]) => s[si] >= lo && s[si] <= hi) &&
						go(ti + 1, si + 1);
					break;
				case 'star':
					r = go(ti + 1, si) || (si < s.length && s[si] !== '/' && go(ti, si + 1));
					break;
				case 'globstar':
					r = go(ti + 1, si) || (si < s.length && go(ti, si + 1));
					break;
				case 'globstarSlash': {
					// zero segments, or consume through the next `/` and retry.
					r = go(ti + 1, si);
					if (!r) {
						const slash = s.indexOf('/', si);
						r = slash >= 0 && go(ti, slash + 1);
					}
					break;
				}
			}
		}
		memo.set(key, r);
		return r;
	};
	return go(0, 0);
}

/** Match `value` against a glob, whole-string. */
export function globMatch(pattern: string, value: string): boolean {
	return expandBraces(pattern).some((alt) => matchTokens(compileGlob(alt), value));
}

/**
 * §4.2 path semantics: a glob **without** `/` matches the basename of the
 * value (`'*.ts'` matches `/a/b/c.ts`); a glob **with** `/` matches the
 * project-relative path (`'src/**\/*.rs'`) when `projectRoot` is known and
 * contains the value, else the value as given.
 */
export function globMatchPath(pattern: string, value: string, projectRoot?: string): boolean {
	const v = value.replace(/\\/g, '/');
	if (!pattern.includes('/')) {
		const base = v.slice(v.lastIndexOf('/') + 1);
		return globMatch(pattern, base);
	}
	let target = v;
	if (projectRoot) {
		const root = projectRoot.replace(/\\/g, '/').replace(/\/+$/, '');
		if (target.startsWith(`${root}/`)) target = target.slice(root.length + 1);
	}
	return globMatch(pattern, target);
}
