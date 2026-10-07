//! VAPID (RFC 8292): the server's push identity.
//!
//! One ECDSA P-256 key per store owner, created once and kept in
//! `vapid.json` (`{"v":1,"pkcs8":"<base64>"}`):
//!
//! * T0: `<data-dir>/push/vapid.json` (the data dir is fenced off from the
//!   fs RPCs by `server::reserved`);
//! * T1: `<root>/operator/push/vapid.json` (root-only).
//!
//! The directory is `0700`, the file `0600`, created `O_CREAT | O_EXCL |
//! O_NOFOLLOW`. A key file that is a symlink, has group/other bits, or is
//! owned by another uid is refused: push is then disabled (fail closed) and
//! the daemon still starts. The private key is held in [`Zeroizing`] memory
//! behind a redacting `Debug`, and neither it nor a JWT is ever logged.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// JWT lifetime (RFC 8292 caps it at 24 h).
pub const JWT_TTL_SECS: i64 = 12 * 60 * 60;
/// A cached JWT is reused until this long before it expires.
const JWT_REFRESH_SECS: i64 = 60 * 60;
/// The `sub` claim when nothing better is configured.
pub const DEFAULT_CONTACT: &str = "https://ikenga.dev";

pub const FILE: &str = "vapid.json";

/// The server's VAPID key pair.
pub struct Vapid {
    pkcs8: Zeroizing<Vec<u8>>,
    key: EcdsaKeyPair,
    public: Vec<u8>,
    contact: String,
    cache: Mutex<HashMap<String, (String, i64)>>,
}

impl std::fmt::Debug for Vapid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vapid")
            .field("key_id", &self.key_id())
            .field("private", &"<redacted>")
            .finish()
    }
}

/// Why a key file was refused (push is then off).
#[derive(Debug)]
pub struct KeyRefused(pub String);

impl std::fmt::Display for KeyRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Vapid {
    /// A key from PKCS#8 bytes.
    pub fn from_pkcs8(pkcs8: Zeroizing<Vec<u8>>, contact: String) -> Result<Self, KeyRefused> {
        let key = EcdsaKeyPair::from_pkcs8(
            &ECDSA_P256_SHA256_FIXED_SIGNING,
            &pkcs8,
            &SystemRandom::new(),
        )
        .map_err(|_| KeyRefused("the VAPID key file does not hold a P-256 key".into()))?;
        let public = key.public_key().as_ref().to_vec();
        Ok(Self {
            pkcs8,
            key,
            public,
            contact,
            cache: Mutex::new(HashMap::new()),
        })
    }

    /// A fresh key (tests, and first boot via [`load_or_create`]).
    pub fn generate(contact: String) -> Result<Self, KeyRefused> {
        let doc =
            EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &SystemRandom::new())
                .map_err(|_| KeyRefused("could not generate a VAPID key".into()))?;
        Self::from_pkcs8(Zeroizing::new(doc.as_ref().to_vec()), contact)
    }

    /// The 65-byte uncompressed public point.
    pub fn public_key(&self) -> &[u8] {
        &self.public
    }

    /// `applicationServerKey`, base64url.
    pub fn public_key_b64(&self) -> String {
        URL_SAFE_NO_PAD.encode(&self.public)
    }

    /// `sha256(public)[..8]` as 16 hex chars: what a subscription records,
    /// so a key change is detectable.
    pub fn key_id(&self) -> String {
        hex::encode(&Sha256::digest(&self.public)[..8])
    }

    pub fn contact(&self) -> &str {
        &self.contact
    }

    /// The JWT for one push-service origin, cached until an hour before it
    /// expires.
    pub fn jwt(&self, audience: &str, now_secs: i64) -> Result<String, KeyRefused> {
        {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((jwt, exp)) = cache.get(audience) {
                if now_secs < exp - JWT_REFRESH_SECS {
                    return Ok(jwt.clone());
                }
            }
        }
        let exp = now_secs + JWT_TTL_SECS;
        let header = URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = serde_json::json!({ "aud": audience, "exp": exp, "sub": self.contact });
        let claims = URL_SAFE_NO_PAD.encode(claims.to_string());
        let signing_input = format!("{header}.{claims}");
        let sig = self
            .key
            .sign(&SystemRandom::new(), signing_input.as_bytes())
            .map_err(|_| KeyRefused("VAPID signing failed".into()))?;
        let jwt = format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(sig.as_ref()));
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() > 64 {
            cache.clear();
        }
        cache.insert(audience.to_string(), (jwt.clone(), exp));
        Ok(jwt)
    }

    /// The `Authorization` header value: `vapid t=<jwt>, k=<public>`.
    pub fn authorization(&self, audience: &str, now_secs: i64) -> Result<String, KeyRefused> {
        Ok(format!(
            "vapid t={}, k={}",
            self.jwt(audience, now_secs)?,
            self.public_key_b64()
        ))
    }

    fn file_json(&self) -> Zeroizing<String> {
        Zeroizing::new(
            serde_json::json!({ "v": 1, "pkcs8": STANDARD.encode(self.pkcs8.as_slice()) })
                .to_string(),
        )
    }
}

/// The VAPID `sub` claim: `--push-contact`, else an https `--public-url`,
/// else [`DEFAULT_CONTACT`]. A bare address becomes `mailto:`.
pub fn contact_for(configured: Option<&str>, public_url: Option<&str>) -> String {
    if let Some(c) = configured.map(str::trim).filter(|c| !c.is_empty()) {
        if c.starts_with("mailto:") || c.starts_with("https://") {
            return c.to_string();
        }
        if c.contains('@') {
            return format!("mailto:{c}");
        }
    }
    if let Some(u) = public_url
        .map(str::trim)
        .filter(|u| u.starts_with("https://"))
    {
        return u.trim_end_matches('/').to_string();
    }
    DEFAULT_CONTACT.to_string()
}

/// `<dir>/push/vapid.json`'s path.
pub fn path_in(dir: &Path) -> PathBuf {
    dir.join("push").join(FILE)
}

/// Load the key at `path`, creating it (and its `0700` directory) on first
/// use. Refuses a symlink, a group/other-readable file or directory, or one
/// owned by another uid.
pub fn load_or_create(path: &Path, contact: String) -> Result<Vapid, KeyRefused> {
    let dir = path
        .parent()
        .ok_or_else(|| KeyRefused("VAPID key path has no directory".into()))?;
    ensure_private_dir(dir)?;
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            check_private(path, &meta, false)?;
            let raw = Zeroizing::new(
                read_nofollow(path).map_err(|e| KeyRefused(format!("{}: {e}", path.display())))?,
            );
            let v: serde_json::Value = serde_json::from_slice(&raw)
                .map_err(|_| KeyRefused(format!("{}: not JSON", path.display())))?;
            let b64 = v
                .get("pkcs8")
                .and_then(|p| p.as_str())
                .filter(|_| v.get("v").and_then(|x| x.as_i64()) == Some(1))
                .ok_or_else(|| KeyRefused(format!("{}: unknown format", path.display())))?;
            let pkcs8 = Zeroizing::new(
                STANDARD
                    .decode(b64)
                    .map_err(|_| KeyRefused(format!("{}: bad key encoding", path.display())))?,
            );
            Vapid::from_pkcs8(pkcs8, contact)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let vapid = Vapid::generate(contact)?;
            write_new(path, vapid.file_json().as_bytes())
                .map_err(|e| KeyRefused(format!("{}: {e}", path.display())))?;
            tracing::info!(
                "push: created a VAPID key (id {}) at {}",
                vapid.key_id(),
                path.display()
            );
            Ok(vapid)
        }
        Err(e) => Err(KeyRefused(format!("{}: {e}", path.display()))),
    }
}

#[cfg(unix)]
fn check_private(path: &Path, meta: &std::fs::Metadata, dir: bool) -> Result<(), KeyRefused> {
    use std::os::unix::fs::MetadataExt;
    if meta.file_type().is_symlink() {
        return Err(KeyRefused(format!("{} is a symlink", path.display())));
    }
    if dir != meta.is_dir() || (!dir && !meta.is_file()) {
        return Err(KeyRefused(format!(
            "{} is not a regular {}",
            path.display(),
            if dir { "directory" } else { "file" }
        )));
    }
    // SAFETY: geteuid has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    if meta.uid() != euid {
        return Err(KeyRefused(format!(
            "{} is owned by uid {}, not {euid}",
            path.display(),
            meta.uid()
        )));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(KeyRefused(format!(
            "{} has mode {:o}; it must be private to its owner",
            path.display(),
            meta.mode() & 0o777
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_private(path: &Path, meta: &std::fs::Metadata, _dir: bool) -> Result<(), KeyRefused> {
    if meta.file_type().is_symlink() {
        return Err(KeyRefused(format!("{} is a symlink", path.display())));
    }
    Ok(())
}

fn ensure_private_dir(dir: &Path) -> Result<(), KeyRefused> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) => check_private(dir, &meta, true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut b = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                b.mode(0o700);
            }
            b.create(dir)
                .map_err(|e| KeyRefused(format!("{}: {e}", dir.display())))?;
            let meta = std::fs::symlink_metadata(dir)
                .map_err(|e| KeyRefused(format!("{}: {e}", dir.display())))?;
            check_private(dir, &meta, true)
        }
        Err(e) => Err(KeyRefused(format!("{}: {e}", dir.display()))),
    }
}

fn read_nofollow(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let mut f = opts.open(path)?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(buf)
}

fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut f = opts.open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{UnparsedPublicKey, ECDSA_P256_SHA256_FIXED};

    fn claims(jwt: &str) -> serde_json::Value {
        let part = jwt.split('.').nth(1).unwrap();
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap()
    }

    /// RFC 8292: ES256 header, `aud` = the push origin, `exp` ≤ 24 h, a
    /// `sub`, and a raw r‖s signature that verifies under `k`.
    #[test]
    fn jwt_claims_and_signature() {
        let v = Vapid::generate("mailto:ops@example.com".into()).unwrap();
        let now = 1_700_000_000;
        let jwt = v.jwt("https://fcm.googleapis.com", now).unwrap();
        let parts: Vec<_> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header, serde_json::json!({"typ": "JWT", "alg": "ES256"}));
        let c = claims(&jwt);
        assert_eq!(c["aud"], "https://fcm.googleapis.com");
        assert_eq!(c["sub"], "mailto:ops@example.com");
        assert_eq!(c["exp"], now + JWT_TTL_SECS);
        assert!(JWT_TTL_SECS <= 24 * 3600);
        assert_eq!(c.as_object().unwrap().len(), 3);
        let sig = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        assert_eq!(sig.len(), 64);
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, v.public_key())
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig)
            .unwrap();
        assert_eq!(v.public_key().len(), 65);
        assert_eq!(v.public_key()[0], 4);

        // Cached per audience until an hour before expiry.
        assert_eq!(v.jwt("https://fcm.googleapis.com", now + 60).unwrap(), jwt);
        let later = now + JWT_TTL_SECS - JWT_REFRESH_SECS + 1;
        assert_ne!(v.jwt("https://fcm.googleapis.com", later).unwrap(), jwt);
        assert_ne!(v.jwt("https://web.push.apple.com", now).unwrap(), jwt);

        let auth = v.authorization("https://web.push.apple.com", now).unwrap();
        assert!(auth.starts_with("vapid t="));
        assert!(auth.ends_with(&format!(", k={}", v.public_key_b64())));
        assert!(!format!("{v:?}").contains(&STANDARD.encode(v.pkcs8.as_slice())));
    }

    #[test]
    fn contact_precedence() {
        assert_eq!(contact_for(Some("ops@x.io"), None), "mailto:ops@x.io");
        assert_eq!(contact_for(Some("https://x.io"), None), "https://x.io");
        assert_eq!(
            contact_for(None, Some("https://ik.example/")),
            "https://ik.example"
        );
        assert_eq!(contact_for(None, Some("http://lan:4000")), DEFAULT_CONTACT);
        assert_eq!(contact_for(Some("  "), None), DEFAULT_CONTACT);
    }

    #[cfg(unix)]
    #[test]
    fn key_file_is_created_private_and_reloaded() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = path_in(tmp.path());
        let a = load_or_create(&path, DEFAULT_CONTACT.into()).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        let b = load_or_create(&path, DEFAULT_CONTACT.into()).unwrap();
        assert_eq!(a.public_key(), b.public_key());
        assert_eq!(a.key_id().len(), 16);

        // A loosened file is refused (push off), never "fixed".
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_or_create(&path, DEFAULT_CONTACT.into()).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        // A symlink in place of the file is refused.
        let tmp2 = tempfile::tempdir().unwrap();
        let link = path_in(tmp2.path());
        std::fs::create_dir(link.parent().unwrap()).unwrap();
        std::fs::set_permissions(
            link.parent().unwrap(),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        std::os::unix::fs::symlink(&path, &link).unwrap();
        let err = load_or_create(&link, DEFAULT_CONTACT.into()).unwrap_err();
        assert!(err.0.contains("symlink"), "{err}");
    }
}
