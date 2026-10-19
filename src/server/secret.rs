//! Generated secrets kept out of tool results: random passwords and 0600 files.
use ring::rand::{SecureRandom, SystemRandom};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
const PASSWORD_LEN: usize = 24;

/// 24 alphanumeric characters from the system CSPRNG (rejection sampling, no modulo bias).
pub fn random_password() -> String {
    let rng = SystemRandom::new();
    let limit = (256 / ALPHABET.len() * ALPHABET.len()) as u8; /* 248 */
    let mut out = String::with_capacity(PASSWORD_LEN);
    let mut buf = [0u8; 32];
    while out.len() < PASSWORD_LEN {
        rng.fill(&mut buf).expect("secure RNG failure");
        for &b in buf.iter().filter(|&&b| b < limit) {
            if out.len() < PASSWORD_LEN { out.push(ALPHABET[b as usize % ALPHABET.len()] as char); }
        }
    }
    out
}

/// Writes `value` to `path` (creating parent directories) with mode 0600.
pub fn write_secret(path: &Path, value: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() { std::fs::create_dir_all(dir)?; }
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    f.write_all(value.as_bytes())?;
    f.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn secret_file_mode() {
        let dir = std::env::temp_dir().join(format!("arcus-secret-{}", std::process::id()));
        let path = dir.join("nested").join("s");
        write_secret(&path, "pw").unwrap();
        write_secret(&path, "pw2").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "pw2");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn password_is_24_alphanumeric_and_random() {
        let a = random_password();
        assert_eq!(a.len(), 24);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(a, random_password());
    }
}
