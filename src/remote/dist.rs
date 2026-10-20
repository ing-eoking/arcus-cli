use crate::server::build::extract;
use crate::server::fetch::{fetch_cached, Checksum, Request};
use crate::server::home::Home;
use std::io;
use std::path::{Path, PathBuf};

const RELEASE_BASE: &str = "https://github.com/ing-eoking/arcus-cli/releases/download";

pub fn asset_names(tag: &str, target: &str) -> (String, String) {
    let tarball = format!("arcus-cli-{tag}-{target}.tar.gz");
    let sha = format!("{tarball}.sha256");
    (tarball, sha)
}

/// First 64-hex token of a `shasum -a 256` output file (lowercased).
pub fn parse_sha256_file(text: &str) -> Option<String> {
    let tok = text.split_whitespace().next()?;
    (tok.len() == 64 && tok.bytes().all(|b| b.is_ascii_hexdigit())).then(|| tok.to_ascii_lowercase())
}

fn safe_component(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')) && !s.starts_with('.')
}

/// Downloads (or reuses the cache of) the release binary for `target`, verified by its
/// published SHA-256, cached at `dist/<tag>/<target>/arcus-cli` with mode 755.
pub fn fetch_binary(home: &Home, tag: &str, target: &str) -> io::Result<PathBuf> {
    if !safe_component(tag) || !safe_component(target) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid tag or target"));
    }
    let dir = home.dist_dir().join(tag).join(target);
    let bin = dir.join("arcus-cli");
    if bin.is_file() { return Ok(bin); }
    let (tarball, sha_name) = asset_names(tag, target);
    let url = |name: &str| Request { url: format!("{RELEASE_BASE}/{tag}/{name}"), headers: vec![] };
    let sha_path = fetch_cached(&url(&sha_name), &dir.join(&sha_name), None)?;
    let sha = std::fs::read_to_string(&sha_path).ok().and_then(|t| parse_sha256_file(&t));
    let Some(sha) = sha else {
        let _ = std::fs::remove_file(&sha_path);
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("invalid checksum file {sha_name}")));
    };
    let tar_path = fetch_cached(&url(&tarball), &dir.join(&tarball), Some(&Checksum::Sha256(sha)))?;
    let top = extract(&tar_path, &dir.join("extract"))?;
    std::fs::copy(top.join("arcus-cli"), &bin)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))?;
    }
    let _ = std::fs::remove_dir_all(dir.join("extract"));
    Ok(bin)
}

const NO_TAG_HINT: &str = "this arcus-cli build has no release tag; pass `binary` (a locally built Linux arcus-cli binary) or `tag` (a release tag such as v1.2.0)";

fn choose_with(local_tag: &str, tag_arg: Option<&str>, binary_arg: Option<&Path>) -> Result<(Option<PathBuf>, String), String> {
    if let Some(b) = binary_arg {
        if !b.is_file() { return Err(format!("binary not found: {}", b.display())); }
        return Ok((Some(b.to_path_buf()), "custom".into()));
    }
    if let Some(t) = tag_arg { return Ok((None, t.to_string())); }
    if local_tag != "dev" { return Ok((None, local_tag.to_string())); }
    Err(NO_TAG_HINT.into())
}

/// Picks the binary to ship: `binary_arg` (tag `custom`), else a release for `tag_arg`,
/// else a release matching this build's tag.
pub fn choose_binary(home: &Home, target: &str, tag_arg: Option<&str>, binary_arg: Option<&Path>) -> Result<(PathBuf, String), String> {
    match choose_with(crate::TAG, tag_arg, binary_arg)? {
        (Some(p), tag) => Ok((p, tag)),
        (None, tag) => fetch_binary(home, &tag, target)
            .map(|p| (p, tag))
            .map_err(|e| format!("could not fetch arcus-cli {target} release: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        let (t, s) = asset_names("v1.2.0", "aarch64-unknown-linux-gnu");
        assert_eq!(t, "arcus-cli-v1.2.0-aarch64-unknown-linux-gnu.tar.gz");
        assert_eq!(s, format!("{t}.sha256"));
    }
    #[test]
    fn sha_file() {
        let h = "a".repeat(64);
        assert_eq!(parse_sha256_file(&format!("{h}  x.tar.gz")), Some(h.clone()));
        assert_eq!(parse_sha256_file("nothex  x"), None);
        assert_eq!(parse_sha256_file(""), None);
    }
    #[test]
    fn dev_build_without_tag_or_binary_is_error() {
        let e = choose_with("dev", None, None).unwrap_err();
        assert!(e.contains("binary") && e.contains("tag"));
    }
    #[test]
    fn binary_arg_wins() {
        let f = std::env::temp_dir().join(format!("arcus-dist-bin-{}", std::process::id()));
        std::fs::write(&f, "x").unwrap();
        assert_eq!(choose_with("v1", Some("v2"), Some(&f)).unwrap(), (Some(f.clone()), "custom".into()));
        assert!(choose_with("v1", None, Some(Path::new("/nonexistent/arcus"))).is_err());
        let _ = std::fs::remove_file(f);
    }
    #[test]
    fn tag_arg_then_local_tag() {
        assert_eq!(choose_with("v1", Some("v2"), None).unwrap(), (None, "v2".into()));
        assert_eq!(choose_with("v1", None, None).unwrap(), (None, "v1".into()));
    }
    #[test]
    fn fetch_rejects_bad_components() {
        let root = std::env::temp_dir().join(format!("arcus-dist-home-{}", std::process::id()));
        let home = Home::new(Some(root.clone())).unwrap();
        assert!(fetch_binary(&home, "../x", "t").is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}
