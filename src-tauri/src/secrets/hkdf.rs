//! HMAC-SHA256 (RFC 2104) and HKDF-SHA256 (RFC 5869), hand-rolled over the
//! `sha2` the crate already depends on.
//!
//! Why not the `hmac` / `hkdf` crates (WP-74a adds both as direct
//! dependencies for WP-74b, G-ACCESS §10.1): neither wipes its intermediate
//! key material. `hmac` 0.12 builds its padded key block in a plain local
//! array, `hkdf` 0.12's extract returns the PRK as a plain `GenericArray`,
//! and `mac.finalize().into_bytes()` hands back another; neither crate
//! implements `Zeroize`/`ZeroizeOnDrop` (no `zeroize` feature exists in those
//! versions), so the per-principal key derivation would leave the PRK and
//! key-derived blocks on the stack. Here every key-derived buffer (padded key block, ipad, opad, inner
//! hash, PRK, each T(i)) is a `Zeroizing` wrapper and digests are finalized
//! straight into those wrappers (`finalize_into`), never into an unwiped
//! temporary. The one residue neither approach can wipe is a consumed
//! `Sha256`'s chaining state (sha2 0.10 has no `zeroize` either). The tests
//! below pin both constructions to the RFC 4231 / RFC 5869 vectors.

use sha2::digest::generic_array::GenericArray;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

const BLOCK: usize = 64;
pub const HASH_LEN: usize = 32;

/// HMAC-SHA256(key, message), with the message given in parts (so callers
/// never concatenate secrets into a temporary buffer).
pub fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> Zeroizing<[u8; HASH_LEN]> {
    let mut block = Zeroizing::new([0u8; BLOCK]);
    if key.len() > BLOCK {
        let mut hasher = Sha256::new();
        hasher.update(key);
        hasher.finalize_into(GenericArray::from_mut_slice(&mut block[..HASH_LEN]));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut ipad = Zeroizing::new([0x36u8; BLOCK]);
    let mut opad = Zeroizing::new([0x5cu8; BLOCK]);
    for i in 0..BLOCK {
        ipad[i] ^= block[i];
        opad[i] ^= block[i];
    }
    let mut inner = Sha256::new();
    inner.update(&ipad[..]);
    for part in parts {
        inner.update(part);
    }
    let mut inner_hash = Zeroizing::new([0u8; HASH_LEN]);
    inner.finalize_into(GenericArray::from_mut_slice(&mut inner_hash[..]));
    let mut outer = Sha256::new();
    outer.update(&opad[..]);
    outer.update(&inner_hash[..]);
    let mut out = Zeroizing::new([0u8; HASH_LEN]);
    outer.finalize_into(GenericArray::from_mut_slice(&mut out[..]));
    out
}

/// HKDF-SHA256 extract-then-expand, `out.len()` ≤ 255 × 32 bytes.
pub fn hkdf_sha256(salt: &[u8], ikm: &[u8], info: &[u8], out: &mut [u8]) {
    assert!(out.len() <= 255 * HASH_LEN, "HKDF output too long");
    // RFC 5869 §2.2: an absent salt is HashLen zero bytes.
    let zero_salt = [0u8; HASH_LEN];
    let salt = if salt.is_empty() {
        &zero_salt[..]
    } else {
        salt
    };
    let prk = hmac_sha256(salt, &[ikm]);
    let mut previous: Zeroizing<[u8; HASH_LEN]> = Zeroizing::new([0u8; HASH_LEN]);
    let mut previous_len = 0usize;
    for (index, chunk) in out.chunks_mut(HASH_LEN).enumerate() {
        let counter = [(index + 1) as u8];
        let block = hmac_sha256(&prk[..], &[&previous[..previous_len], info, &counter]);
        chunk.copy_from_slice(&block[..chunk.len()]);
        *previous = *block;
        previous_len = HASH_LEN;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        hex::decode(s.replace([' ', '\n'], "")).unwrap()
    }

    /// RFC 4231 test cases 1, 2 and 6 (short key, short ASCII key, a key
    /// longer than the block).
    #[test]
    fn hmac_matches_rfc_4231() {
        assert_eq!(
            hmac_sha256(&[0x0b; 20], &[b"Hi There"]).to_vec(),
            hex("b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7")
        );
        assert_eq!(
            hmac_sha256(b"Jefe", &[b"what do ya want ", b"for nothing?"]).to_vec(),
            hex("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843")
        );
        assert_eq!(
            hmac_sha256(
                &[0xaa; 131],
                &[b"Test Using Larger Than Block-Size Key - Hash Key First"]
            )
            .to_vec(),
            hex("60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54")
        );
    }

    /// RFC 5869 A.1 (basic), A.2 (longer inputs, multi-block output) and
    /// A.3 (no salt, no info).
    #[test]
    fn hkdf_matches_rfc_5869() {
        let mut okm = [0u8; 42];
        hkdf_sha256(
            &hex("000102030405060708090a0b0c"),
            &[0x0b; 22],
            &hex("f0f1f2f3f4f5f6f7f8f9"),
            &mut okm,
        );
        assert_eq!(
            okm.to_vec(),
            hex(
                "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf
                 34007208d5b887185865"
            )
        );

        let ikm: Vec<u8> = (0x00..=0x4f).collect();
        let salt: Vec<u8> = (0x60..=0xaf).collect();
        let info: Vec<u8> = (0xb0..=0xff).collect();
        let mut okm = [0u8; 82];
        hkdf_sha256(&salt, &ikm, &info, &mut okm);
        assert_eq!(
            okm.to_vec(),
            hex(
                "b11e398dc80327a1c8e7f78c596a49344f012eda2d4efad8a050cc4c19afa97c
                 59045a99cac7827271cb41c65e590e09da3275600c2f09b8367793a9aca3db71
                 cc30c58179ec3e87c14c01d5c1f3434f1d87"
            )
        );

        let mut okm = [0u8; 42];
        hkdf_sha256(&[], &[0x0b; 22], &[], &mut okm);
        assert_eq!(
            okm.to_vec(),
            hex(
                "8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d
                 9d201395faa4b61a96c8"
            )
        );
    }
}
