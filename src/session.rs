//! Keeping the HN login between runs. What's kept is HN's session cookie,
//! never the password: in the system's keyring (Keychain on macOS,
//! Credential Manager on Windows, Secret Service on Linux) or, where there
//! isn't one, in `~/.lshn/session`, encrypted with a passphrase.

use chacha20poly1305::aead::{Aead, Generate, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use std::path::{Path, PathBuf};

const SERVICE: &str = "lshn";
const ACCOUNT: &str = "news.ycombinator.com";
/// The start of an encrypted session file, and its format's version.
const MAGIC: &[u8] = b"lshn-session-1\n";
const SALT: usize = 16;
const NONCE: usize = 24;

/// Where a saved session is.
#[derive(Debug, PartialEq, Eq)]
pub enum Saved {
    /// In the keyring: the cookie.
    Keyring(String),
    /// In the encrypted file, which needs the passphrase.
    File,
    Nothing,
}

fn file(dir: &Path) -> PathBuf {
    dir.join("session")
}

fn entry() -> Option<keyring::Entry> {
    keyring::Entry::new(SERVICE, ACCOUNT).ok()
}

/// The session saved last time, if any.
pub fn load(dir: Option<&Path>) -> Saved {
    if let Some(cookie) = entry().and_then(|e| e.get_password().ok()) {
        return Saved::Keyring(cookie);
    }
    match dir {
        Some(dir) if file(dir).is_file() => Saved::File,
        _ => Saved::Nothing,
    }
}

/// Saves the cookie in the keyring. Errs if there isn't one, or it won't
/// take it: then it's [`save_file`]'s, with a passphrase.
pub fn save_keyring(cookie: &str) -> Result<(), String> {
    let entry = entry().ok_or("no keyring")?;
    entry.set_password(cookie).map_err(|e| e.to_string())
}

/// Saves the cookie encrypted with `passphrase`, readable only by you.
pub fn save_file(dir: &Path, cookie: &str, passphrase: &str) -> Result<(), String> {
    let bytes = encrypt(cookie.as_bytes(), passphrase)?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = file(dir);
    let tmp = path.with_extension("tmp");
    write_private(&tmp, &bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

/// The cookie in the encrypted file.
pub fn open_file(dir: &Path, passphrase: &str) -> Result<String, String> {
    let bytes = std::fs::read(file(dir)).map_err(|e| e.to_string())?;
    let plain = decrypt(&bytes, passphrase)?;
    String::from_utf8(plain).map_err(|_| "the session file is damaged".into())
}

/// Forgets the session, wherever it was.
pub fn forget(dir: Option<&Path>) {
    if let Some(entry) = entry() {
        let _ = entry.delete_credential();
    }
    if let Some(dir) = dir {
        let _ = std::fs::remove_file(file(dir));
    }
}

/// The key for `passphrase` and `salt`, with Argon2id: slow on purpose, so
/// guessing passphrases is too.
fn key(passphrase: &str, salt: &[u8]) -> Result<chacha20poly1305::Key, String> {
    let mut key = chacha20poly1305::Key::default();
    argon2::Argon2::default()
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|e| e.to_string())?;
    Ok(key)
}

fn encrypt(plain: &[u8], passphrase: &str) -> Result<Vec<u8>, String> {
    let mut salt = [0u8; SALT];
    getrandom::fill(&mut salt).map_err(|e| e.to_string())?;
    let cipher = XChaCha20Poly1305::new(&key(passphrase, &salt)?);
    let nonce = XNonce::generate();
    let sealed = cipher
        .encrypt(&nonce, plain)
        .map_err(|_| "couldn't encrypt".to_string())?;
    Ok([MAGIC, &salt, nonce.as_slice(), &sealed].concat())
}

fn decrypt(bytes: &[u8], passphrase: &str) -> Result<Vec<u8>, String> {
    let rest = bytes
        .strip_prefix(MAGIC)
        .filter(|r| r.len() > SALT + NONCE)
        .ok_or("the session file is damaged")?;
    let (salt, rest) = rest.split_at(SALT);
    let (nonce, sealed) = rest.split_at(NONCE);
    let nonce = XNonce::try_from(nonce).map_err(|_| "the session file is damaged")?;
    let cipher = XChaCha20Poly1305::new(&key(passphrase, salt)?);
    cipher
        .decrypt(&nonce, sealed)
        .map_err(|_| "wrong passphrase".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypts_so_only_the_passphrase_opens_it() {
        let sealed = encrypt(b"user=pg&abc", "correct horse").unwrap();
        assert!(sealed.starts_with(MAGIC));
        assert!(!sealed.windows(3).any(|w| w == b"abc"));
        assert_eq!(decrypt(&sealed, "correct horse").unwrap(), b"user=pg&abc");
        assert_eq!(decrypt(&sealed, "wrong").unwrap_err(), "wrong passphrase");
        // The same thing twice comes out different: a fresh salt and nonce.
        assert_ne!(sealed, encrypt(b"user=pg&abc", "correct horse").unwrap());
        assert!(decrypt(b"nonsense", "x").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_yours_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("lshn-session-{}", std::process::id()));
        save_file(&dir, "user=pg&abc", "pass").unwrap();
        let mode = std::fs::metadata(file(&dir)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(open_file(&dir, "pass").unwrap(), "user=pg&abc");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
