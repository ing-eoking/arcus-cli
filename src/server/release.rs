//! ARCUS release listing, source download, and the GitHub CLI (`gh`) browser login used for EE access.

use super::features::Edition;
use super::fetch::{fetch_cached, get_json, Checksum, Request};
use super::home::Home;
use super::jobs::JobCtx;
use serde_json::{json, Value};
use std::fmt;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

pub const PUBLIC_REPO: &str = "naver/arcus-memcached";
pub const EE_REPO: &str = "jam2in/arcus-memcached-EE";
pub const DEVICE_URL: &str = "https://github.com/login/device";
const GH_REPO: &str = "cli/cli";
const CODE_WAIT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub asset_name: String,
    pub asset_api_url: String,
    pub sha256: Option<String>,
}

/// GitHub token; never printed.
#[derive(Clone)]
pub struct Token(String);

impl Token {
    fn bearer(&self) -> String { format!("Bearer {}", self.0) }
}
impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("***") }
}
impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("***") }
}

fn semver(s: &str) -> Option<(u64, u64, u64)> {
    let mut it = s.split('.');
    let t = (it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next()?.parse().ok()?);
    if it.next().is_some() { return None; }
    Some(t)
}

/// Releases of one edition, newest first.
pub fn parse_releases(edition: Edition, json: &Value) -> Vec<Release> {
    let mut found: Vec<((u64, u64, u64), Release)> = Vec::new();
    for rel in json.as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let Some(tag) = rel["tag_name"].as_str() else { continue };
        let version = match edition {
            Edition::Public => tag,
            Edition::Ee => match tag.strip_suffix("-E") { Some(v) => v, None => continue },
        };
        let Some(sv) = semver(version) else { continue };
        if edition == Edition::Public && sv < (1, 15, 0) { continue; }
        let want = match edition {
            Edition::Public => format!("arcus-memcached-{version}.tar.gz"),
            Edition::Ee => format!("arcus-memcached-EE-{version}.tar.gz"),
        };
        let Some(asset) = rel["assets"].as_array().and_then(|a| a.iter().find(|a| a["name"] == want.as_str())) else { continue };
        let Some(url) = asset["url"].as_str() else { continue };
        let sha256 = asset["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).map(str::to_string);
        found.push((sv, Release { version: version.to_string(), asset_name: want, asset_api_url: url.to_string(), sha256 }));
    }
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().map(|(_, r)| r).collect()
}

fn api_request(url: String, token: Option<&Token>, accept: &str) -> Request {
    let mut headers = vec![("Accept".to_string(), accept.to_string())];
    if let Some(t) = token { headers.push(("Authorization".to_string(), t.bearer())); }
    Request { url, headers }
}

pub fn list_releases(edition: Edition, token: Option<&Token>) -> io::Result<Vec<Release>> {
    let repo = if edition == Edition::Ee { EE_REPO } else { PUBLIC_REPO };
    let token = if edition == Edition::Ee { token } else { None };
    let req = api_request(format!("https://api.github.com/repos/{repo}/releases?per_page=100"), token,
        "application/vnd.github+json");
    Ok(parse_releases(edition, &get_json(&req)?))
}

pub fn download_source(rel: &Release, token: Option<&Token>, home: &Home) -> io::Result<PathBuf> {
    let req = api_request(rel.asset_api_url.clone(), token, "application/octet-stream");
    let sum = rel.sha256.clone().map(Checksum::Sha256);
    fetch_cached(&req, &home.downloads().join(&rel.asset_name), sum.as_ref())
}

/// `GITHUB_TOKEN`, then `<gh> auth token` (read-only).
pub fn find_token(gh: Option<&Path>) -> Option<Token> {
    if let Some(t) = std::env::var("GITHUB_TOKEN").ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) {
        return Some(Token(t));
    }
    let out = Command::new(gh?).args(["auth", "token"]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    if !out.status.success() { return None; }
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if t.is_empty() { None } else { Some(Token(t)) }
}

pub fn find_gh(home: &Home) -> Option<PathBuf> {
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let p = dir.join("gh");
            if p.is_file() { return Some(p); }
        }
    }
    let p = home.gh_dir().join("bin").join("gh");
    p.is_file().then_some(p)
}

/// Release asset name of the GitHub CLI for `std::env::consts` OS/ARCH values.
pub fn gh_asset_name(os: &str, arch: &str, version: &str) -> String {
    let arch = if arch == "aarch64" { "arm64" } else { "amd64" };
    if os == "macos" { format!("gh_{version}_macOS_{arch}.zip") } else { format!("gh_{version}_linux_{arch}.tar.gz") }
}

fn bad(msg: impl Into<String>) -> io::Error { io::Error::new(io::ErrorKind::InvalidData, msg.into()) }

/// Downloads the latest GitHub CLI into `gh_dir()` (checksum verified) and returns `bin/gh`.
pub fn download_gh(home: &Home) -> io::Result<PathBuf> {
    let rel = get_json(&api_request(format!("https://api.github.com/repos/{GH_REPO}/releases/latest"), None,
        "application/vnd.github+json"))?;
    let version = rel["tag_name"].as_str().ok_or_else(|| bad("gh release has no tag"))?.trim_start_matches('v').to_string();
    let name = gh_asset_name(std::env::consts::OS, std::env::consts::ARCH, &version);
    let asset_url = |n: &str| rel["assets"].as_array()
        .and_then(|a| a.iter().find(|a| a["name"] == n))
        .and_then(|a| a["browser_download_url"].as_str()).map(str::to_string);
    let url = asset_url(&name).ok_or_else(|| bad(format!("gh release {version} has no asset {name}")))?;
    let sums_name = format!("gh_{version}_checksums.txt");
    let sums_url = asset_url(&sums_name).ok_or_else(|| bad(format!("gh release {version} has no {sums_name}")))?;
    let dl = home.downloads();
    let sums_path = fetch_cached(&Request { url: sums_url, headers: vec![] }, &dl.join(&sums_name), None)?;
    let sums = std::fs::read_to_string(&sums_path)?;
    let hex = sums.lines().filter_map(|l| { let mut p = l.split_whitespace(); Some((p.next()?, p.next()?)) })
        .find(|(_, n)| n.trim_start_matches('*') == name).map(|(h, _)| h.to_string())
        .ok_or_else(|| bad(format!("{sums_name} has no entry for {name}")))?;
    let archive = fetch_cached(&Request { url, headers: vec![] }, &dl.join(&name), Some(&Checksum::Sha256(hex)))?;
    extract_gh(&archive, &home.gh_dir())?;
    let bin = home.gh_dir().join("bin").join("gh");
    if bin.is_file() { Ok(bin) } else { Err(bad("gh archive has no bin/gh")) }
}

/// Extracts `<top>/bin/gh` from the archive into `<dest>/bin/gh` (the top directory is stripped).
fn extract_gh(archive: &Path, dest: &Path) -> io::Result<()> {
    if archive.extension().is_some_and(|e| e == "zip") {
        let tmp = dest.join(".extract");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp)?;
        // The system `unzip` is present on macOS; avoids a zip crate dependency.
        let st = Command::new("unzip").arg("-q").arg("-o").arg(archive).arg("-d").arg(&tmp).status()?;
        if !st.success() { return Err(io::Error::other("unzip failed")); }
        for e in std::fs::read_dir(&tmp)? {
            let bin = e?.path().join("bin");
            if bin.is_dir() {
                let _ = std::fs::remove_dir_all(dest.join("bin"));
                std::fs::rename(&bin, dest.join("bin"))?;
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);
        return Ok(());
    }
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(std::fs::File::open(archive)?));
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let comps: Vec<_> = path.components().collect();
        if comps.len() < 2 || !comps.iter().all(|c| matches!(c, std::path::Component::Normal(_))) { continue; }
        let rel: PathBuf = comps[1..].iter().collect();
        if rel != Path::new("bin/gh") { continue; }
        let target = dest.join(&rel);
        std::fs::create_dir_all(target.parent().unwrap())?;
        entry.unpack(&target)?;
    }
    Ok(())
}

/// The `XXXX-XXXX` one-time code from gh's login output.
pub fn parse_device_code(text: &str) -> Option<String> {
    for line in text.lines() {
        let Some((_, rest)) = line.split_once("one-time code:") else { continue };
        let code = rest.trim();
        let ok = code.len() == 9 && code.chars().enumerate()
            .all(|(i, c)| if i == 4 { c == '-' } else { c.is_ascii_alphanumeric() });
        if ok { return Some(code.to_string()); }
    }
    None
}

/// `EeLogin` job: `gh auth login --web`, publishing the device code as soon as gh prints it.
pub fn ee_login_job(ctx: &JobCtx, _params: &Value) -> Result<Value, String> {
    let gh = find_gh(&ctx.home).ok_or("gh is not installed")?;
    ctx.step("starting gh");
    let log = std::fs::OpenOptions::new().create(true).append(true).open(&ctx.log).map_err(|e| e.to_string())?;
    let mut child = Command::new(&gh)
        .args(["auth", "login", "--hostname", "github.com", "--web", "--git-protocol", "https"])
        .env("GH_BROWSER", "true")
        .env_remove("GITHUB_TOKEN").env_remove("GH_TOKEN")
        .stdin(Stdio::piped()).stdout(log.try_clone().map_err(|e| e.to_string())?).stderr(Stdio::piped())
        .spawn().map_err(|e| format!("failed to run gh: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() { let _ = stdin.write_all(b"\n"); }
    let stderr = child.stderr.take().ok_or("no stderr")?;
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let mut log_w = log;
    std::thread::spawn(move || {
        for line in io::BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = writeln!(log_w, "{line}");
            if tx.send(line).is_err() { break; }
        }
    });
    ctx.step("waiting for code");
    let deadline = std::time::Instant::now() + CODE_WAIT;
    let mut code = None;
    while code.is_none() {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(left) {
            Ok(line) => code = parse_device_code(&line),
            Err(_) => break,
        }
    }
    let Some(code) = code else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("gh did not print a device code".into());
    };
    ctx.publish(json!({"code": code, "url": DEVICE_URL}));
    ctx.step("waiting for browser approval");
    let status = child.wait().map_err(|e| e.to_string())?;
    if !status.success() { return Err(format!("gh auth login failed ({status})")); }
    ctx.step("verifying access");
    let token = find_token(Some(&gh)).ok_or("login finished but no token is available")?;
    let versions = list_releases(Edition::Ee, Some(&token)).map_err(|e| format!("EE access check failed: {e}"))?;
    Ok(json!({"code": code, "url": DEVICE_URL, "ee_versions": versions.len()}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rel(tag: &str, asset: &str, digest: Option<&str>) -> Value {
        let mut a = json!({"name": asset, "url": format!("https://api.github.com/a/{asset}")});
        if let Some(d) = digest { a["digest"] = json!(d); }
        json!({"tag_name": tag, "assets": [a]})
    }

    #[test]
    fn public_filter() {
        let j = json!([
            rel("1.14.1", "arcus-memcached-1.14.1.tar.gz", None),
            rel("1.16.1", "arcus-memcached-1.16.1.tar.gz", Some("sha256:ab")),
        ]);
        let r = parse_releases(Edition::Public, &j);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].version, "1.16.1");
        assert_eq!(r[0].sha256.as_deref(), Some("ab"));
    }

    #[test]
    fn newest_first() {
        let j = json!([
            rel("1.15.0", "arcus-memcached-1.15.0.tar.gz", None),
            rel("1.16.10", "arcus-memcached-1.16.10.tar.gz", None),
            rel("1.16.2", "arcus-memcached-1.16.2.tar.gz", None),
        ]);
        let v: Vec<_> = parse_releases(Edition::Public, &j).into_iter().map(|r| r.version).collect();
        assert_eq!(v, ["1.16.10", "1.16.2", "1.15.0"]);
    }

    #[test]
    fn ee_tag_asset() {
        let j = json!([
            rel("0.12.1-E", "arcus-memcached-EE-0.12.1.tar.gz", Some("sha256:cd")),
            rel("0.11.0", "arcus-memcached-EE-0.11.0.tar.gz", None),
            rel("0.10.0-E", "arcus-memcached-0.10.0-E.tar.gz", None),
        ]);
        let r = parse_releases(Edition::Ee, &j);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].version, "0.12.1");
        assert_eq!(r[0].asset_name, "arcus-memcached-EE-0.12.1.tar.gz");
    }

    #[test]
    fn device_code_parsed() {
        assert_eq!(parse_device_code("! First copy your one-time code: 1A2B-3C4D\nOpen this URL..."), Some("1A2B-3C4D".into()));
        assert_eq!(parse_device_code("nothing"), None);
    }

    #[test]
    fn token_masked() {
        assert_eq!(format!("{:?} {}", Token("s".into()), Token("s".into())), "*** ***");
    }

    #[test]
    fn gh_asset_names() {
        assert_eq!(gh_asset_name("linux", "aarch64", "2.60.0"), "gh_2.60.0_linux_arm64.tar.gz");
        assert_eq!(gh_asset_name("linux", "x86_64", "2.60.0"), "gh_2.60.0_linux_amd64.tar.gz");
        assert_eq!(gh_asset_name("macos", "aarch64", "2.60.0"), "gh_2.60.0_macOS_arm64.zip");
        assert_eq!(gh_asset_name("macos", "x86_64", "2.60.0"), "gh_2.60.0_macOS_amd64.zip");
    }
}
