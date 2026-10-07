//! Web Push message encryption: RFC 8291 over RFC 8188 `aes128gcm`.
//!
//! Per message, the server makes a fresh P-256 key (`as`), agrees a secret
//! with the browser's subscription key (`ua`), and derives the content key
//! and nonce with the subscription's 16-byte `auth` secret. The body is one
//! record:
//!
//! ```text
//! salt(16) ‖ rs = 4096 (u32 BE) ‖ idlen = 65 ‖ as_public(65) ‖ AES-128-GCM(payload ‖ 0x02 ‖ 0x00…)
//! ```
//!
//! [`encrypt_with`] is the pure core (checked against RFC 8291 Appendix A
//! byte-for-byte); [`encrypt`] adds the random key and salt. ring can't
//! import a static ECDH private key, so the vector's `ecdh_secret` is fed to
//! [`encrypt_with`] directly.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Nonce};
use hkdf::Hkdf;
use ring::agreement;
use ring::rand::{SecureRandom, SystemRandom};
use sha2::Sha256;
use zeroize::Zeroizing;

/// RFC 8188 record size. One record holds the whole message.
pub const RECORD_SIZE: u32 = 4096;
/// An uncompressed P-256 point.
pub const PUBLIC_KEY_LEN: usize = 65;
pub const AUTH_LEN: usize = 16;
pub const SALT_LEN: usize = 16;
const TAG_LEN: usize = 16;

#[derive(Debug, PartialEq, Eq)]
pub enum CryptoError {
    /// A subscription key that isn't a valid uncompressed P-256 point, or an
    /// `auth` that isn't 16 bytes.
    BadKey,
    /// The padded plaintext doesn't fit one record.
    TooLong,
    Internal,
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            CryptoError::BadKey => "invalid subscription key",
            CryptoError::TooLong => "payload too long for one record",
            CryptoError::Internal => "encryption failed",
        })
    }
}

impl std::error::Error for CryptoError {}

/// The derived content key and nonce (RFC 8291 §3.3–§3.4).
struct Keys {
    cek: Zeroizing<[u8; 16]>,
    nonce: [u8; 12],
}

fn derive(
    ecdh_secret: &[u8],
    auth: &[u8],
    ua_public: &[u8],
    as_public: &[u8],
    salt: &[u8],
) -> Result<Keys, CryptoError> {
    // IKM = HKDF(salt = auth, ikm = ecdh_secret, info = "WebPush: info\0" ‖ ua ‖ as, 32)
    let mut key_info = Vec::with_capacity(14 + 2 * PUBLIC_KEY_LEN);
    key_info.extend_from_slice(b"WebPush: info\0");
    key_info.extend_from_slice(ua_public);
    key_info.extend_from_slice(as_public);
    let mut ikm = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(Some(auth), ecdh_secret)
        .expand(&key_info, &mut ikm[..])
        .map_err(|_| CryptoError::Internal)?;
    // PRK = HKDF-Extract(salt, IKM); CEK / NONCE = Expand(PRK, info, L)
    let prk = Hkdf::<Sha256>::new(Some(salt), &ikm[..]);
    let mut cek = Zeroizing::new([0u8; 16]);
    prk.expand(b"Content-Encoding: aes128gcm\0", &mut cek[..])
        .map_err(|_| CryptoError::Internal)?;
    let mut nonce = [0u8; 12];
    prk.expand(b"Content-Encoding: nonce\0", &mut nonce)
        .map_err(|_| CryptoError::Internal)?;
    Ok(Keys { cek, nonce })
}

/// The pure RFC 8291 encryption: everything random is an argument.
/// `pad_to` is the plaintext length (payload ‖ `0x02` ‖ zeros); `0` (or
/// anything not longer than the payload) means no padding.
pub fn encrypt_with(
    ecdh_secret: &[u8],
    auth: &[u8],
    ua_public: &[u8],
    as_public: &[u8],
    salt: &[u8; SALT_LEN],
    payload: &[u8],
    pad_to: usize,
) -> Result<Vec<u8>, CryptoError> {
    if auth.len() != AUTH_LEN || ua_public.len() != PUBLIC_KEY_LEN || ua_public[0] != 0x04 {
        return Err(CryptoError::BadKey);
    }
    let len = pad_to.max(payload.len() + 1);
    let header_len = SALT_LEN + 4 + 1 + PUBLIC_KEY_LEN;
    if header_len + len + TAG_LEN > RECORD_SIZE as usize {
        return Err(CryptoError::TooLong);
    }
    let keys = derive(ecdh_secret, auth, ua_public, as_public, salt)?;
    let mut plaintext = Zeroizing::new(Vec::with_capacity(len));
    plaintext.extend_from_slice(payload);
    // RFC 8188 §2: the last record's delimiter, then zero padding.
    plaintext.push(0x02);
    plaintext.resize(len, 0);
    let cipher = Aes128Gcm::new_from_slice(&keys.cek[..]).map_err(|_| CryptoError::Internal)?;
    let sealed = cipher
        .encrypt(Nonce::from_slice(&keys.nonce), plaintext.as_slice())
        .map_err(|_| CryptoError::Internal)?;
    let mut body = Vec::with_capacity(header_len + sealed.len());
    body.extend_from_slice(salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(PUBLIC_KEY_LEN as u8);
    body.extend_from_slice(as_public);
    body.extend_from_slice(&sealed);
    Ok(body)
}

/// Encrypt `payload` (padded to `pad_to`) for one subscription with a fresh
/// server key and salt.
pub fn encrypt(
    ua_public: &[u8],
    auth: &[u8],
    payload: &[u8],
    pad_to: usize,
) -> Result<Vec<u8>, CryptoError> {
    if ua_public.len() != PUBLIC_KEY_LEN || auth.len() != AUTH_LEN {
        return Err(CryptoError::BadKey);
    }
    let rng = SystemRandom::new();
    let as_private = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng)
        .map_err(|_| CryptoError::Internal)?;
    let as_public = as_private
        .compute_public_key()
        .map_err(|_| CryptoError::Internal)?;
    let as_public = as_public.as_ref().to_vec();
    let peer = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, ua_public);
    let secret = agreement::agree_ephemeral(as_private, &peer, |s| Zeroizing::new(s.to_vec()))
        .map_err(|_| CryptoError::BadKey)?;
    let mut salt = [0u8; SALT_LEN];
    rng.fill(&mut salt).map_err(|_| CryptoError::Internal)?;
    encrypt_with(&secret, auth, ua_public, &as_public, &salt, payload, pad_to)
}

/// Whether `key` looks like a subscription's `p256dh` (65 bytes, `0x04`
/// prefix, a point on the curve). Checked at subscribe.
pub fn valid_ua_public(key: &[u8]) -> bool {
    if key.len() != PUBLIC_KEY_LEN || key[0] != 0x04 {
        return false;
    }
    // A throwaway agreement validates the point (ring rejects off-curve).
    let rng = SystemRandom::new();
    let Ok(k) = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng) else {
        return false;
    };
    agreement::agree_ephemeral(
        k,
        &agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, key),
        |_| (),
    )
    .is_ok()
}

/// Test-side decryption: what a browser does with the body. Used by the
/// round-trip test and the local end-to-end checker.
#[cfg(test)]
pub fn decrypt_with(
    ecdh_secret: &[u8],
    auth: &[u8],
    ua_public: &[u8],
    body: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let header_len = SALT_LEN + 4 + 1 + PUBLIC_KEY_LEN;
    if body.len() < header_len + TAG_LEN || body[20] as usize != PUBLIC_KEY_LEN {
        return Err(CryptoError::BadKey);
    }
    let salt = &body[..SALT_LEN];
    let as_public = &body[21..21 + PUBLIC_KEY_LEN];
    let keys = derive(ecdh_secret, auth, ua_public, as_public, salt)?;
    let cipher = Aes128Gcm::new_from_slice(&keys.cek[..]).map_err(|_| CryptoError::Internal)?;
    let mut plain = cipher
        .decrypt(Nonce::from_slice(&keys.nonce), &body[header_len..])
        .map_err(|_| CryptoError::Internal)?;
    while plain.last() == Some(&0) {
        plain.pop();
    }
    if plain.pop() != Some(0x02) {
        return Err(CryptoError::Internal);
    }
    Ok(plain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
    use base64::Engine as _;

    fn b(s: &str) -> Vec<u8> {
        B64.decode(s).unwrap()
    }

    /// RFC 8291 Appendix A, byte for byte.
    #[test]
    fn rfc8291_appendix_a() {
        let plaintext = b("V2hlbiBJIGdyb3cgdXAsIEkgd2FudCB0byBiZSBhIHdhdGVybWVsb24");
        let as_public = b("BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8");
        let ua_public = b("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4");
        let salt: [u8; 16] = b("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap();
        let auth = b("BTBZMqHH6r4Tts7J_aSIgg");
        let ecdh = b("kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs");

        let keys = derive(&ecdh, &auth, &ua_public, &as_public, &salt).unwrap();
        assert_eq!(B64.encode(&keys.cek[..]), "oIhVW04MRdy2XN9CiKLxTg");
        assert_eq!(B64.encode(keys.nonce), "4h_95klXJ5E_qnoN");

        let body =
            encrypt_with(&ecdh, &auth, &ua_public, &as_public, &salt, &plaintext, 0).unwrap();
        // Header = salt ‖ rs 4096 ‖ idlen 65 ‖ as_public (RFC 8291 A "Header").
        let expected = [
            salt.to_vec(),
            vec![0, 0, 0x10, 0, 65],
            as_public.clone(),
            b("8pfeW0KbunFT06SuDKoJH9Ql87S1QUrdirN6GcG7sFz1y1sqLgVi1VhjVkHsUoEsbI_0LpXMuGvnzQ"),
        ]
        .concat();
        assert_eq!(
            B64.encode(&expected[..86]),
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8"
        );
        assert_eq!(body, expected);
        assert_eq!(
            decrypt_with(&ecdh, &auth, &ua_public, &body).unwrap(),
            plaintext
        );
    }

    /// The sender's real path: a fresh server key, decrypted with the
    /// subscription's private key the way a browser would.
    #[test]
    fn round_trip_with_the_subscription_private_key() {
        let rng = SystemRandom::new();
        let ua_private =
            agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng).unwrap();
        let ua_public = ua_private.compute_public_key().unwrap().as_ref().to_vec();
        let mut auth = [0u8; 16];
        rng.fill(&mut auth).unwrap();
        assert!(valid_ua_public(&ua_public));

        let payload =
            crate::server::push::payload(crate::server::push::PushKind::Permission, "n:7").unwrap();
        let body = encrypt(
            &ua_public,
            &auth,
            &payload,
            crate::server::push::PAYLOAD_LEN,
        )
        .unwrap();
        // One record, the fixed size for every kind.
        assert_eq!(body.len(), 86 + crate::server::push::PAYLOAD_LEN + 16);

        let as_public = &body[21..86];
        let ecdh = agreement::agree_ephemeral(
            ua_private,
            &agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, as_public),
            |s| s.to_vec(),
        )
        .unwrap();
        let plain = decrypt_with(&ecdh, &auth, &ua_public, &body).unwrap();
        assert_eq!(plain, payload);
    }

    #[test]
    fn bad_keys_are_refused() {
        assert_eq!(
            encrypt(&[4u8; 10], &[0u8; 16], b"x", 0),
            Err(CryptoError::BadKey)
        );
        let mut not_on_curve = [7u8; 65];
        not_on_curve[0] = 4;
        assert!(!valid_ua_public(&not_on_curve));
        assert_eq!(
            encrypt(&not_on_curve, &[0u8; 16], b"x", 0),
            Err(CryptoError::BadKey)
        );
        assert_eq!(
            encrypt_with(&[0; 32], &[0; 16], &[4; 65], &[4; 65], &[0; 16], b"x", 5000),
            Err(CryptoError::TooLong)
        );
    }
}
