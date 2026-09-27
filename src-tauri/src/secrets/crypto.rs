use std::fmt;

use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Nonce,
};
use argon2::{self, Config, Variant, Version};
use rand::{rngs::OsRng as RandOsRng, RngCore};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub const ENVELOPE_VERSION: u32 = 1;
pub const KDF_NAME: &str = "argon2id";
pub const DEK_LEN: usize = 32;
pub const SALT_LEN: usize = 16;
pub const NONCE_LEN: usize = 12;
pub const TAG_LEN: usize = 16;
pub const ARGON2_MEMORY_KIB: u32 = 19_456;
pub const ARGON2_ITERATIONS: u32 = 2;
pub const ARGON2_PARALLELISM: u32 = 1;
pub const MAX_ENVELOPE_BYTES: usize = 4_096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
}

impl KdfParams {
    pub fn supported() -> Self {
        Self {
            memory_kib: ARGON2_MEMORY_KIB,
            iterations: ARGON2_ITERATIONS,
            parallelism: ARGON2_PARALLELISM,
        }
    }

    fn validate(&self) -> Result<(), CryptoError> {
        if self.memory_kib != ARGON2_MEMORY_KIB
            || self.iterations != ARGON2_ITERATIONS
            || self.parallelism != ARGON2_PARALLELISM
        {
            return Err(CryptoError::UnsupportedKdfParameters);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrappedDekEnvelope {
    pub version: u32,
    pub kdf: String,
    pub params: KdfParams,
    pub salt: Vec<u8>,
    pub nonce: Vec<u8>,
    pub wrapped_dek: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CryptoError {
    InvalidPassphrase,
    InvalidEnvelope(&'static str),
    UnsupportedVersion(u32),
    UnsupportedKdf(String),
    UnsupportedKdfParameters,
    KeyDerivation(String),
    Encryption(String),
    Decryption,
    WrongPassphrase,
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPassphrase => f.write_str("passphrase must not be empty"),
            Self::InvalidEnvelope(message) => write!(f, "invalid secret envelope: {message}"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported secret envelope version {version}")
            }
            Self::UnsupportedKdf(kdf) => write!(f, "unsupported secret KDF `{kdf}`"),
            Self::UnsupportedKdfParameters => f.write_str("unsupported Argon2id parameters"),
            Self::KeyDerivation(message) => write!(f, "Argon2id key derivation failed: {message}"),
            Self::Encryption(message) => write!(f, "AES-256-GCM encryption failed: {message}"),
            Self::Decryption => f.write_str("AES-256-GCM decryption failed"),
            Self::WrongPassphrase => f.write_str("wrong passphrase"),
        }
    }
}

impl std::error::Error for CryptoError {}

pub fn random_nonce() -> [u8; NONCE_LEN] {
    let mut nonce = [0u8; NONCE_LEN];
    RandOsRng.fill_bytes(&mut nonce);
    nonce
}

pub fn random_dek() -> Zeroizing<[u8; DEK_LEN]> {
    let mut dek = [0u8; DEK_LEN];
    RandOsRng.fill_bytes(&mut dek);
    Zeroizing::new(dek)
}

pub fn wrap_dek(passphrase: &str, dek: &[u8; DEK_LEN]) -> Result<WrappedDekEnvelope, CryptoError> {
    validate_passphrase(passphrase)?;
    let mut salt = [0u8; SALT_LEN];
    RandOsRng.fill_bytes(&mut salt);
    let nonce = random_nonce();
    let kek = derive_kek(passphrase, &salt, &KdfParams::supported())?;
    let cipher = Aes256Gcm::new_from_slice(kek.as_ref())
        .map_err(|error| CryptoError::KeyDerivation(error.to_string()))?;
    let wrapped_dek = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &dek[..],
                aad: DEK_AAD,
            },
        )
        .map_err(|error| CryptoError::Encryption(error.to_string()))?;
    Ok(WrappedDekEnvelope {
        version: ENVELOPE_VERSION,
        kdf: KDF_NAME.to_string(),
        params: KdfParams::supported(),
        salt: salt.to_vec(),
        nonce: nonce.to_vec(),
        wrapped_dek,
    })
}

pub fn create_envelope(
    passphrase: &str,
) -> Result<(WrappedDekEnvelope, Zeroizing<[u8; DEK_LEN]>), CryptoError> {
    let dek = random_dek();
    let envelope = wrap_dek(passphrase, &dek)?;
    Ok((envelope, dek))
}

pub fn unwrap_dek(
    passphrase: &str,
    envelope: &WrappedDekEnvelope,
) -> Result<Zeroizing<[u8; DEK_LEN]>, CryptoError> {
    validate_passphrase(passphrase)?;
    envelope.validate()?;
    let kek = derive_kek(passphrase, &envelope.salt, &envelope.params)?;
    let cipher = Aes256Gcm::new_from_slice(kek.as_ref())
        .map_err(|error| CryptoError::KeyDerivation(error.to_string()))?;
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(
                Nonce::from_slice(&envelope.nonce),
                Payload {
                    msg: &envelope.wrapped_dek,
                    aad: DEK_AAD,
                },
            )
            .map_err(|_| CryptoError::WrongPassphrase)?,
    );
    if plaintext.len() != DEK_LEN {
        return Err(CryptoError::WrongPassphrase);
    }
    let mut dek = [0u8; DEK_LEN];
    dek.copy_from_slice(&plaintext[..]);
    Ok(Zeroizing::new(dek))
}

pub fn encrypt_value(
    dek: &[u8; DEK_LEN],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let cipher = Aes256Gcm::new_from_slice(dek)
        .map_err(|error| CryptoError::Encryption(error.to_string()))?;
    let nonce = random_nonce();
    cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map(|mut ciphertext| {
            let mut output = Vec::with_capacity(NONCE_LEN + ciphertext.len());
            output.extend_from_slice(&nonce);
            output.append(&mut ciphertext);
            output
        })
        .map_err(|error| CryptoError::Encryption(error.to_string()))
}

pub fn decrypt_value(
    dek: &[u8; DEK_LEN],
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    if ciphertext.len() < NONCE_LEN + TAG_LEN {
        return Err(CryptoError::Decryption);
    }
    let cipher = Aes256Gcm::new_from_slice(dek).map_err(|_| CryptoError::Decryption)?;
    let (nonce, body) = ciphertext.split_at(NONCE_LEN);
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce), Payload { msg: body, aad })
        .map_err(|_| CryptoError::Decryption)?;
    Ok(Zeroizing::new(plaintext))
}

impl WrappedDekEnvelope {
    pub fn validate(&self) -> Result<(), CryptoError> {
        if self.version != ENVELOPE_VERSION {
            return Err(CryptoError::UnsupportedVersion(self.version));
        }
        if self.kdf != KDF_NAME {
            return Err(CryptoError::UnsupportedKdf(self.kdf.clone()));
        }
        self.params.validate()?;
        if self.salt.len() != SALT_LEN {
            return Err(CryptoError::InvalidEnvelope("salt length"));
        }
        if self.nonce.len() != NONCE_LEN {
            return Err(CryptoError::InvalidEnvelope("nonce length"));
        }
        if self.wrapped_dek.len() != DEK_LEN + TAG_LEN {
            return Err(CryptoError::InvalidEnvelope("wrapped DEK length"));
        }
        Ok(())
    }

    pub fn to_json(&self) -> Result<Vec<u8>, CryptoError> {
        self.validate()?;
        let mut body =
            serde_json::to_vec(self).map_err(|_| CryptoError::InvalidEnvelope("serialization"))?;
        body.push(b'\n');
        Ok(body)
    }

    pub fn from_slice(body: &[u8]) -> Result<Self, CryptoError> {
        if body.len() > MAX_ENVELOPE_BYTES {
            return Err(CryptoError::InvalidEnvelope("size"));
        }
        let envelope: Self =
            serde_json::from_slice(body).map_err(|_| CryptoError::InvalidEnvelope("JSON"))?;
        envelope.validate()?;
        Ok(envelope)
    }
}

const DEK_AAD: &[u8] = b"ikenga:secrets:dek:v1";

fn validate_passphrase(passphrase: &str) -> Result<(), CryptoError> {
    if passphrase.is_empty() {
        return Err(CryptoError::InvalidPassphrase);
    }
    Ok(())
}

fn derive_kek(
    passphrase: &str,
    salt: &[u8],
    params: &KdfParams,
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    params.validate()?;
    if salt.len() != SALT_LEN {
        return Err(CryptoError::InvalidEnvelope("salt length"));
    }
    let config = Config {
        ad: &[],
        hash_length: DEK_LEN as u32,
        lanes: params.parallelism,
        mem_cost: params.memory_kib,
        secret: &[],
        time_cost: params.iterations,
        variant: Variant::Argon2id,
        version: Version::Version13,
    };
    let derived = argon2::hash_raw(passphrase.as_bytes(), salt, &config)
        .map_err(|error| CryptoError::KeyDerivation(error.to_string()))?;
    if derived.len() != DEK_LEN {
        return Err(CryptoError::KeyDerivation(
            "unexpected output length".to_string(),
        ));
    }
    Ok(Zeroizing::new(derived))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argon2id_wrap_and_unwrap_round_trip() {
        let (envelope, dek) = create_envelope("correct horse battery staple").unwrap();
        assert_eq!(envelope.version, ENVELOPE_VERSION);
        assert_eq!(envelope.kdf, KDF_NAME);
        assert_eq!(envelope.salt.len(), SALT_LEN);
        assert_eq!(envelope.nonce.len(), NONCE_LEN);
        assert_eq!(envelope.wrapped_dek.len(), DEK_LEN + TAG_LEN);
        assert_eq!(
            &*unwrap_dek("correct horse battery staple", &envelope).unwrap(),
            &*dek
        );
    }

    #[test]
    fn envelope_json_round_trip_preserves_version_and_lengths() {
        let (envelope, _) = create_envelope("passphrase").unwrap();
        let body = envelope.to_json().unwrap();
        let parsed = WrappedDekEnvelope::from_slice(&body).unwrap();
        assert_eq!(parsed, envelope);
        assert_eq!(parsed.version, ENVELOPE_VERSION);
        assert_eq!(parsed.nonce.len(), NONCE_LEN);
    }

    #[test]
    fn wrong_passphrase_is_typed_and_does_not_return_a_dek() {
        let (envelope, _) = create_envelope("right").unwrap();
        assert_eq!(
            unwrap_dek("wrong", &envelope).unwrap_err(),
            CryptoError::WrongPassphrase
        );
    }

    #[test]
    fn wrap_generates_a_fresh_nonce_and_salt() {
        let (first, _) = create_envelope("same").unwrap();
        let (second, _) = create_envelope("same").unwrap();
        assert_ne!(first.nonce, second.nonce);
        assert_ne!(first.salt, second.salt);
    }

    #[test]
    fn value_nonce_is_random_and_authenticated() {
        let dek = random_dek();
        let first = encrypt_value(&dek, b"secret", b"name").unwrap();
        let second = encrypt_value(&dek, b"secret", b"name").unwrap();
        assert_ne!(first, second);
        assert_eq!(&*decrypt_value(&dek, &first, b"name").unwrap(), b"secret");
        assert!(decrypt_value(&dek, &first, b"other").is_err());
    }

    #[test]
    fn envelope_rejects_unknown_version_and_parameters() {
        let (mut envelope, _) = create_envelope("passphrase").unwrap();
        envelope.version += 1;
        assert!(matches!(
            envelope.validate(),
            Err(CryptoError::UnsupportedVersion(_))
        ));
        let (mut envelope, _) = create_envelope("passphrase").unwrap();
        envelope.params.iterations += 1;
        assert_eq!(
            envelope.validate().unwrap_err(),
            CryptoError::UnsupportedKdfParameters
        );
    }
}
