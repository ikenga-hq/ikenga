// G-ACCESS §3.5 (WP-74b) — the 4-word pairing fingerprint phrase.
//
// The EFF Large Wordlist (7776 words, CC BY 3.0 US) lives ONCE, at
// `src-tauri/src/access/wordlist_eff_large.txt` (the EFF file verbatim,
// `<dice>\t<word>` lines). Rust embeds it with `include_str!`; this side
// imports the same file through Vite's raw import (review m-3). A-14 pins
// its SHA-256 on both sides.
//
// Derivation: n = fpSeed as a big-endian u128; for i in 0..4:
// word[i] = WORDS[n mod 7776], n = n div 7776.

import wordlistFile from '../../../src-tauri/src/access/wordlist_eff_large.txt?raw';

/** SHA-256 of the list file — same pin as `fingerprint.rs` `WORDLIST_SHA256`. */
export const WORDLIST_SHA256 = 'addd35536511597a02fa0a9ff1e5284677b8883b83e986e43f15a3db996b903e';
export const WORD_COUNT = 7776;

export const WORDLIST_FILE: string = wordlistFile;

let cached: string[] | null = null;

/** The 7776 words, in file order. */
export function words(): string[] {
	if (!cached) {
		cached = WORDLIST_FILE.split('\n')
			.map((line) => line.split('\t')[1]?.trim() ?? '')
			.filter((w) => w.length > 0);
	}
	return cached;
}

export type Fingerprint = [string, string, string, string];

/** The 4 words both sides show for a 16-byte `fpSeed`. */
export function fingerprintPhrase(fpSeed: Uint8Array): Fingerprint {
	if (fpSeed.length !== 16) throw new Error('fingerprint: fpSeed is 16 bytes');
	const list = words();
	const base = BigInt(list.length);
	let n = 0n;
	for (const b of fpSeed) n = (n << 8n) | BigInt(b);
	const out: string[] = [];
	for (let i = 0; i < 4; i++) {
		out.push(list[Number(n % base)] ?? '');
		n /= base;
	}
	return out as Fingerprint;
}
