//! HMAC-SHA256 (RFC 2104) and HKDF-SHA256 (RFC 5869), as thin wrappers over
//! the `hmac` / `hkdf` crates (added as direct dependencies by WP-74a,
//! G-ACCESS §3.5).
//!
//! WP-21 first shipped these hand-rolled over `sha2`, because it ran beside
//! WP-74a in one wave and added no crate (G-ACCESS §10.1). The signatures are
//! unchanged and the tests below still pin both to the RFC 4231 / RFC 5869
//! vectors, so the swap is checked by the same vectors.

use ::hkdf::Hkdf;
use ::hmac::{Hmac, Mac};
use sha2::Sha256;
use zeroize::Zeroizing;

pub const HASH_LEN: usize = 32;

/// HMAC-SHA256(key, message), with the message given in parts (so callers
/// never concatenate secrets into a temporary buffer).
pub fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> Zeroizing<[u8; HASH_LEN]> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC takes any key length");
    for part in parts {
        mac.update(part);
    }
    let mut out = Zeroizing::new([0u8; HASH_LEN]);
    out.copy_from_slice(&mac.finalize().into_bytes());
    out
}

/// HKDF-SHA256 extract-then-expand, `out.len()` ≤ 255 × 32 bytes. An empty
/// `salt` is RFC 5869 §2.2's absent salt (HMAC zero-pads the key, so the two
/// are the same PRK).
pub fn hkdf_sha256(salt: &[u8], ikm: &[u8], info: &[u8], out: &mut [u8]) {
    Hkdf::<Sha256>::new(Some(salt), ikm)
        .expand(info, out)
        .expect("HKDF output too long");
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
