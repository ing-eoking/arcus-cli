//! Local ZooKeeper ensemble (one or more nodes) for development clusters (download, start, stop).

use super::build::detect_os;
use super::build::Os;
use super::fetch::{fetch_cached, get_json, Checksum, Request};
use super::home::Home;
use super::jobs::JobCtx;
use serde::{Deserialize, Serialize};
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

/// One member of the local ensemble (every port is on 127.0.0.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node { pub id: u32, pub client_port: u16, pub peer_port: u16, pub election_port: u16 }

/// Description of the local ensemble, stored as `zk/ensemble.json` while it runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ensemble { pub version: String, pub nodes: Vec<Node> }

impl Ensemble {
    pub fn connect_string(&self) -> String {
        self.nodes.iter().map(|n| format!("127.0.0.1:{}", n.client_port)).collect::<Vec<_>>().join(",")
    }
}

/// `zoo.cfg` of `node`. A single-node ensemble is standalone (no `server.N` lines).
pub fn zoo_cfg(node: &Node, nodes: &[Node], data_dir: &Path) -> String {
    let mut c = format!("tickTime=2000\ninitLimit=10\nsyncLimit=5\ndataDir={}\nclientPort={}\nadmin.enableServer=false\n4lw.commands.whitelist=ruok,srvr\n",
        data_dir.display(), node.client_port);
    if nodes.len() > 1 {
        for n in nodes { c.push_str(&format!("server.{}=127.0.0.1:{}:{}\n", n.id, n.peer_port, n.election_port)); }
    }
    c
}

fn port_list(args: &Value, key: &str, count: usize, base_key: &str, base_default: u64) -> Result<Vec<u16>, String> {
    let to_port = |p: u64| u16::try_from(p).ok().filter(|p| *p != 0).ok_or_else(|| format!("invalid port {p} (use 1..=65535)"));
    match args.get(key).filter(|v| !v.is_null()) {
        Some(v) => {
            let list = v.as_array().ok_or_else(|| format!("`{key}` must be an array of ports"))?;
            if list.len() != count { return Err(format!("`{key}` has {} entries but count is {count}", list.len())); }
            list.iter().map(|p| to_port(p.as_u64().ok_or_else(|| format!("`{key}` must contain only integer ports"))?)).collect()
        }
        None => {
            let base = match args.get(base_key).filter(|v| !v.is_null()) {
                Some(b) => b.as_u64().ok_or_else(|| format!("`{base_key}` must be an integer"))?,
                None => base_default,
            };
            (0..count as u64).map(|i| to_port(base + i)).collect()
        }
    }
}

/// Nodes from the `arcus_zk_start` arguments: `count` (default 3), `client_ports`/`peer_ports`/`election_ports`
/// (else derived from `base_*_port` + index, default 2181/2888/3888).
pub fn nodes_from_args(args: &Value) -> Result<Vec<Node>, String> {
    let count = match args.get("count").filter(|v| !v.is_null()) {
        None => 3,
        Some(c) => c.as_u64().filter(|c| (1..=7).contains(c)).ok_or("`count` must be an integer from 1 to 7")? as usize,
    };
    let client = port_list(args, "client_ports", count, "base_client_port", 2181)?;
    let peer = port_list(args, "peer_ports", count, "base_peer_port", 2888)?;
    let election = port_list(args, "election_ports", count, "base_election_port", 3888)?;
    let mut seen = std::collections::BTreeSet::new();
    for p in client.iter().chain(&peer).chain(&election) {
        if !seen.insert(*p) { return Err(format!("port {p} is used more than once; all client, peer and election ports must be distinct")); }
    }
    Ok((0..count).map(|i| Node { id: i as u32 + 1, client_port: client[i], peer_port: peer[i], election_port: election[i] }).collect())
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
        .is_some_and(|m| m >= 8)
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

fn ensemble_file(home: &Home) -> PathBuf { home.zk_dir().join("ensemble.json") }
fn node_dir(home: &Home, id: u32) -> PathBuf { home.zk_dir().join(format!("node{id}")) }
fn pid_file(home: &Home, id: u32) -> PathBuf { node_dir(home, id).join("zookeeper.pid") }
/// Pid file of the old single-node layout (before ensembles).
fn legacy_pid_file(home: &Home) -> PathBuf { home.zk_dir().join("zookeeper.pid") }

pub fn read_ensemble(home: &Home) -> Option<Ensemble> {
    serde_json::from_str(&std::fs::read_to_string(ensemble_file(home)).ok()?).ok()
}

pub fn write_ensemble(home: &Home, e: &Ensemble) -> io::Result<()> {
    std::fs::create_dir_all(home.zk_dir())?;
    std::fs::write(ensemble_file(home), serde_json::to_string_pretty(e).map_err(io::Error::other)?)
}

/// Client connect string of the local ensemble described by `zk/ensemble.json`.
pub fn local_ensemble(home: &Home) -> Option<String> { read_ensemble(home).map(|e| e.connect_string()) }

fn live_pid_in(file: &Path) -> Option<u32> {
    let pid: u32 = std::fs::read_to_string(file).ok()?.trim().parse().ok()?;
    let out = Command::new("ps").args(["-p", &pid.to_string(), "-o", "comm="]).stdin(Stdio::null()).output().ok()?;
    (out.status.success() && String::from_utf8_lossy(&out.stdout).contains("java")).then_some(pid)
}

fn four_letter(port: u16, word: &[u8]) -> Option<String> {
    let addr = ("127.0.0.1", port).to_socket_addrs().ok().and_then(|mut a| a.next())?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(500)).ok()?;
    let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
    s.write_all(word).ok()?;
    let mut buf = String::new();
    let _ = s.read_to_string(&mut buf);
    Some(buf)
}

pub fn ruok(port: u16) -> bool { four_letter(port, b"ruok").is_some_and(|b| b.trim() == "imok") }

/// True once the node serves requests. `ruok` already answers `imok` while a quorum is still forming,
/// so readiness is judged from `srvr`, which only reports a `Mode:` for a serving node.
fn serving(port: u16) -> bool { four_letter(port, b"srvr").is_some_and(|b| b.contains("Mode:")) }

fn ensure_client_port_free(port: u16) -> io::Result<()> {
    if ruok(port) {
        return Err(io::Error::new(io::ErrorKind::AddrInUse, format!(
            "client port {port} is already served by another ZooKeeper; pick a different port, or use that ensemble (127.0.0.1:{port}) directly")));
    }
    Ok(())
}

/// Fails if something already listens on, or cannot be bound at, `port` on 127.0.0.1.
fn ensure_bindable(port: u16) -> io::Result<()> {
    let busy = |why: &str| io::Error::new(io::ErrorKind::AddrInUse, format!("port {port} is already in use ({why}); pick a different port"));
    if TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_millis(500)).is_ok() {
        return Err(busy("something is listening"));
    }
    std::net::TcpListener::bind(("127.0.0.1", port)).map(drop).map_err(|e| busy(&e.to_string()))
}

pub struct Started { pub ensemble: Ensemble, pub pids: Vec<Option<u32>>, pub already_running: bool }

fn kill_pid(pid: u32) { let _ = Command::new("kill").args(["-TERM", &pid.to_string()]).stdin(Stdio::null()).status(); }

fn wait_exit(pid: u32, secs: u64) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        let alive = Command::new("ps").args(["-p", &pid.to_string(), "-o", "comm="]).stdin(Stdio::null()).output()
            .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("java")).unwrap_or(false);
        if !alive { return; }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Starts the ensemble `nodes` (one detached java process per node), or returns the one already running
/// (`already_running`; the caller compares it with the request). Nodes started here are stopped again on failure.
pub fn start(home: &Home, nodes: &[Node], version: &str) -> io::Result<Started> {
    if live_pid_in(&legacy_pid_file(home)).is_some() {
        return Err(io::Error::other("a ZooKeeper from an older arcus-cli layout is still running; call arcus_zk_stop first"));
    }
    if let Some(e) = read_ensemble(home) {
        let pids: Vec<Option<u32>> = e.nodes.iter().map(|n| live_pid_in(&pid_file(home, n.id))).collect();
        if pids.iter().any(Option::is_some) { return Ok(Started { ensemble: e, pids, already_running: true }); }
    }
    if !valid_version(version) { return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("invalid ZooKeeper version `{version}`"))); }
    for n in nodes { ensure_client_port_free(n.client_port)?; }
    for n in nodes { for p in [n.client_port, n.peer_port, n.election_port] { ensure_bindable(p)?; } }
    let dist = home.zookeeper_dist(version);
    if !dist.join("lib").is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("ZooKeeper {version} is not downloaded")));
    }
    let java = find_java(home).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no Java 8+ found"))?;
    let ensemble = Ensemble { version: version.to_string(), nodes: nodes.to_vec() };
    let mut children: Vec<(u32, std::process::Child)> = Vec::new();
    let result = (|| -> io::Result<()> {
        for n in nodes {
            let dir = node_dir(home, n.id);
            let data = dir.join("data");
            std::fs::create_dir_all(&data)?;
            if nodes.len() > 1 { std::fs::write(data.join("myid"), n.id.to_string())?; }
            let cfg = dir.join("zoo.cfg");
            std::fs::write(&cfg, zoo_cfg(n, nodes, &data))?;
            let log = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("zookeeper.log"))?;
            let child = Command::new(&java)
                .arg("-cp").arg(format!("{}/lib/*", dist.display()))
                .arg("org.apache.zookeeper.server.quorum.QuorumPeerMain").arg(&cfg)
                .process_group(0).stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log).spawn()?;
            std::fs::write(pid_file(home, n.id), child.id().to_string())?;
            children.push((n.id, child));
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            for (id, child) in children.iter_mut() {
                if let Ok(Some(st)) = child.try_wait() {
                    let logp = node_dir(home, *id).join("zookeeper.log");
                    return Err(io::Error::other(format!("ZooKeeper node {id} exited ({st}); see {}\n{}",
                        logp.display(), super::jobs::tail(&logp, 20).unwrap_or_default())));
                }
            }
            if nodes.iter().all(|n| serving(n.client_port)) { return Ok(()); }
            if Instant::now() >= deadline {
                let waiting: Vec<String> = nodes.iter().filter(|n| !serving(n.client_port)).map(|n| n.id.to_string()).collect();
                return Err(io::Error::other(format!("ZooKeeper node(s) {} was not serving within 20 s; see {}/node<id>/zookeeper.log",
                    waiting.join(","), home.zk_dir().display())));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    })();
    match result {
        Ok(()) => {
            let pids = children.iter().map(|(_, c)| Some(c.id())).collect();
            for (_, mut c) in children { std::thread::spawn(move || { let _ = c.wait(); }); }
            write_ensemble(home, &ensemble)?;
            Ok(Started { ensemble, pids, already_running: false })
        }
        Err(e) => {
            for (id, mut c) in children {
                kill_pid(c.id());
                let _ = c.wait();
                let _ = std::fs::remove_file(pid_file(home, id));
            }
            let _ = std::fs::remove_file(ensemble_file(home));
            Err(e)
        }
    }
}

/// Stops every node of the local ensemble (and a leftover single-node ZooKeeper of the old layout).
/// Pid files and `ensemble.json` are removed; data stays. Returns the number of processes stopped.
pub fn stop(home: &Home) -> io::Result<usize> {
    let mut files: Vec<PathBuf> = read_ensemble(home).map(|e| e.nodes.iter().map(|n| pid_file(home, n.id)).collect()).unwrap_or_default();
    files.push(legacy_pid_file(home));
    let mut seen_any = ensemble_file(home).exists();
    let mut stopped = 0;
    for f in &files {
        if let Some(pid) = live_pid_in(f) { kill_pid(pid); wait_exit(pid, 5); stopped += 1; seen_any = true; }
        match std::fs::remove_file(f) {
            Ok(()) => seen_any = true,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    let _ = std::fs::remove_file(ensemble_file(home));
    if seen_any { Ok(stopped) } else { Err(io::Error::new(io::ErrorKind::NotFound, "local ZooKeeper is not running")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nodes3() -> Vec<Node> {
        (1..=3).map(|i| Node { id: i, client_port: 22180 + i as u16, peer_port: 22887 + i as u16, election_port: 23887 + i as u16 }).collect()
    }
    #[test] fn cfg_three_nodes() {
        let n = nodes3();
        let c = zoo_cfg(&n[1], &n, Path::new("/d/node2/data"));
        assert!(c.contains("dataDir=/d/node2/data\n") && c.contains("clientPort=22182\n"));
        for (i, want) in ["server.1=127.0.0.1:22888:23888\n", "server.2=127.0.0.1:22889:23889\n", "server.3=127.0.0.1:22890:23890\n"].iter().enumerate() {
            assert!(c.contains(want), "server.{} missing in {c}", i + 1);
        }
        for k in ["tickTime=2000\n", "initLimit=10\n", "syncLimit=5\n", "admin.enableServer=false\n", "4lw.commands.whitelist=ruok,srvr\n"] { assert!(c.contains(k), "{k}"); }
    }
    #[test] fn cfg_single_node_is_standalone() {
        let n = &nodes3()[..1];
        let c = zoo_cfg(&n[0], n, Path::new("/d"));
        assert!(!c.contains("server.") && c.contains("clientPort=22181\n") && c.contains("dataDir=/d\n"));
    }
    fn a(v: Value) -> Value { v }
    #[test] fn ports_default_to_recommended_consecutive() {
        let n = nodes_from_args(&a(json!({}))).unwrap();
        assert_eq!(n.len(), 3);
        assert_eq!((n[2].id, n[2].client_port, n[2].peer_port, n[2].election_port), (3, 2183, 2890, 3890));
    }
    #[test] fn ports_derive_from_bases_and_explicit_lists() {
        let n = nodes_from_args(&a(json!({"count": 2, "base_client_port": 22181, "base_peer_port": 22888, "base_election_port": 23888}))).unwrap();
        assert_eq!((n[1].client_port, n[1].peer_port, n[1].election_port), (22182, 22889, 23889));
        let n = nodes_from_args(&a(json!({"count": 2, "client_ports": [30001, 30005], "peer_ports": [30002, 30006], "election_ports": [30003, 30007]}))).unwrap();
        assert_eq!((n[1].client_port, n[1].peer_port, n[1].election_port), (30005, 30006, 30007));
    }
    #[test] fn ports_validation() {
        let err = |v: Value| nodes_from_args(&v).unwrap_err();
        assert!(err(json!({"count": 0})).contains("count"));
        assert!(err(json!({"count": 8})).contains("count"));
        assert!(err(json!({"count": 2, "client_ports": [30001]})).contains("client_ports"));
        assert!(err(json!({"count": 2, "client_ports": [30001, 30001], "peer_ports": [30002, 30003], "election_ports": [30004, 30005]})).contains("30001"));
        assert!(err(json!({"count": 1, "client_ports": [30001], "peer_ports": [30001], "election_ports": [30004]})).contains("30001"));
        assert!(err(json!({"count": 1, "client_ports": [0]})).contains("port"));
        assert!(err(json!({"count": 1, "client_ports": [70000]})).contains("port"));
        assert!(err(json!({"count": 3, "base_client_port": 65535})).contains("port"));
        assert!(err(json!({"count": 1, "client_ports": ["x"]})).contains("client_ports"));
    }
    #[test] fn ensemble_string_and_file_roundtrip() {
        let e = Ensemble { version: "3.8.6".into(), nodes: nodes3() };
        assert_eq!(e.connect_string(), "127.0.0.1:22181,127.0.0.1:22182,127.0.0.1:22183");
        let d = std::env::temp_dir().join(format!("arcus-zk-ens-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let home = Home::new(Some(d.clone())).unwrap();
        assert!(local_ensemble(&home).is_none());
        write_ensemble(&home, &e).unwrap();
        let back = read_ensemble(&home).unwrap();
        assert_eq!(back.nodes, e.nodes);
        assert_eq!(local_ensemble(&home).unwrap(), e.connect_string());
        let _ = std::fs::remove_dir_all(d);
    }
    #[test] fn in_use_port_is_refused() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(ensure_bindable(port).unwrap_err().to_string().contains(&port.to_string()));
        drop(l);
        // another test may grab the freed ephemeral port; any free port must pass
        let free = (0..5).any(|_| {
            let p = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
            ensure_bindable(p).is_ok()
        });
        assert!(free);
    }
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
    #[test] fn client_port_already_served_is_refused() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            for _ in 0..2 {
                if let Ok((mut s, _)) = l.accept() { let mut b = [0u8; 4]; let _ = s.read_exact(&mut b); let _ = s.write_all(b"imok"); }
            }
        });
        let e = ensure_client_port_free(port).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AddrInUse);
        assert!(e.to_string().contains(&format!("port {port} is already served")));
        drop(t);
    }
    #[test] fn version_validation() { assert!(valid_version("3.8.6")); assert!(!valid_version("../x")); }
    #[test] fn stop_without_ensemble_is_error() {
        let d = std::env::temp_dir().join(format!("arcus-zk-{}", std::process::id()));
        let home = Home::new(Some(d.clone())).unwrap();
        assert!(stop(&home).is_err());
        let _ = std::fs::remove_dir_all(d);
    }
}
