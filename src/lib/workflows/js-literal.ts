/**
 * Non-executing readers for object/array literals embedded in an imported
 * JavaScript file (WP-31 review fix, Round 29).
 *
 * The workflow importers read two literals out of source they did not write:
 * groundwork's `const WAVES = [...]` (pure JSON — `JSON.stringify(waves, null, 2)`)
 * and a workflow script's `export const meta = { ... }` (hand-written JS: bare
 * keys, single quotes, comments). Both used to be handed to
 * `new Function(...)`, which *executes* the imported file. Nothing here
 * executes anything: `sliceBalanced` is a string-aware scanner and
 * `parseJsObjectLiteral` normalizes a literal to JSON and calls `JSON.parse`.
 *
 * Deliberate limits — a literal containing a computed value (a call, a
 * template string, a reference to another binding, a spread) is *not*
 * supported and fails to parse. That is the point: an importer has no business
 * resolving those, and the caller degrades instead of evaluating.
 */

/**
 * Find the balanced `open`…`close` slice starting at the first `open` at or
 * after `from`, skipping anything inside a string literal or a comment (a
 * groundwork `brief` routinely contains `[`, `]`, `{`, `}` and `)()`).
 * Returns `null` when there is no balanced slice.
 */
export function sliceBalanced(
  content: string,
  from: number,
  open: '[' | '{',
  close: ']' | '}',
): string | null {
  const start = content.indexOf(open, from);
  if (start === -1) return null;

  let depth = 0;
  let i = start;
  while (i < content.length) {
    const ch = content[i];

    if (ch === '"' || ch === "'" || ch === '`') {
      i = skipString(content, i);
      continue;
    }
    if (ch === '/' && content[i + 1] === '/') {
      const nl = content.indexOf('\n', i);
      i = nl === -1 ? content.length : nl + 1;
      continue;
    }
    if (ch === '/' && content[i + 1] === '*') {
      const end = content.indexOf('*/', i + 2);
      i = end === -1 ? content.length : end + 2;
      continue;
    }

    if (ch === open) {
      depth++;
    } else if (ch === close) {
      depth--;
      if (depth === 0) return content.slice(start, i + 1);
    }
    i++;
  }
  return null;
}

/** Index just past the string literal that starts at `i`. */
function skipString(src: string, i: number): number {
  const quote = src[i];
  let j = i + 1;
  while (j < src.length) {
    if (src[j] === '\\') {
      j += 2;
      continue;
    }
    if (src[j] === quote) return j + 1;
    j++;
  }
  return src.length;
}

const BARE_KEY = /^[A-Za-z_$][A-Za-z0-9_$]*/;

/**
 * Parse a JS object/array literal by normalizing it to JSON, then
 * `JSON.parse`. Returns `undefined` when the literal uses anything beyond
 * JSON-expressible values (calls, identifiers, template strings, spreads).
 *
 * Never evaluates the input.
 */
export function parseJsObjectLiteral(slice: string): unknown {
  let out = '';
  let i = 0;
  // True when the next token in value position could be an object key.
  let expectKey = false;
  const stack: Array<'object' | 'array'> = [];

  while (i < slice.length) {
    const ch = slice[i];

    // ── comments ────────────────────────────────────────────────────────────
    if (ch === '/' && slice[i + 1] === '/') {
      const nl = slice.indexOf('\n', i);
      i = nl === -1 ? slice.length : nl;
      continue;
    }
    if (ch === '/' && slice[i + 1] === '*') {
      const end = slice.indexOf('*/', i + 2);
      i = end === -1 ? slice.length : end + 2;
      continue;
    }

    // ── strings ─────────────────────────────────────────────────────────────
    if (ch === '"' || ch === "'") {
      const end = skipString(slice, i);
      out += requote(slice.slice(i, end));
      i = end;
      expectKey = false;
      continue;
    }
    if (ch === '`') {
      // A template literal may interpolate — not JSON-expressible.
      return undefined;
    }

    // ── structure ───────────────────────────────────────────────────────────
    if (ch === '{') {
      stack.push('object');
      expectKey = true;
      out += ch;
      i++;
      continue;
    }
    if (ch === '[') {
      stack.push('array');
      expectKey = false;
      out += ch;
      i++;
      continue;
    }
    if (ch === '}' || ch === ']') {
      stack.pop();
      out = dropTrailingComma(out);
      out += ch;
      expectKey = false;
      i++;
      continue;
    }
    if (ch === ',') {
      expectKey = stack[stack.length - 1] === 'object';
      out += ch;
      i++;
      continue;
    }
    if (ch === ':') {
      expectKey = false;
      out += ch;
      i++;
      continue;
    }
    if (/\s/.test(ch)) {
      out += ch;
      i++;
      continue;
    }

    // ── bare identifiers ────────────────────────────────────────────────────
    const bare = slice.slice(i).match(BARE_KEY);
    if (bare) {
      const word = bare[0];
      const rest = slice.slice(i + word.length);
      const isKey = expectKey && /^\s*:/.test(rest);
      if (isKey) {
        out += JSON.stringify(word);
        i += word.length;
        expectKey = false;
        continue;
      }
      if (word === 'true' || word === 'false' || word === 'null') {
        out += word;
        i += word.length;
        continue;
      }
      // An identifier in value position (or a function call) — refuse.
      return undefined;
    }

    // ── numbers ─────────────────────────────────────────────────────────────
    const num = slice.slice(i).match(/^-?\d+(\.\d+)?([eE][+-]?\d+)?/);
    if (num) {
      out += num[0];
      i += num[0].length;
      continue;
    }

    // Anything else (operators, spreads, parens) is not JSON-expressible.
    return undefined;
  }

  try {
    return JSON.parse(out);
  } catch {
    return undefined;
  }
}

/** Re-emit a JS string literal as a JSON string literal. */
function requote(literal: string): string {
  const quote = literal[0];
  const body = literal.slice(1, -1);
  if (quote === '"') {
    // Already JSON-shaped, except that `\'` is legal JS and illegal JSON.
    return `"${body.replace(/\\'/g, "'")}"`;
  }
  // Single-quoted: unescape `\'`, escape any bare `"`.
  let decoded = '';
  for (let i = 0; i < body.length; i++) {
    if (body[i] === '\\') {
      const next = body[i + 1];
      if (next === "'") {
        decoded += "'";
        i++;
        continue;
      }
      decoded += body[i] + (next ?? '');
      i++;
      continue;
    }
    decoded += body[i] === '"' ? '\\"' : body[i];
  }
  return `"${decoded}"`;
}

function dropTrailingComma(out: string): string {
  const trimmed = out.replace(/\s+$/, '');
  if (trimmed.endsWith(',')) {
    return trimmed.slice(0, -1);
  }
  return out;
}
