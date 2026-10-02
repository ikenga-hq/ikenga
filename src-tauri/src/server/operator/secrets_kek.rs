//! The master KEK for every principal's secret store (remote-access WP-21,
//! founder decision DEC-R18-1: a server-held KEK envelope).
//!
//! `operator/secrets-kek` — 32 random bytes, hex, `root:root 0600`, inside
//! the `0700` root-only `operator/` (G-PRINCIPAL §4). Created the first time
//! a T1 broker launches a principal child, and from then on only read. The
//! broker derives one wrapping key per principal from it
//! ([`WrapKey::derive`], HKDF-SHA256 with the principal id as info) and hands
//! a child **only its own** derived key (`server::broker::children`). The
//! KEK itself never leaves this process: it is not in any child's
//! environment, argv or files, and no principal uid can open the file.
//!
//! Losing the file loses every principal's stored secrets (the operator
//! defaults are unaffected). Back it up with `operator/`. Rotation is a
//! rewrap of each principal's envelope
//! ([`crate::secrets::PrincipalStore::rewrap`]); the operator command that
//! drives it is deferred.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use rand::RngCore;
use zeroize::Zeroizing;

use crate::executor::PrincipalId;
use crate::secrets::principal_store::{WrapKey, KEY_LEN};

/// Under `operator/`.
pub const KEK_FILENAME: &str = "secrets-kek";
const HEADER: &str = "ikenga-secrets-kek:v1:";

/// The master KEK, held by the broker. Never `Debug`-printed, never cloned
/// out of this type.
pub struct SecretsKek {
    key: Zeroizing<[u8; KEY_LEN]>,
}

impl std::fmt::Debug for SecretsKek {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretsKek(<redacted>)")
    }
}

/// Whether the file must be root-owned. Production always enforces; unit
/// tests that run unprivileged skip only the owner check (the mode is still
/// checked).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KekOwner {
    Root,
    #[cfg(test)]
    Any,
}

impl SecretsKek {
    pub fn path(operator_dir: &Path) -> PathBuf {
        operator_dir.join(KEK_FILENAME)
    }

    /// Read `operator/secrets-kek`, creating it on first use. Refuses (never
    /// regenerates) a file that is a symlink, not regular, readable by group
    /// or other, not root-owned, or malformed: a silently re-minted KEK
    /// would orphan every principal's store.
    pub fn load_or_create(operator_dir: &Path, owner: KekOwner) -> io::Result<Self> {
        let path = Self::path(operator_dir);
        match Self::load(&path, owner) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            other => return other,
        }
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        rand::rngs::OsRng.fill_bytes(&mut key[..]);
        // Written whole to a private temp file, then linked into place: the
        // link fails if another launch won the race, and a crash never leaves
        // a short KEK file behind.
        let tmp = operator_dir.join(format!(
            ".{KEK_FILENAME}.tmp-{}-{}",
            std::process::id(),
            hex::encode(&{
                let mut n = [0u8; 8];
                rand::rngs::OsRng.fill_bytes(&mut n);
                n
            })
        ));
        {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&tmp)?;
            let body = Zeroizing::new(format!("{HEADER}{}\n", hex::encode(&key[..])));
            let written = file
                .write_all(body.as_bytes())
                .and_then(|()| file.sync_all());
            if let Err(e) = written {
                let _ = fs::remove_file(&tmp);
                return Err(e);
            }
        }
        let linked = fs::hard_link(&tmp, &path);
        let _ = fs::remove_file(&tmp);
        match linked {
            Ok(()) => {
                if let Ok(dir) = fs::File::open(operator_dir) {
                    let _ = dir.sync_all();
                }
                tracing::info!("secrets: created the operator KEK at {}", path.display());
                Ok(Self { key })
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Self::load(&path, owner),
            Err(e) => Err(e),
        }
    }

    fn load(path: &Path, owner: KekOwner) -> io::Result<Self> {
        let invalid = |why: String| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {why}", path.display()),
            )
        };
        let mut file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(invalid("not a regular file".into()));
        }
        if meta.mode() & 0o077 != 0 {
            return Err(invalid(format!(
                "mode {:o} is open to group or other (expected 0600)",
                meta.mode() & 0o7777
            )));
        }
        if owner == KekOwner::Root && (meta.uid() != 0 || meta.gid() != 0) {
            return Err(invalid(format!(
                "owned by {}:{}, not root:root",
                meta.uid(),
                meta.gid()
            )));
        }
        let mut text = Zeroizing::new(String::new());
        (&mut file).take(256).read_to_string(&mut text)?;
        let hex = text
            .trim_end()
            .strip_prefix(HEADER)
            .ok_or_else(|| invalid("unrecognised format".into()))?;
        let bytes = Zeroizing::new(hex::decode(hex).map_err(|_| invalid("not hex".into()))?);
        if bytes.len() != KEY_LEN {
            return Err(invalid(format!("not {KEY_LEN} bytes")));
        }
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        key.copy_from_slice(&bytes);
        Ok(Self { key })
    }

    /// The wrapping key for one principal's store — the only key material a
    /// child ever receives.
    pub fn wrap_key_for(&self, principal: PrincipalId) -> WrapKey {
        WrapKey::derive(&self.key, &principal.to_string())
            .expect("a PrincipalId's text form is a valid store principal")
    }

    /// The hex form a test can search a child's environment for.
    #[cfg(test)]
    pub(crate) fn hex_for_tests(&self) -> String {
        hex::encode(&self.key[..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn operator_dir() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        tmp
    }

    #[test]
    fn created_once_0600_then_reloaded_identically() {
        let dir = operator_dir();
        let first = SecretsKek::load_or_create(dir.path(), KekOwner::Any).unwrap();
        let path = SecretsKek::path(dir.path());
        let meta = fs::symlink_metadata(&path).unwrap();
        assert!(meta.is_file());
        assert_eq!(meta.mode() & 0o7777, 0o600);
        let second = SecretsKek::load_or_create(dir.path(), KekOwner::Any).unwrap();
        assert_eq!(*first.key, *second.key, "never re-minted");
        // No temp file left behind.
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from(KEK_FILENAME)]);
        assert!(!format!("{first:?}").contains(&first.hex_for_tests()));
    }

    #[test]
    fn an_unsafe_or_malformed_kek_is_refused_never_regenerated() {
        let dir = operator_dir();
        let path = SecretsKek::path(dir.path());
        let kek = SecretsKek::load_or_create(dir.path(), KekOwner::Any).unwrap();
        let body = fs::read(&path).unwrap();

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let err = SecretsKek::load_or_create(dir.path(), KekOwner::Any).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        fs::write(&path, "ikenga-secrets-kek:v1:abcd\n").unwrap();
        assert!(SecretsKek::load_or_create(dir.path(), KekOwner::Any).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"ikenga-secrets-kek:v1:abcd\n");

        fs::remove_file(&path).unwrap();
        let elsewhere = dir.path().join("elsewhere");
        fs::write(&elsewhere, &body).unwrap();
        fs::set_permissions(&elsewhere, fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
        assert!(SecretsKek::load_or_create(dir.path(), KekOwner::Any).is_err());

        // Unprivileged, a root-owner check always fails.
        if unsafe { libc::geteuid() } != 0 {
            fs::remove_file(&path).unwrap();
            fs::rename(&elsewhere, &path).unwrap();
            assert!(SecretsKek::load_or_create(dir.path(), KekOwner::Root).is_err());
        }
        drop(kek);
    }

    #[test]
    fn each_principal_gets_a_distinct_key_that_is_not_the_kek() {
        let dir = operator_dir();
        let kek = SecretsKek::load_or_create(dir.path(), KekOwner::Any).unwrap();
        let (a, b) = (PrincipalId::new_v7(), PrincipalId::new_v7());
        let ka = kek.wrap_key_for(a);
        let kb = kek.wrap_key_for(b);
        assert_eq!(ka.principal(), a.to_string());
        let (ea, eb) = (ka.to_env_value(), kb.to_env_value());
        assert_ne!(ea.split(':').nth(2), eb.split(':').nth(2));
        assert!(!ea.contains(&kek.hex_for_tests()));
        assert_eq!(
            kek.wrap_key_for(a).to_env_value().as_str(),
            ea.as_str(),
            "deterministic: the same principal reopens its store after a broker restart"
        );
    }

    /// Real uids. `#[ignore]`d; the `t1-root` CI job runs them as root with
    /// only the T1 capabilities, from a world-traversable `/opt/t1` copy of
    /// this test binary (`--ignored --test-threads=1 t1_root`).
    mod t1_root {
        use super::*;
        use std::ffi::OsString;
        use std::os::unix::fs::MetadataExt;

        use crate::executor::t1::tests::t1_root::{home_for, require_root};
        use crate::executor::t1::tests::{config, piped, principal};
        use crate::executor::t1::T1Executor;
        use crate::executor::{Principal, SpawnSpec};
        use crate::secrets::principal_store::{PrincipalStore, WRAP_KEY_ENV};
        use crate::secrets::SecretsStore;
        use crate::server::broker::children::T1Launcher;
        use crate::server::operator::{OperatorRoot, Ownership};

        const ENTRY: &str =
            "server::operator::secrets_kek::tests::t1_root::t1_root_secrets_child_entry";
        const OWN_DATA: &str = "IKENGA_T1ROOT_SECRETS_DATA";
        const OTHER_DATA: &str = "IKENGA_T1ROOT_SECRETS_OTHER";
        const KEK_PATH: &str = "IKENGA_T1ROOT_SECRETS_KEK";

        fn environ_hex() -> String {
            hex::encode(fs::read("/proc/self/environ").unwrap_or_default())
        }

        /// The child side, run as a principal's uid through the real T1
        /// executor with the broker's real `host_env`. Prints its environment
        /// block before and after taking the key; exits non-zero on the first
        /// failed check, naming it on stderr.
        #[test]
        #[ignore = "t1-root (secrets child entry)"]
        fn t1_root_secrets_child_entry() {
            let Some(own) = std::env::var_os(OWN_DATA) else {
                return;
            };
            let fail = |code: i32, why: String| -> ! {
                eprintln!("secrets child: {why}");
                std::process::exit(code)
            };
            println!("BEFORE:{}", environ_hex());
            let key = match WrapKey::take_from_env() {
                Some(Ok(key)) => key,
                Some(Err(e)) => fail(10, e),
                None => fail(11, format!("no {WRAP_KEY_ENV}")),
            };
            if std::env::var_os(WRAP_KEY_ENV).is_some() {
                fail(12, "the wrapping key is still in the environment".into());
            }
            println!("AFTER:{}", environ_hex());
            // The master KEK is out of reach (operator/ is root 0700).
            let kek = std::env::var_os(KEK_PATH).unwrap();
            if fs::File::open(&kek).is_ok() {
                fail(13, "a principal opened the operator KEK".into());
            }
            let store = PrincipalStore::open(Path::new(&own), &key)
                .unwrap_or_else(|e| fail(14, format!("open own store: {e}")));
            store
                .set("WHO", key.principal())
                .unwrap_or_else(|e| fail(15, format!("set: {e}")));
            if store.get("WHO").ok().flatten().as_deref() != Some(key.principal()) {
                fail(16, "own value did not read back".into());
            }
            // Another principal's store is closed to this uid by the kernel.
            let other = PathBuf::from(std::env::var_os(OTHER_DATA).unwrap());
            for path in [
                other.join("secrets"),
                other.join("secrets/envelope.json"),
                other.join("secrets/values.json"),
            ] {
                if fs::read(&path).is_ok() || fs::read_dir(&path).is_ok() {
                    fail(17, format!("read {}", path.display()));
                }
            }
            std::process::exit(0);
        }

        fn data_for(tmp: &Path, name: &str, uid: u32) -> PathBuf {
            let dir = tmp.join(name);
            fs::create_dir(&dir).unwrap();
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
            std::os::unix::fs::lchown(&dir, Some(uid), Some(uid)).unwrap();
            dir
        }

        async fn run_child(
            exec: &T1Executor,
            p: &Principal,
            kek: &SecretsKek,
            own: &Path,
            other: &Path,
            kek_path: &Path,
        ) -> (String, String) {
            let mut spec = SpawnSpec::new(std::env::current_exe().unwrap());
            spec.args(
                [
                    "--exact",
                    ENTRY,
                    "--ignored",
                    "--test-threads=1",
                    "--nocapture",
                    "-q",
                ]
                .map(OsString::from),
            )
            .principal(Some(p.clone()))
            .env(OWN_DATA, own)
            .env(OTHER_DATA, other)
            .env(KEK_PATH, kek_path)
            // The deny floor drops this from a spec: only `host_env` passes it.
            .env(WRAP_KEY_ENV, "v1:smuggled:00");
            let host = T1Launcher::host_env("per-child-token", &kek.wrap_key_for(p.id));
            let out = exec
                .spawn_piped_with_host_env(spec, piped(), &host)
                .unwrap()
                .wait_with_output()
                .await
                .unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            assert!(
                out.status.success(),
                "secrets child {} failed ({:?}): {}\n{stdout}",
                p.uid,
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            );
            let line = |tag: &str| {
                let hex = stdout
                    .lines()
                    .find_map(|l| l.strip_prefix(tag))
                    .unwrap_or_else(|| panic!("no {tag} line: {stdout}"));
                String::from_utf8_lossy(&hex::decode(hex).unwrap()).into_owned()
            };
            (line("BEFORE:"), line("AFTER:"))
        }

        /// WP-21: two principals' stores are isolated by uid and by key; the
        /// master KEK is root 0600 and never in a child's environment; the
        /// wrapping key is gone from the child's environment block after
        /// startup.
        #[tokio::test]
        #[ignore = "t1-root"]
        async fn t1_root_principal_stores_are_isolated_and_the_kek_stays_root() {
            require_root();
            let (ua, ub) = (28_550, 28_551);
            let (_ta, home_a) = home_for(ua);
            let (_tb, home_b) = home_for(ub);
            let (a, b) = (principal(ua, &home_a), principal(ub, &home_b));

            let tmp = tempfile::tempdir().unwrap();
            fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o755)).unwrap();
            let root = OperatorRoot::new(tmp.path().join("root")).unwrap();
            root.prepare(Ownership::Enforce).unwrap();
            let kek = SecretsKek::load_or_create(&root.operator_dir(), KekOwner::Root).unwrap();
            let kek_path = SecretsKek::path(&root.operator_dir());
            let meta = fs::symlink_metadata(&kek_path).unwrap();
            assert!(meta.is_file());
            assert_eq!(
                (meta.uid(), meta.gid(), meta.mode() & 0o7777),
                (0, 0, 0o600),
                "operator/secrets-kek is root:root 0600"
            );
            let kek_hex = kek.hex_for_tests();

            let data_a = data_for(tmp.path(), "a", ua);
            let data_b = data_for(tmp.path(), "b", ub);
            let exec = T1Executor::new(config());
            for (p, own, other) in [(&a, &data_a, &data_b), (&b, &data_b, &data_a)] {
                let (before, after) = run_child(&exec, p, &kek, own, other, &kek_path).await;
                let own_key = kek.wrap_key_for(p.id).to_env_value();
                assert!(
                    before.contains(own_key.as_str()),
                    "the broker handed over its key"
                );
                assert!(!before.contains("v1:smuggled"), "a spec can't set the key");
                assert!(!before.contains(&kek_hex), "never the master KEK");
                let other_id = if p.id == a.id { b.id } else { a.id };
                let other_key = kek.wrap_key_for(other_id).to_env_value();
                let other_hex = other_key.rsplit(':').next().unwrap();
                assert!(!before.contains(other_hex), "never another principal's key");
                let own_hex = own_key.rsplit(':').next().unwrap();
                assert!(
                    !after.contains(own_hex),
                    "scrubbed from /proc/<pid>/environ"
                );
                assert!(!after.contains(&kek_hex));
            }

            // §4 / I-9: each store is its uid's, 0700 / 0600.
            for (data, uid) in [(&data_a, ua), (&data_b, ub)] {
                for (path, mode) in [
                    (data.join("secrets"), 0o700),
                    (data.join("secrets/envelope.json"), 0o600),
                    (data.join("secrets/values.json"), 0o600),
                ] {
                    let m = fs::symlink_metadata(&path).unwrap();
                    assert_eq!(
                        (m.uid(), m.mode() & 0o7777),
                        (uid, mode),
                        "{}",
                        path.display()
                    );
                }
            }

            // Key isolation, even for root holding the files: Bob's key never
            // opens Ada's store, and each opens with its own.
            let err = PrincipalStore::open(&data_a, &kek.wrap_key_for(b.id)).unwrap_err();
            assert!(err.is_invalid(), "{err}");
            let ada = PrincipalStore::open(&data_a, &kek.wrap_key_for(a.id)).unwrap();
            assert_eq!(ada.get("WHO").unwrap(), Some(a.id.to_string()));
            let bob = PrincipalStore::open(&data_b, &kek.wrap_key_for(b.id)).unwrap();
            assert_eq!(bob.get("WHO").unwrap(), Some(b.id.to_string()));
        }
    }
}
