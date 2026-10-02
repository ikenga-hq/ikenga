//! The pairing fingerprint phrase (G-ACCESS §3.5, P-8, DEC-77).
//!
//! Four words from the **EFF Large Wordlist** (7776 words, CC BY 3.0 US —
//! the list Bitwarden's fingerprint phrase uses), derived from the 16-byte
//! `fp_seed` (`HKDF-SHA256(K, pairing_id, "ikenga-pair-v1 fingerprint")`):
//! `n = fp_seed` as a big-endian u128; for i in 0..4,
//! `word[i] = WORDS[n mod 7776]`, then `n = n div 7776`. About 51.7 bits.
//!
//! **One copy of the list** (review m-3): `wordlist_eff_large.txt` beside
//! this file is the EFF file verbatim (`<dice>\t<word>` lines). Rust embeds
//! it here; the browser imports the same file with Vite's `?raw`
//! (`src/lib/access/fingerprint.ts`). A-14 pins its SHA-256 on both sides.

use std::sync::OnceLock;

/// The EFF file, verbatim.
pub const WORDLIST_FILE: &str = include_str!("wordlist_eff_large.txt");

/// SHA-256 of [`WORDLIST_FILE`] — the published
/// `https://www.eff.org/files/2016/07/18/eff_large_wordlist.txt`. Pinned on
/// both sides (A-14); `src/lib/access/fingerprint.ts` carries the same hex.
pub const WORDLIST_SHA256: &str =
    "addd35536511597a02fa0a9ff1e5284677b8883b83e986e43f15a3db996b903e";

pub const WORD_COUNT: usize = 7776;
pub const PHRASE_WORDS: usize = 4;

/// The 7776 words, in file order.
pub fn words() -> &'static [&'static str] {
    static WORDS: OnceLock<Vec<&'static str>> = OnceLock::new();
    WORDS.get_or_init(|| {
        WORDLIST_FILE
            .lines()
            .filter_map(|line| line.split_once('\t').map(|(_, w)| w.trim()))
            .filter(|w| !w.is_empty())
            .collect()
    })
}

/// The 4-word phrase for a 16-byte `fp_seed`.
pub fn phrase(fp_seed: &[u8; 16]) -> [&'static str; PHRASE_WORDS] {
    let list = words();
    debug_assert_eq!(list.len(), WORD_COUNT);
    let mut n = u128::from_be_bytes(*fp_seed);
    let base = list.len() as u128;
    let mut out = [""; PHRASE_WORDS];
    for slot in &mut out {
        *slot = list[(n % base) as usize];
        n /= base;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    /// A-14 (Rust half): the one list file is the pinned EFF file.
    #[test]
    fn the_wordlist_is_the_pinned_eff_large_list() {
        assert_eq!(
            hex::encode(Sha256::digest(WORDLIST_FILE.as_bytes())),
            WORDLIST_SHA256
        );
        let w = words();
        assert_eq!(w.len(), WORD_COUNT);
        assert_eq!((w[0], w[WORD_COUNT - 1]), ("abacus", "zoom"));
        let mut sorted = w.to_vec();
        sorted.dedup();
        assert_eq!(sorted.len(), WORD_COUNT, "no duplicate words");
    }

    /// The derivation is little-endian in word order over a big-endian u128.
    #[test]
    fn phrase_reads_the_seed_big_endian_low_digit_first() {
        assert_eq!(phrase(&[0u8; 16]), ["abacus"; 4]);
        let mut one = [0u8; 16];
        one[15] = 1;
        assert_eq!(phrase(&one), [words()[1], "abacus", "abacus", "abacus"]);
        // n = 7776 → digits (0, 1, 0, 0).
        let n: u128 = 7776;
        assert_eq!(
            phrase(&n.to_be_bytes()),
            ["abacus", words()[1], "abacus", "abacus"]
        );
        let max = [0xffu8; 16];
        let p = phrase(&max);
        let mut n = u128::MAX;
        for word in p {
            assert_eq!(word, words()[(n % 7776) as usize]);
            n /= 7776;
        }
    }
}
