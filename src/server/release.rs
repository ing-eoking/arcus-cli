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

/// Where gh keeps its login on this computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhConfig {
    /// gh's own config (macOS keychain, or a Linux keyring).
    Normal,
    /// Linux without a keyring: a temporary `GH_CONFIG_DIR` on the tmpfs `$XDG_RUNTIME_DIR`, removed when the MCP server exits.
    Ephemeral(PathBuf),
}

/// Name of the temporary gh config directory under `$XDG_RUNTIME_DIR`.
pub const EPHEMERAL_GH_DIR: &str = "arcus-cli-gh";

/// Chooses where a new gh login goes. Without a keyring on Linux the login never goes to disk:
/// it needs `$XDG_RUNTIME_DIR` (tmpfs), otherwise this is an error.
pub fn select_gh_config(os: &str, keyring: bool, runtime_dir: Option<&Path>) -> Result<GhConfig, String> {
    if os != "linux" || keyring { return Ok(GhConfig::Normal); }
    match runtime_dir.filter(|d| d.is_dir()) {
        Some(d) => Ok(GhConfig::Ephemeral(d.join(EPHEMERAL_GH_DIR))),
        None => Err("this computer has no keyring (Secret Service) and XDG_RUNTIME_DIR is not set, so the GitHub login \
            could only be stored on disk, which arcus-cli does not allow. Ask the user to run the MCP client in a desktop session \
            with a keyring, or in a login session that provides XDG_RUNTIME_DIR, or to set GITHUB_TOKEN in the MCP server env.".into()),
    }
}

/// `select_gh_config` for this process (Linux keyring detected through the Secret Service).
pub fn current_gh_config() -> Result<GhConfig, String> {
    let os = std::env::consts::OS;
    let keyring = os == "linux" && secret_service_available();
    let rt = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()).map(PathBuf::from);
    select_gh_config(os, keyring, rt.as_deref())
}

/// Whether `org.freedesktop.secrets` has an owner on the session bus (a mere D-Bus session address is not enough:
/// pam_systemd sets it over SSH).
pub fn secret_service_available() -> bool {
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none_or(|v| v.is_empty()) { return false; }
    let run = |cmd: &str, args: &[&str]| Command::new(cmd).args(args)
        .stdin(Stdio::null()).stderr(Stdio::null()).output().ok();
    if let Some(o) = run("dbus-send", &["--session", "--print-reply", "--dest=org.freedesktop.DBus", "/org/freedesktop/DBus",
        "org.freedesktop.DBus.NameHasOwner", "string:org.freedesktop.secrets"]) {
        return o.status.success() && String::from_utf8_lossy(&o.stdout).contains("boolean true");
    }
    run("busctl", &["--user", "status", "org.freedesktop.secrets"]).is_some_and(|o| o.status.success())
}

/// The temporary gh config directory when it exists (a login made without a keyring).
pub fn ephemeral_gh_dir() -> Option<PathBuf> {
    let rt = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty())?;
    let d = PathBuf::from(rt).join(EPHEMERAL_GH_DIR);
    d.is_dir().then_some(d)
}

/// Creates the temporary gh config directory with mode 700.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// Ends the temporary login when the MCP server exits: `gh auth logout` in that config dir, then removes the dir.
/// No-op without one.
pub fn end_ephemeral_login(gh: Option<&Path>) {
    if let Some(dir) = ephemeral_gh_dir() { end_login_in(&dir, gh); }
}

fn end_login_in(dir: &Path, gh: Option<&Path>) {
    if let Some(gh) = gh {
        let _ = Command::new(gh).args(["auth", "logout", "--hostname", "github.com"])
            .env("GH_CONFIG_DIR", dir).env_remove("GH_TOKEN").env_remove("GITHUB_TOKEN")
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// The token `<gh> auth token` prints (read-only); `config_dir` is a `GH_CONFIG_DIR` to read instead of gh's default.
pub fn gh_token(gh: &Path, config_dir: Option<&Path>) -> Option<Token> {
    /* gh prefers GH_TOKEN/GITHUB_TOKEN over its own login; drop them to read the login itself */
    let mut cmd = Command::new(gh);
    cmd.args(["auth", "token"]).env_remove("GH_TOKEN").env_remove("GITHUB_TOKEN");
    if let Some(d) = config_dir { cmd.env("GH_CONFIG_DIR", d); }
    let out = cmd.stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    if !out.status.success() { return None; }
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if t.is_empty() { None } else { Some(Token(t)) }
}

fn env_token() -> Option<Token> {
    std::env::var("GITHUB_TOKEN").ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).map(Token)
}

/// Candidate tokens in preference order: `GITHUB_TOKEN`, then the gh login (when it differs).
pub fn candidate_tokens(env: Option<Token>, gh: Option<Token>) -> Vec<Token> {
    let mut v: Vec<Token> = env.into_iter().collect();
    if let Some(g) = gh && !v.iter().any(|t| t.0 == g.0) { v.push(g); }
    v
}

/// All usable tokens: `GITHUB_TOKEN`, then `<gh> auth token` (from the temporary gh config dir when it exists).
pub fn find_tokens(gh: Option<&Path>) -> Vec<Token> {
    let dir = ephemeral_gh_dir();
    candidate_tokens(env_token(), gh.and_then(|g| gh_token(g, dir.as_deref())))
}

/// Error when no token exists for the EE edition.
pub const NO_TOKEN: &str = "no GitHub token found on this computer for the EE edition; call arcus_ee_login (without `machine`), \
    show the code and URL to the user, then retry";

/// The EE tarball of `version` already in `downloads/` (written only after its checksum was verified).
fn cached_ee(home: &Home, version: &str) -> Option<(Release, PathBuf)> {
    semver(version)?;
    let asset_name = format!("arcus-memcached-EE-{version}.tar.gz");
    let path = home.downloads().join(&asset_name);
    path.is_file().then(|| (Release { version: version.to_string(), asset_name, asset_api_url: String::new(), sha256: None }, path))
}

/// Resolves `version` (default: the newest) and downloads its source tarball into `downloads/` (cached).
/// For EE the token is looked up here and never leaves this function; a given version that is already
/// cached needs no token at all.
pub fn fetch_source(home: &Home, edition: Edition, version: Option<&str>) -> Result<(Release, PathBuf), String> {
    let pick = |releases: Vec<Release>| -> Result<Release, String> {
        match version {
            Some(v) => releases.into_iter().find(|r| r.version == v).ok_or_else(|| format!("version {v} not found; see arcus_list_versions")),
            None => releases.into_iter().next().ok_or_else(|| "no releases found".to_string()),
        }
    };
    if edition == Edition::Public {
        let rel = pick(list_releases(edition, None).map_err(|e| format!("failed to list releases: {e}"))?)?;
        let path = download_source(&rel, None, home).map_err(|e| format!("download failed: {e}"))?;
        return Ok((rel, path));
    }
    if let Some(hit) = version.and_then(|v| cached_ee(home, v)) { return Ok(hit); }
    let tokens = find_tokens(find_gh(home).as_deref());
    if tokens.is_empty() { return Err(NO_TOKEN.into()); }
    let (releases, token) = list_ee_releases(&tokens).map_err(|e| ee_failure("failed to list EE releases", &e))?;
    let rel = pick(releases)?;
    let path = download_source(&rel, Some(&token), home).map_err(|e| ee_failure("download failed", &e))?;
    Ok((rel, path))
}

/// Runs `f` with each token in turn. A token GitHub rejects (401/403/404) falls through to the next one,
/// so a stale `GITHUB_TOKEN` does not hide a working gh login. Returns the result and the token that worked.
pub fn with_tokens<T>(tokens: &[Token], mut f: impl FnMut(&Token) -> io::Result<T>) -> io::Result<(T, Token)> {
    let mut last = io::Error::new(io::ErrorKind::PermissionDenied, "no GitHub token available");
    for t in tokens {
        match f(t) {
            Ok(v) => return Ok((v, t.clone())),
            Err(e) if matches!(e.kind(), io::ErrorKind::PermissionDenied | io::ErrorKind::NotFound) => last = e,
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

/// EE releases using the first token that GitHub accepts; returns that token too.
pub fn list_ee_releases(tokens: &[Token]) -> io::Result<(Vec<Release>, Token)> {
    with_tokens(tokens, |t| list_releases(Edition::Ee, Some(t)))
}

/// Message for a failed EE call: the shared access guidance for auth errors, otherwise `<what>: <error>`.
pub fn ee_failure(what: &str, e: &io::Error) -> String {
    if matches!(e.kind(), io::ErrorKind::PermissionDenied | io::ErrorKind::NotFound) { ee_access_error(e) } else { format!("{what}: {e}") }
}

/// Shared explanation for "GitHub refused access to the EE repository", used by every EE tool.
pub fn ee_access_error(e: &io::Error) -> String {
    format!("GitHub denied access to the EE repository {EE_REPO} ({e}). Possible causes: \
        (1) the GitHub account has no access to {EE_REPO} (ask the user to check their membership); \
        (2) a stale GITHUB_TOKEN is set and the gh login was not accepted either; \
        (3) the organization restricts OAuth apps, so the gh login token is rejected. \
        For (2) and (3) ask the user to create a personal access token with read access to the repository \
        and set it as GITHUB_TOKEN in the env of this MCP server in the MCP client configuration, then restart the client. \
        Do not ask the user to paste a token into the conversation. If the user was never logged in, call arcus_ee_login.")
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
        let st = Command::new("unzip").arg("-q").arg("-o").arg(archive).arg("-d").arg(&tmp)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status()?;
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
/// Params: `{gh_config_dir?}`: a temporary `GH_CONFIG_DIR` (no keyring) for the login.
pub fn ee_login_job(ctx: &JobCtx, params: &Value) -> Result<Value, String> {
    let config_dir = params["gh_config_dir"].as_str().map(PathBuf::from);
    let gh = match find_gh(&ctx.home) {
        Some(gh) => gh,
        None => {
            ctx.step("download gh");
            download_gh(&ctx.home).map_err(|e| format!("GitHub CLI (gh) is not installed and downloading it failed: {e}"))?
        }
    };
    ctx.step("starting gh");
    let log = std::fs::OpenOptions::new().create(true).append(true).open(&ctx.log).map_err(|e| e.to_string())?;
    let mut cmd = Command::new(&gh);
    if let Some(d) = &config_dir { cmd.env("GH_CONFIG_DIR", d); }
    let mut child = cmd
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
    // Check the login token itself, not GITHUB_TOKEN, which may be stale.
    let token = gh_token(&gh, config_dir.as_deref()).ok_or("login finished but no token is available")?;
    let versions = list_releases(Edition::Ee, Some(&token)).map_err(|e| ee_access_error(&e))?;
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

    fn tok(s: &str) -> Token { Token(s.to_string()) }

    #[test]
    fn candidates_prefer_env_and_dedupe() {
        let v = candidate_tokens(Some(tok("a")), Some(tok("b")));
        assert_eq!(v.iter().map(|t| t.0.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(candidate_tokens(Some(tok("a")), Some(tok("a"))).len(), 1);
        assert_eq!(candidate_tokens(None, Some(tok("b"))).len(), 1);
        assert!(candidate_tokens(None, None).is_empty());
    }

    #[test]
    fn rejected_token_falls_back_to_next() {
        let denied = || io::Error::new(io::ErrorKind::PermissionDenied, "HTTP 401");
        let mut seen = Vec::new();
        let (v, used) = with_tokens(&[tok("stale"), tok("good")], |t| {
            seen.push(t.0.clone());
            if t.0 == "stale" { Err(denied()) } else { Ok(7) }
        }).unwrap();
        assert_eq!((v, used.0.as_str()), (7, "good"));
        assert_eq!(seen, ["stale", "good"]);
        // all rejected -> last error; non-auth errors are not retried
        let e = with_tokens(&[tok("a"), tok("b")], |_| -> io::Result<()> { Err(denied()) }).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        let mut n = 0;
        let e = with_tokens(&[tok("a"), tok("b")], |_| -> io::Result<()> { n += 1; Err(io::Error::other("net")) }).unwrap_err();
        assert_eq!((n, e.kind()), (1, io::ErrorKind::Other));
        assert!(with_tokens::<()>(&[], |_| Ok(())).is_err());
    }

    #[test]
    fn ee_access_error_guides_without_secrets() {
        let m = ee_access_error(&io::Error::new(io::ErrorKind::PermissionDenied, "GET https://x failed: HTTP 403"));
        assert!(m.contains("GITHUB_TOKEN") && m.contains("OAuth") && m.contains("HTTP 403") && m.contains("arcus_ee_login"), "{m}");
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
    fn gh_config_selection() {
        let rt = std::env::temp_dir().join(format!("arcus-rt-{}", std::process::id()));
        std::fs::create_dir_all(&rt).unwrap();
        // macOS and Linux with a keyring: gh's normal config
        assert_eq!(select_gh_config("macos", false, None), Ok(GhConfig::Normal));
        assert_eq!(select_gh_config("linux", true, None), Ok(GhConfig::Normal));
        // no keyring + XDG_RUNTIME_DIR: the tmpfs dir
        assert_eq!(select_gh_config("linux", false, Some(&rt)), Ok(GhConfig::Ephemeral(rt.join(EPHEMERAL_GH_DIR))));
        // neither: an error, never a disk fallback
        let e = select_gh_config("linux", false, None).unwrap_err();
        assert!(e.contains("keyring") && e.contains("XDG_RUNTIME_DIR"), "{e}");
        assert!(select_gh_config("linux", false, Some(&rt.join("missing"))).is_err());
        let d = rt.join("priv");
        create_private_dir(&d).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&d).unwrap().permissions().mode() & 0o777, 0o700);
        let _ = std::fs::remove_dir_all(&rt);
    }

    #[test]
    fn cached_ee_version_needs_no_token() {
        let root = std::env::temp_dir().join(format!("arcus-cache-ee-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = Home::new(Some(root.clone())).unwrap();
        std::fs::create_dir_all(home.downloads()).unwrap();
        std::fs::write(home.downloads().join("arcus-memcached-EE-0.12.1.tar.gz"), b"t").unwrap();
        // returns before any token lookup or GitHub call
        let (rel, path) = fetch_source(&home, Edition::Ee, Some("0.12.1")).unwrap();
        assert_eq!((rel.version.as_str(), rel.asset_name.as_str()), ("0.12.1", "arcus-memcached-EE-0.12.1.tar.gz"));
        assert_eq!(path, home.downloads().join("arcus-memcached-EE-0.12.1.tar.gz"));
        assert!(cached_ee(&home, "0.12.2").is_none());
        assert!(cached_ee(&home, "../x").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ending_a_login_removes_the_dir() {
        let d = std::env::temp_dir().join(format!("arcus-endlogin-{}", std::process::id()));
        create_private_dir(&d).unwrap();
        std::fs::write(d.join("hosts.yml"), "x").unwrap();
        end_login_in(&d, None);
        assert!(!d.exists());
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
