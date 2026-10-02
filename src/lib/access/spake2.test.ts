// A-13 / A-14 (WP-74b): the TS port replays the Rust-generated vectors
// (`src-tauri/src/access/testdata/spake2_vectors.json`) both ways, and the
// fingerprint reads the one pinned wordlist file.

import { sha256 } from '@noble/hashes/sha2.js';
import { describe, expect, it } from 'vitest';

import vectorsFile from '../../../src-tauri/src/access/testdata/spake2_vectors.json?raw';
import {
	fingerprintPhrase,
	WORD_COUNT,
	WORDLIST_FILE,
	WORDLIST_SHA256,
	words,
} from './fingerprint';
import {
	b64url,
	bytesEqual,
	deriveKeys,
	fromHex,
	hostIdentity,
	ID_A,
	pairPassword,
	startA,
	startB,
	toHex,
	unb64url,
	utf8,
} from './spake2';

interface Vector {
	code: string;
	password: string;
	storeId: string;
	idA: string;
	idB: string;
	pairingId: string;
	rngA: string;
	rngB: string;
	msgA: string;
	msgB: string;
	key: string;
	kConfirm: string;
	pollKey: string;
	fpSeed: string;
	transcript: string;
	hostConfirm: string;
	deviceConfirm: string;
	fingerprint: string[];
}

const doc = JSON.parse(vectorsFile) as { wordlistSha256: string; vectors: Vector[] };

describe('spake2 (A-13)', () => {
	it('has vectors to replay', () => {
		expect(doc.vectors.length).toBeGreaterThan(0);
	});

	for (const v of doc.vectors) {
		describe(`vector ${v.code}`, () => {
			const pw = pairPassword(v.code);
			const idA = utf8(ID_A);
			const idB = hostIdentity(v.storeId);

			it('builds the same password and identities', () => {
				expect(toHex(pw)).toBe(v.password);
				expect(toHex(idA)).toBe(v.idA);
				expect(toHex(idB)).toBe(v.idB);
			});

			it('side A: same message, and the same key from the Rust host message', () => {
				const a = startA(pw, idA, idB, fromHex(v.rngA));
				expect(toHex(a.msg)).toBe(v.msgA);
				expect(toHex(a.finish(fromHex(v.msgB)))).toBe(v.key);
			});

			it('side B: same message, and the same key from the Rust device message', () => {
				const b = startB(pw, idA, idB, fromHex(v.rngB));
				expect(toHex(b.msg)).toBe(v.msgB);
				expect(toHex(b.finish(fromHex(v.msgA)))).toBe(v.key);
			});

			it('derives the same confirmations, poll key and words (A-14)', () => {
				const k = deriveKeys(fromHex(v.key), v.pairingId, fromHex(v.msgA), fromHex(v.msgB));
				expect(toHex(k.kConfirm)).toBe(v.kConfirm);
				expect(toHex(k.pollKey)).toBe(v.pollKey);
				expect(toHex(k.fpSeed)).toBe(v.fpSeed);
				expect(toHex(k.transcript)).toBe(v.transcript);
				expect(toHex(k.hostConfirm)).toBe(v.hostConfirm);
				expect(toHex(k.deviceConfirm)).toBe(v.deviceConfirm);
				expect(fingerprintPhrase(k.fpSeed)).toEqual(v.fingerprint);
			});
		});
	}

	it('a live TS ↔ TS run agrees; a wrong code disagrees', () => {
		const idA = utf8(ID_A);
		const idB = hostIdentity('store');
		const a = startA(pairPassword('K7P42Q'), idA, idB);
		const b = startB(pairPassword('K7P42Q'), idA, idB);
		expect(bytesEqual(a.finish(b.msg), b.finish(a.msg))).toBe(true);
		const wrong = startB(pairPassword('K7P42R'), idA, idB);
		expect(bytesEqual(a.finish(wrong.msg), wrong.finish(a.msg))).toBe(false);
	});

	it('refuses malformed host messages', () => {
		const a = startA(pairPassword('K7P42Q'), utf8(ID_A), hostIdentity('s'));
		expect(() => a.finish(new Uint8Array(32))).toThrow(/length/);
		const sameSide = Uint8Array.from(a.msg);
		expect(() => a.finish(sameSide)).toThrow(/side/);
	});

	it('round-trips base64url without padding', () => {
		const bytes = Uint8Array.from([0xfb, 0xff, 0x00, 0x10]);
		expect(b64url(bytes)).toBe('-_8AEA');
		expect(Array.from(unb64url(b64url(bytes)))).toEqual(Array.from(bytes));
	});
});

describe('fingerprint wordlist (A-14)', () => {
	it('is the pinned EFF Large Wordlist, shared with Rust', () => {
		expect(toHex(sha256(utf8(WORDLIST_FILE)))).toBe(WORDLIST_SHA256);
		expect(doc.wordlistSha256).toBe(WORDLIST_SHA256);
		const w = words();
		expect(w.length).toBe(WORD_COUNT);
		expect([w[0], w[WORD_COUNT - 1]]).toEqual(['abacus', 'zoom']);
	});

	it('reads the seed big-endian, low digit first', () => {
		expect(fingerprintPhrase(new Uint8Array(16))).toEqual(['abacus', 'abacus', 'abacus', 'abacus']);
		const one = new Uint8Array(16);
		one[15] = 1;
		expect(fingerprintPhrase(one)[0]).toBe(words()[1]);
	});
});
