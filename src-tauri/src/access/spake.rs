//! The SPAKE2 pairing handshake, host side (G-ACCESS §3.4, P-7), and the
//! key-confirmation / fingerprint derivations (§3.5, P-8).
//!
//! * **Construction:** RustCrypto `spake2 =0.4.0`, `Ed25519Group`, the host
//!   is side **B** (`start_b`); the device (browser) is side A, ported in
//!   `src/lib/access/spake2.ts` on `@noble/curves` and checked against the
//!   same fixed vectors (`testdata/spake2_vectors.json`, A-13).
//! * **Password:** `b"ikenga-pair-v1/" ‖ normalized 6-symbol code` (the
//!   slot included: it binds the session but adds no secrecy, §3.2).
//! * **Identities:** `idA = b"ikenga-pair-v1/device"`,
//!   `idB = b"ikenga-pair-v1/host/" ‖ store_id`, so one host's transcripts
//!   never verify against another's.
//! * **Messages** are 33 bytes (`side ‖ point`), base64url without padding
//!   on the wire. The transcript `T = SHA-256(msgA ‖ msgB)` is over the full
//!   33-byte messages.
//! * **Derived** from the 32-byte key `K`, `salt = pairing_id` (ASCII):
//!   `K_confirm`, `poll_key` (32 bytes each), `fp_seed` (16 bytes) by
//!   HKDF-SHA256, and `host_confirm` / `device_confirm` =
//!   `HMAC-SHA256(K_confirm, "host"|"device" ‖ T)`.
//!
//! Nothing here is logged; the key material zeroizes on drop.

use base64::Engine as _;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use zeroize::Zeroize;

pub const PW_PREFIX: &[u8] = b"ikenga-pair-v1/";
pub const ID_A: &[u8] = b"ikenga-pair-v1/device";
pub const ID_B_PREFIX: &[u8] = b"ikenga-pair-v1/host/";
pub const INFO_CONFIRM: &[u8] = b"ikenga-pair-v1 confirm";
pub const INFO_POLL: &[u8] = b"ikenga-pair-v1 poll";
pub const INFO_FINGERPRINT: &[u8] = b"ikenga-pair-v1 fingerprint";
/// `side ‖ compressed point`.
pub const MSG_LEN: usize = 33;

/// A handshake refusal. The HTTP layer maps every one to the uniform
/// `404 pair_failed` (§3.7) — the variants are for tests and logs only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpakeError {
    /// Not base64url, wrong length, wrong side byte, or not a curve point.
    BadMessage,
}

pub fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn unb64(s: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s.trim())
        .ok()
}

/// The SPAKE2 password bytes for a **normalized** code (§3.2).
pub fn password_bytes(normalized_code: &str) -> Vec<u8> {
    let mut pw = PW_PREFIX.to_vec();
    pw.extend_from_slice(normalized_code.as_bytes());
    pw
}

pub fn id_b(store_id: &str) -> Vec<u8> {
    let mut id = ID_B_PREFIX.to_vec();
    id.extend_from_slice(store_id.as_bytes());
    id
}

/// Everything §3.5 derives from `K`. Zeroized on drop.
#[derive(Clone)]
pub struct Keys {
    k_confirm: [u8; 32],
    poll_key: [u8; 32],
    fp_seed: [u8; 16],
    transcript: [u8; 32],
}

impl Drop for Keys {
    fn drop(&mut self) {
        self.k_confirm.zeroize();
        self.poll_key.zeroize();
        self.fp_seed.zeroize();
        self.transcript.zeroize();
    }
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Keys(..)")
    }
}

type HmacSha256 = Hmac<Sha256>;

impl Keys {
    /// §3.5 from the SPAKE2 key, the pairing id (salt) and both 33-byte
    /// messages.
    pub fn derive(key: &[u8], pairing_id: &str, msg_a: &[u8], msg_b: &[u8]) -> Self {
        let hk = Hkdf::<Sha256>::new(Some(pairing_id.as_bytes()), key);
        let mut k_confirm = [0u8; 32];
        let mut poll_key = [0u8; 32];
        let mut fp_seed = [0u8; 16];
        // Lengths ≤ 255·32: expand cannot fail.
        hk.expand(INFO_CONFIRM, &mut k_confirm)
            .expect("hkdf length");
        hk.expand(INFO_POLL, &mut poll_key).expect("hkdf length");
        hk.expand(INFO_FINGERPRINT, &mut fp_seed)
            .expect("hkdf length");
        let mut t = Sha256::new();
        t.update(msg_a);
        t.update(msg_b);
        Keys {
            k_confirm,
            poll_key,
            fp_seed,
            transcript: t.finalize().into(),
        }
    }

    fn mac(&self, label: &[u8]) -> HmacSha256 {
        let mut m = HmacSha256::new_from_slice(&self.k_confirm).expect("hmac accepts any key");
        m.update(label);
        m.update(&self.transcript);
        m
    }

    /// `HMAC-SHA256(K_confirm, "host" ‖ T)` — sent to the device with msgB.
    pub fn host_confirm(&self) -> [u8; 32] {
        self.mac(b"host").finalize().into_bytes().into()
    }

    /// `HMAC-SHA256(K_confirm, "device" ‖ T)`.
    pub fn device_confirm(&self) -> [u8; 32] {
        self.mac(b"device").finalize().into_bytes().into()
    }

    /// Constant-time check of the device's confirm.
    pub fn verify_device_confirm(&self, presented: &[u8]) -> bool {
        self.mac(b"device").verify_slice(presented).is_ok()
    }

    /// Constant-time check of an `X-Ikenga-Pair-Poll` value.
    pub fn verify_poll_key(&self, presented: &[u8]) -> bool {
        presented.len() == self.poll_key.len()
            && presented
                .iter()
                .zip(self.poll_key.iter())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0
    }

    pub fn poll_key(&self) -> &[u8; 32] {
        &self.poll_key
    }

    pub fn fp_seed(&self) -> &[u8; 16] {
        &self.fp_seed
    }

    pub fn transcript(&self) -> &[u8; 32] {
        &self.transcript
    }

    pub fn k_confirm(&self) -> &[u8; 32] {
        &self.k_confirm
    }

    /// The 4 fingerprint words both sides show (§3.5).
    pub fn fingerprint(&self) -> [&'static str; 4] {
        super::fingerprint::phrase(&self.fp_seed)
    }
}

/// What the host answers a `hello` with.
#[derive(Debug)]
pub struct HostReply {
    /// The host's 33-byte message (`0x42 ‖ Y`).
    pub msg_b: Vec<u8>,
    pub keys: Keys,
}

fn check_msg_a(msg_a: &[u8]) -> Result<(), SpakeError> {
    if msg_a.len() != MSG_LEN || msg_a[0] != 0x41 {
        return Err(SpakeError::BadMessage);
    }
    Ok(())
}

/// The host side of one exchange (`start_b` with the OS RNG, then
/// `finish(msgA)`), plus the §3.5 derivations.
pub fn host_reply(
    normalized_code: &str,
    store_id: &str,
    pairing_id: &str,
    msg_a: &[u8],
) -> Result<HostReply, SpakeError> {
    host_reply_with_rng(
        normalized_code,
        store_id,
        pairing_id,
        msg_a,
        rand::rngs::OsRng,
    )
}

/// [`host_reply`] with a caller-chosen RNG (the fixed vectors, A-13).
pub fn host_reply_with_rng(
    normalized_code: &str,
    store_id: &str,
    pairing_id: &str,
    msg_a: &[u8],
    rng: impl rand::CryptoRng + rand::RngCore,
) -> Result<HostReply, SpakeError> {
    check_msg_a(msg_a)?;
    let mut pw = password_bytes(normalized_code);
    let (state, msg_b) = Spake2::<Ed25519Group>::start_b_with_rng(
        &Password::new(&pw),
        &Identity::new(ID_A),
        &Identity::new(&id_b(store_id)),
        rng,
    );
    pw.zeroize();
    let mut key = state.finish(msg_a).map_err(|_| SpakeError::BadMessage)?;
    let keys = Keys::derive(&key, pairing_id, msg_a, &msg_b);
    key.zeroize();
    Ok(HostReply { msg_b, keys })
}

/// The device side, for tests and the fixed vectors: what the browser port
/// does (`start_a`, then `finish(msgB)`).
#[cfg(test)]
pub fn device_start_with_rng(
    normalized_code: &str,
    store_id: &str,
    rng: impl rand::CryptoRng + rand::RngCore,
) -> (Spake2<Ed25519Group>, Vec<u8>) {
    Spake2::<Ed25519Group>::start_a_with_rng(
        &Password::new(password_bytes(normalized_code)),
        &Identity::new(ID_A),
        &Identity::new(&id_b(store_id)),
        rng,
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use rand::{CryptoRng, RngCore};
    use serde_json::{json, Value};
    use std::path::PathBuf;

    /// A deterministic "RNG" that hands out a fixed byte stream (the
    /// `Scalar::random` draw is 64 bytes, reduced mod ℓ).
    pub(crate) struct FixedRng(pub Vec<u8>, pub usize);

    impl RngCore for FixedRng {
        fn next_u32(&mut self) -> u32 {
            let mut b = [0u8; 4];
            self.fill_bytes(&mut b);
            u32::from_le_bytes(b)
        }
        fn next_u64(&mut self) -> u64 {
            let mut b = [0u8; 8];
            self.fill_bytes(&mut b);
            u64::from_le_bytes(b)
        }
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            for d in dest {
                *d = self.0[self.1 % self.0.len()];
                self.1 += 1;
            }
        }
        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand::Error> {
            self.fill_bytes(dest);
            Ok(())
        }
    }
    impl CryptoRng for FixedRng {}

    /// 64 bytes derived from a label (the vectors' RNG draws).
    fn draw(label: &str) -> Vec<u8> {
        let mut out = Sha256::digest(format!("{label}/0").as_bytes()).to_vec();
        out.extend_from_slice(&Sha256::digest(format!("{label}/1").as_bytes()));
        out
    }

    const CASES: &[(&str, &str, &str)] = &[
        // (code, store_id, pairing_id)
        (
            "K7P42Q",
            "0192f3c4-7d1e-7a00-8b00-000000000001",
            "0192f3c4-7d1e-7a00-8b00-0000000000aa",
        ),
        (
            "0ZZ9AB",
            "0192f3c4-7d1e-7a00-8b00-000000000002",
            "0192f3c4-7d1e-7a00-8b00-0000000000bb",
        ),
    ];

    /// Render `testdata/spake2_vectors.json` (A-13, A-14): fixed scalars,
    /// both messages, the key, every §3.5 derivation and the 4 words.
    pub(crate) fn render_vectors() -> String {
        let mut vectors = Vec::new();
        for (i, (code, store_id, pairing_id)) in CASES.iter().enumerate() {
            let draw_a = draw(&format!("ikenga-spake2-vector-{i}/a"));
            let draw_b = draw(&format!("ikenga-spake2-vector-{i}/b"));
            let (a, msg_a) = device_start_with_rng(code, store_id, FixedRng(draw_a.clone(), 0));
            let (b, msg_b) = Spake2::<Ed25519Group>::start_b_with_rng(
                &Password::new(password_bytes(code)),
                &Identity::new(ID_A),
                &Identity::new(&id_b(store_id)),
                FixedRng(draw_b.clone(), 0),
            );
            let key_a = a.finish(&msg_b).unwrap();
            let key_b = b.finish(&msg_a).unwrap();
            assert_eq!(key_a, key_b);
            let keys = Keys::derive(&key_a, pairing_id, &msg_a, &msg_b);
            vectors.push(json!({
                "code": code,
                "password": hex::encode(password_bytes(code)),
                "storeId": store_id,
                "idA": hex::encode(ID_A),
                "idB": hex::encode(id_b(store_id)),
                "pairingId": pairing_id,
                "rngA": hex::encode(&draw_a),
                "rngB": hex::encode(&draw_b),
                "msgA": hex::encode(&msg_a),
                "msgB": hex::encode(&msg_b),
                "key": hex::encode(&key_a),
                "kConfirm": hex::encode(keys.k_confirm()),
                "pollKey": hex::encode(keys.poll_key()),
                "fpSeed": hex::encode(keys.fp_seed()),
                "transcript": hex::encode(keys.transcript()),
                "hostConfirm": hex::encode(keys.host_confirm()),
                "deviceConfirm": hex::encode(keys.device_confirm()),
                "fingerprint": keys.fingerprint(),
            }));
        }
        let doc: Value = json!({
            "_comment": "GENERATED by src-tauri/src/access/spake.rs tests — do not edit (G-ACCESS §3.4 A-13, §3.5 A-14). rngA/rngB are the 64-byte Scalar::random draws (read little-endian, reduced mod l).",
            "construction": "spake2 =0.4.0 Ed25519Group; host = side B; pw = 'ikenga-pair-v1/' || code",
            "wordlistSha256": super::super::fingerprint::WORDLIST_SHA256,
            "vectors": vectors,
        });
        let mut s = serde_json::to_string_pretty(&doc).unwrap();
        s.push('\n');
        s
    }

    fn vectors_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/access/testdata/spake2_vectors.json")
    }

    /// A-13 (Rust half): the checked-in vectors equal the render. On a
    /// mismatch the message carries the whole file;
    /// `IKENGA_UPDATE_GENERATED=1` rewrites it.
    #[test]
    fn spake2_vectors_are_current() {
        let expected = render_vectors();
        let path = vectors_path();
        if std::env::var_os("IKENGA_UPDATE_GENERATED").is_some_and(|v| v == "1") {
            std::fs::write(&path, &expected).unwrap();
            return;
        }
        let actual = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            actual == expected,
            "{} is stale. Regenerate with `IKENGA_UPDATE_GENERATED=1 cargo test --lib \
             access::spake`, or replace it with exactly:\n\
             ----- BEGIN spake2_vectors.json -----\n{expected}----- END spake2_vectors.json -----",
            path.display()
        );
    }

    /// The pinned constants match `spake2-0.4.0/src/ed25519.rs` (§3.4: the
    /// TS port copies them; a wrong one fails A-13).
    #[test]
    fn host_reply_matches_the_vectors_and_the_device_side() {
        let doc: Value = serde_json::from_str(&render_vectors()).unwrap();
        for v in doc["vectors"].as_array().unwrap() {
            let code = v["code"].as_str().unwrap();
            let store = v["storeId"].as_str().unwrap();
            let pid = v["pairingId"].as_str().unwrap();
            let msg_a = hex::decode(v["msgA"].as_str().unwrap()).unwrap();
            let rng_b = hex::decode(v["rngB"].as_str().unwrap()).unwrap();
            let reply = host_reply_with_rng(code, store, pid, &msg_a, FixedRng(rng_b, 0)).unwrap();
            assert_eq!(hex::encode(&reply.msg_b), v["msgB"].as_str().unwrap());
            assert_eq!(
                hex::encode(reply.keys.host_confirm()),
                v["hostConfirm"].as_str().unwrap()
            );
            let dc = hex::decode(v["deviceConfirm"].as_str().unwrap()).unwrap();
            assert!(reply.keys.verify_device_confirm(&dc));
            assert!(!reply.keys.verify_device_confirm(&reply.keys.host_confirm()));
            let poll = hex::decode(v["pollKey"].as_str().unwrap()).unwrap();
            assert!(reply.keys.verify_poll_key(&poll));
            assert!(!reply.keys.verify_poll_key(&poll[..31]));
        }
    }

    /// A live exchange with the OS RNG: same code → same keys and words; a
    /// different code (or another host's store id) → different words and a
    /// failing confirm.
    #[test]
    fn a_wrong_code_or_host_changes_the_words_and_fails_confirm() {
        let store = "0192f3c4-0000-7000-8000-000000000001";
        let (a, msg_a) = device_start_with_rng("K7P42Q", store, rand::rngs::OsRng);
        let host = host_reply("K7P42Q", store, "p1", &msg_a).unwrap();
        let key = a.finish(&host.msg_b).unwrap();
        let dev = Keys::derive(&key, "p1", &msg_a, &host.msg_b);
        assert_eq!(dev.fingerprint(), host.keys.fingerprint());
        assert!(host.keys.verify_device_confirm(&dev.device_confirm()));
        assert_eq!(dev.host_confirm(), host.keys.host_confirm());

        for (code, store_b) in [("K7P42R", store), ("K7P42Q", "another-store")] {
            let (a, msg_a) = device_start_with_rng("K7P42Q", store, rand::rngs::OsRng);
            let host = host_reply(code, store_b, "p1", &msg_a).unwrap();
            let key = a.finish(&host.msg_b).unwrap();
            let dev = Keys::derive(&key, "p1", &msg_a, &host.msg_b);
            assert_ne!(dev.fingerprint(), host.keys.fingerprint());
            assert!(!host.keys.verify_device_confirm(&dev.device_confirm()));
        }
    }

    #[test]
    fn malformed_messages_are_refused() {
        let store = "s";
        let (_, mut msg_a) = device_start_with_rng("K7P42Q", store, rand::rngs::OsRng);
        assert!(host_reply("K7P42Q", store, "p", &msg_a[..32]).is_err());
        msg_a[0] = 0x42;
        assert_eq!(
            host_reply("K7P42Q", store, "p", &msg_a).unwrap_err(),
            SpakeError::BadMessage
        );
        assert_eq!(unb64("not base64!"), None);
        assert_eq!(unb64(&b64(&[1, 2, 3])), Some(vec![1, 2, 3]));
    }
}
