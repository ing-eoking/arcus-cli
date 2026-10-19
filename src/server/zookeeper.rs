//! Local single-node ZooKeeper for development clusters (download, start, stop).

use super::build::detect_os;
use super::build::Os;
use super::fetch::{fetch_cached, get_json, Checksum, Request};
use super::home::Home;
use super::jobs::JobCtx;
use serde_json::{json, Value};
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const DEFAULT_ZK_VERSION: &str = "3.8.6";

pub fn dist_urls(version: &str) -> (String, String) {
    let tar = format!("https://archive.apache.org/dist/zookeeper/zookeeper-{version}/apache-zookeeper-{version}-bin.tar.gz");
    let sum = format!("{tar}.sha512");
    (tar, sum)
}

/// First 128-hex-digit token of an Apache `.sha512` file.
pub fn parse_sha512_file(text: &str) -> Option<String> {
    let t = text.split_whitespace().next()?;
    (t.len() == 128 && t.chars().all(|c| c.is_ascii_hexdigit())).then(|| t.to_ascii_lowercase())
}

pub fn zoo_cfg(port: u16, data_dir: &Path) -> String {
    format!("tickTime=2000\ninitLimit=10\nsyncLimit=5\ndataDir={}\nclientPort={port}\nadmin.enableServer=false\n4lw.commands.whitelist=ruok\n",
        data_dir.display())
}

/// Major Java version from `java -version` output (`1.8.0_x` -> 8).
pub fn java_major(version_output: &str) -> Option<u32> {
    let start = version_output.find('"')? + 1;
    let rest = &version_output[start..];
    let v = &rest[..rest.find('"')?];
    let mut parts = v.split(['.', '_', '-', '+']);
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 { parts.next()?.parse().ok() } else { Some(first) }
}

fn java_ok(java: &Path) -> bool {
    Command::new(java).arg("-version").stdin(Stdio::null()).output().ok()
        .filter(|o| o.status.success())
        .and_then(|o| java_major(&String::from_utf8_lossy(&o.stderr)).or_else(|| java_major(&String::from_utf8_lossy(&o.stdout))))
        .map_or(false, |m| m >= 8)
}

fn jre_java(home: &Home) -> Vec<PathBuf> {
    let jre = home.root.join("zookeeper").join("jre");
    vec![jre.join("bin/java"), jre.join("Contents/Home/bin/java")]
}

pub fn find_java(home: &Home) -> Option<PathBuf> {
    let mut cands = Vec::new();
    if let Some(h) = std::env::var_os("JAVA_HOME").filter(|h| !h.is_empty()) {
        cands.push(PathBuf::from(h).join("bin/java"));
    }
    if let Some(paths) = std::env::var_os("PATH") {
        cands.extend(std::env::split_paths(&paths).map(|d| d.join("java")));
    }
    cands.extend(jre_java(home));
    cands.into_iter().filter(|p| p.is_file()).find(|p| java_ok(p))
}

fn os_arch() -> Result<(&'static str, &'static str), String> {
    let os = match detect_os() { Os::Mac => "mac", Os::Other if !cfg!(target_os = "linux") => return Err("unsupported OS".into()), _ => "linux" };
    let arch = match std::env::consts::ARCH { "x86_64" => "x64", "aarch64" => "aarch64", a => return Err(format!("unsupported architecture {a}")) };
    Ok((os, arch))
}

/// Extracts `tarball` into `<target>.tmp` and moves its single top-level directory to `target`.
fn install_tarball(tarball: &Path, target: &Path) -> io::Result<()> {
    let tmp = PathBuf::from(format!("{}.tmp", target.display()));
    let top = super::build::extract(tarball, &tmp)?;
    let _ = std::fs::remove_dir_all(target);
    std::fs::rename(&top, target)?;
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(())
}

fn download_dist(ctx: &JobCtx, version: &str) -> Result<PathBuf, String> {
    let dist = ctx.home.zookeeper_dist(version);
    if dist.join("lib").is_dir() { return Ok(dist); }
    let (tar_url, sum_url) = dist_urls(version);
    let sum_file = ctx.home.downloads().join(format!("apache-zookeeper-{version}-bin.tar.gz.sha512"));
    let _ = std::fs::remove_file(&sum_file);
    fetch_cached(&Request { url: sum_url, headers: vec![] }, &sum_file, None).map_err(|e| e.to_string())?;
    let sum = parse_sha512_file(&std::fs::read_to_string(&sum_file).map_err(|e| e.to_string())?)
        .ok_or("could not parse the .sha512 file")?;
    let tarball = fetch_cached(&Request { url: tar_url, headers: vec![] },
        &ctx.home.downloads().join(format!("apache-zookeeper-{version}-bin.tar.gz")), Some(&Checksum::Sha512(sum)))
        .map_err(|e| e.to_string())?;
    ctx.step("extract");
    install_tarball(&tarball, &dist).map_err(|e| e.to_string())?;
    Ok(dist)
}

fn download_jre(ctx: &JobCtx) -> Result<(), String> {
    if jre_java(&ctx.home).iter().any(|p| p.is_file()) { return Ok(()); }
    let (os, arch) = os_arch()?;
    let url = format!("https://api.adoptium.net/v3/assets/latest/17/hotspot?os={os}&architecture={arch}&image_type=jre");
    let assets = get_json(&Request { url, headers: vec![] }).map_err(|e| e.to_string())?;
    let pkg = &assets[0]["binary"]["package"];
    let link = pkg["link"].as_str().ok_or("Adoptium returned no JRE package")?.to_string();
    let sum = pkg["checksum"].as_str().ok_or("Adoptium returned no JRE checksum")?.to_string();
    let name = pkg["name"].as_str().unwrap_or("temurin-jre17.tar.gz");
    let tarball = fetch_cached(&Request { url: link, headers: vec![] },
        &ctx.home.downloads().join(name), Some(&Checksum::Sha256(sum))).map_err(|e| e.to_string())?;
    install_tarball(&tarball, &ctx.home.root.join("zookeeper").join("jre")).map_err(|e| e.to_string())
}

/// `ZkDownload` job handler: params `{version, download_jre}`.
pub fn download_job(ctx: &JobCtx, params: &Value) -> Result<Value, String> {
    let version = params["version"].as_str().unwrap_or(DEFAULT_ZK_VERSION).to_string();
    if !valid_version(&version) { return Err(format!("invalid ZooKeeper version `{version}`")); }
    ctx.step("download zookeeper");
    let dist = download_dist(ctx, &version)?;
    if params["download_jre"].as_bool().unwrap_or(false) {
        ctx.step("download jre");
        download_jre(ctx)?;
    }
    Ok(json!({"version": version, "dist": dist.display().to_string()}))
}

pub fn valid_version(v: &str) -> bool {
    !v.is_empty() && v.len() <= 32 && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

fn pid_file(home: &Home) -> PathBuf { home.zk_dir().join("zookeeper.pid") }

fn live_java_pid(home: &Home) -> Option<u32> {
    let pid: u32 = std::fs::read_to_string(pid_file(home)).ok()?.trim().parse().ok()?;
    let out = Command::new("ps").args(["-p", &pid.to_string(), "-o", "comm="]).stdin(Stdio::null()).output().ok()?;
    (out.status.success() && String::from_utf8_lossy(&out.stdout).contains("java")).then_some(pid)
}

pub fn ruok(port: u16) -> bool {
    let Some(addr) = ("127.0.0.1", port).to_socket_addrs().ok().and_then(|mut a| a.next()) else { return false };
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else { return false };
    let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
    if s.write_all(b"ruok").is_err() { return false; }
    let mut buf = String::new();
    let _ = s.read_to_string(&mut buf);
    buf.trim() == "imok"
}

pub fn start(home: &Home, port: u16, version: &str) -> io::Result<u32> {
    if let Some(pid) = live_java_pid(home) { return Ok(pid); }
    if !valid_version(version) { return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("invalid ZooKeeper version `{version}`"))); }
    let dist = home.zookeeper_dist(version);
    if !dist.join("lib").is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("ZooKeeper {version} is not downloaded")));
    }
    let java = find_java(home).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no Java 8+ found"))?;
    let zk = home.zk_dir();
    let data = zk.join("data");
    std::fs::create_dir_all(&data)?;
    let cfg = zk.join("zoo.cfg");
    std::fs::write(&cfg, zoo_cfg(port, &data))?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(zk.join("zookeeper.log"))?;
    let mut child = Command::new(java)
        .arg("-cp").arg(format!("{}/lib/*", dist.display()))
        .arg("org.apache.zookeeper.server.quorum.QuorumPeerMain").arg(&cfg)
        .process_group(0).stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log).spawn()?;
    let pid = child.id();
    std::fs::write(pid_file(home), pid.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if ruok(port) {
            std::thread::spawn(move || { let _ = child.wait(); });
            return Ok(pid);
        }
        if let Ok(Some(st)) = child.try_wait() {
            let _ = std::fs::remove_file(pid_file(home));
            return Err(io::Error::other(format!("ZooKeeper exited ({st}); see {}", zk.join("zookeeper.log").display())));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err(io::Error::other(format!("ZooKeeper did not answer ruok within 10 s (pid {pid}); see {}", zk.join("zookeeper.log").display())))
}

pub fn stop(home: &Home) -> io::Result<()> {
    match live_java_pid(home) {
        Some(pid) => {
            Command::new("kill").args(["-TERM", &pid.to_string()]).stdin(Stdio::null()).status()?;
            std::fs::remove_file(pid_file(home))
        }
        None => match std::fs::remove_file(pid_file(home)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound =>
                Err(io::Error::new(io::ErrorKind::NotFound, "local ZooKeeper is not running")),
            Err(e) => Err(e),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] fn cfg() { let c = zoo_cfg(2181, Path::new("/d")); assert!(c.contains("clientPort=2181\n") && c.contains("dataDir=/d\n") && c.contains("4lw.commands.whitelist=ruok\n")); }
    #[test] fn java_versions() {
        assert_eq!(java_major("openjdk version \"1.8.0_392\""), Some(8));
        assert_eq!(java_major("openjdk version \"17.0.9\" 2023-10-17"), Some(17));
        assert_eq!(java_major("openjdk version \"26\" 2027-03-17"), Some(26));
        assert_eq!(java_major("garbage"), None);
    }
    #[test] fn sha512_parse() {
        let h = "a".repeat(128);
        assert_eq!(parse_sha512_file(&format!("{h}  apache-zookeeper-3.8.4-bin.tar.gz")), Some(h));
        assert_eq!(parse_sha512_file("short  x"), None);
    }
    #[test] fn urls() {
        let (t, s) = dist_urls("3.8.6");
        assert_eq!(t, "https://archive.apache.org/dist/zookeeper/zookeeper-3.8.6/apache-zookeeper-3.8.6-bin.tar.gz");
        assert_eq!(s, format!("{t}.sha512"));
    }
    #[test] fn version_validation() { assert!(valid_version("3.8.6")); assert!(!valid_version("../x")); }
    #[test] fn stop_without_pid_is_error() {
        let d = std::env::temp_dir().join(format!("arcus-zk-{}", std::process::id()));
        let home = Home::new(Some(d.clone())).unwrap();
        assert!(stop(&home).is_err());
        let _ = std::fs::remove_dir_all(d);
    }
}
