// G-ACCESS §3.2 (P-5, P-6; WP-74b) — the pairing code format. No imports, so
// the boot-time re-auth overlay can use it without loading the curve code.
//
// Crockford base32 without I L O U (32 symbols), 6 symbols shown `XXX-XXX`.
// Symbol 1 is the public slot; symbols 2–6 are the 25 secret bits.

export const PAIR_ALPHABET = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';

/** Accept lowercase, map O→0 and I/L→1, strip `-` and whitespace. `null`
 *  unless exactly 6 alphabet symbols remain. */
export function normalizePairCode(input: string): string | null {
	let out = '';
	for (const raw of input) {
		if (raw === '-' || /\s/.test(raw)) continue;
		let c = raw.toUpperCase();
		if (c === 'O') c = '0';
		if (c === 'I' || c === 'L') c = '1';
		if (!PAIR_ALPHABET.includes(c)) return null;
		out += c;
	}
	return out.length === 6 ? out : null;
}

/** `K7P42Q` → `K7P-42Q`. */
export function displayPairCode(normalized: string): string {
	return normalized.length === 6 ? `${normalized.slice(0, 3)}-${normalized.slice(3)}` : normalized;
}
