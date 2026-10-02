// G-ACCESS §3.4 / §3.5 (WP-74b) — the device side of the pairing handshake.
//
// A port of RustCrypto `spake2 =0.4.0` (`Ed25519Group`) on audited
// `@noble/curves` v2 (`ed25519.Point`) and `@noble/hashes` v2. The host runs
// the Rust crate as side B (`src-tauri/src/access/spake.rs`); the browser is
// side A. Both are checked against the same fixed vectors
// (`src-tauri/src/access/testdata/spake2_vectors.json`, A-13).
//
// The construction, read from `spake2-0.4.0/src/{lib,ed25519}.rs`:
// - password scalar: HKDF-SHA256(salt="", ikm=pw, info="SPAKE2 pw", L=48),
//   read big-endian, reduced mod ℓ;
// - message: side ‖ compress(x·B + pw·M) (A = 0x41, B = 0x42; 33 bytes);
// - shared point: K = compress((Y − pw·N)·x) on side A (M on side B);
// - key: SHA-256(SHA-256(pw) ‖ SHA-256(idA) ‖ SHA-256(idB) ‖ X ‖ Y ‖ K).
// `x` is `Scalar::random`: 64 random bytes read little-endian, reduced mod ℓ.

import { ed25519 } from '@noble/curves/ed25519.js';
import { hkdf } from '@noble/hashes/hkdf.js';
import { hmac } from '@noble/hashes/hmac.js';
import { sha256 } from '@noble/hashes/sha2.js';

type Point = InstanceType<typeof ed25519.Point>;

const Point = ed25519.Point;
/** ℓ, the prime order of the Ed25519 base point. */
const L = Point.Fn.ORDER;

// Copied verbatim from `spake2-0.4.0/src/ed25519.rs` (`const_m`, `const_n`).
export const M_HEX = '15cfd18e385952982b6a8f8c7854963b58e34388c8e6dae891db756481a02312';
export const N_HEX = 'f04f2e7eb734b2a8f8b472eaf9c3c632576ac64aea650b496a8a20ff00e583c3';
const M = Point.fromHex(M_HEX);
const N = Point.fromHex(N_HEX);

export const PW_PREFIX = 'ikenga-pair-v1/';
export const ID_A = 'ikenga-pair-v1/device';
export const ID_B_PREFIX = 'ikenga-pair-v1/host/';
export const MSG_LEN = 33;
const SIDE_A = 0x41;
const SIDE_B = 0x42;

const enc = new TextEncoder();
export const utf8 = (s: string): Uint8Array => enc.encode(s);

export function concat(...parts: Uint8Array[]): Uint8Array {
	const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
	let at = 0;
	for (const p of parts) {
		out.set(p, at);
		at += p.length;
	}
	return out;
}

export function toHex(b: Uint8Array): string {
	return Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');
}

export function fromHex(h: string): Uint8Array {
	const out = new Uint8Array(h.length / 2);
	for (let i = 0; i < out.length; i++) out[i] = Number.parseInt(h.slice(i * 2, i * 2 + 2), 16);
	return out;
}

/** base64url, no padding (the wire encoding, §3.4). */
export function b64url(b: Uint8Array): string {
	let s = '';
	for (const x of b) s += String.fromCharCode(x);
	return btoa(s).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

export function unb64url(s: string): Uint8Array {
	const t = s.replace(/-/g, '+').replace(/_/g, '/');
	const bin = atob(t + '='.repeat((4 - (t.length % 4)) % 4));
	return Uint8Array.from(bin, (c) => c.charCodeAt(0));
}

function leToBigint(b: Uint8Array): bigint {
	let n = 0n;
	for (let i = b.length - 1; i >= 0; i--) n = (n << 8n) | BigInt(b[i] ?? 0);
	return n;
}

function beToBigint(b: Uint8Array): bigint {
	let n = 0n;
	for (const x of b) n = (n << 8n) | BigInt(x);
	return n;
}

/** dalek `Scalar::random`: 64 bytes, little-endian, mod ℓ. */
export function scalarFromWide(bytes64: Uint8Array): bigint {
	if (bytes64.length !== 64) throw new Error('spake2: a wide scalar is 64 bytes');
	return leToBigint(bytes64) % L;
}

/** `ed25519_hash_to_scalar` (the password scalar). */
export function passwordScalar(pw: Uint8Array): bigint {
	const okm = hkdf(sha256, pw, new Uint8Array(0), utf8('SPAKE2 pw'), 48);
	return beToBigint(okm) % L;
}

function randomWide(): Uint8Array {
	const b = new Uint8Array(64);
	crypto.getRandomValues(b);
	return b;
}

/** `b"ikenga-pair-v1/" ‖ normalized code` (§3.2). */
export function pairPassword(normalizedCode: string): Uint8Array {
	return utf8(PW_PREFIX + normalizedCode);
}

export function hostIdentity(storeId: string): Uint8Array {
	return utf8(ID_B_PREFIX + storeId);
}

export interface Spake2State {
	side: 'A' | 'B';
	/** This side's 33-byte message. */
	msg: Uint8Array;
	finish(other: Uint8Array): Uint8Array;
}

function start(
	side: 'A' | 'B',
	pw: Uint8Array,
	idA: Uint8Array,
	idB: Uint8Array,
	wide: Uint8Array
): Spake2State {
	const x = scalarFromWide(wide);
	const pwS = passwordScalar(pw);
	if (x === 0n || pwS === 0n) throw new Error('spake2: degenerate scalar');
	const blind = side === 'A' ? M : N;
	const unblind = side === 'A' ? N : M;
	const mine = Point.BASE.multiply(x).add(blind.multiply(pwS));
	const mineBytes = mine.toBytes();
	const msg = concat(Uint8Array.of(side === 'A' ? SIDE_A : SIDE_B), mineBytes);
	return {
		side,
		msg,
		finish(other: Uint8Array): Uint8Array {
			if (other.length !== MSG_LEN) throw new Error('spake2: wrong message length');
			if (other[0] !== (side === 'A' ? SIDE_B : SIDE_A)) throw new Error('spake2: bad side');
			const theirsBytes = other.slice(1);
			let theirs: Point;
			try {
				theirs = Point.fromBytes(theirsBytes);
			} catch {
				throw new Error('spake2: corrupt message');
			}
			const k = theirs.subtract(unblind.multiply(pwS)).multiply(x).toBytes();
			const [first, second] = side === 'A' ? [mineBytes, theirsBytes] : [theirsBytes, mineBytes];
			return sha256(concat(sha256(pw), sha256(idA), sha256(idB), first, second, k));
		},
	};
}

/** Side A (the device). `wide` is for the fixed vectors only. */
export function startA(
	pw: Uint8Array,
	idA: Uint8Array,
	idB: Uint8Array,
	wide: Uint8Array = randomWide()
): Spake2State {
	return start('A', pw, idA, idB, wide);
}

/** Side B (the host's role) — the TS side of the both-ways interop test. */
export function startB(
	pw: Uint8Array,
	idA: Uint8Array,
	idB: Uint8Array,
	wide: Uint8Array = randomWide()
): Spake2State {
	return start('B', pw, idA, idB, wide);
}

/** §3.5: everything derived from the SPAKE2 key. */
export interface PairKeys {
	kConfirm: Uint8Array;
	pollKey: Uint8Array;
	fpSeed: Uint8Array;
	transcript: Uint8Array;
	hostConfirm: Uint8Array;
	deviceConfirm: Uint8Array;
}

export function deriveKeys(
	key: Uint8Array,
	pairingId: string,
	msgA: Uint8Array,
	msgB: Uint8Array
): PairKeys {
	const salt = utf8(pairingId);
	const kConfirm = hkdf(sha256, key, salt, utf8('ikenga-pair-v1 confirm'), 32);
	const pollKey = hkdf(sha256, key, salt, utf8('ikenga-pair-v1 poll'), 32);
	const fpSeed = hkdf(sha256, key, salt, utf8('ikenga-pair-v1 fingerprint'), 16);
	const transcript = sha256(concat(msgA, msgB));
	return {
		kConfirm,
		pollKey,
		fpSeed,
		transcript,
		hostConfirm: hmac(sha256, kConfirm, concat(utf8('host'), transcript)),
		deviceConfirm: hmac(sha256, kConfirm, concat(utf8('device'), transcript)),
	};
}

/** Constant-time-ish byte compare (no early exit). */
export function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
	if (a.length !== b.length) return false;
	let d = 0;
	for (let i = 0; i < a.length; i++) d |= (a[i] ?? 0) ^ (b[i] ?? 0);
	return d === 0;
}
